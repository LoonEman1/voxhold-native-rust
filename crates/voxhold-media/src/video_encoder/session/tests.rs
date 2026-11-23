//! Contract tests of the hardware session queue (G07).
//!
//! They use the deterministic double: the real Windows session is covered by
//! the ignored `hardware_` tests that need a driver.

use super::fake::FakeGpuEncoder;
use super::{EncodeSubmit, GpuEncoder, Nv12Frame};

fn frame(timestamp_us: i64) -> Nv12Frame {
    Nv12Frame {
        width: 4,
        height: 2,
        data: vec![0x10; 4 * 2 + 2 * 2],
        timestamp_us,
    }
}

#[test]
fn the_input_queue_is_bounded_and_drops_before_the_encoder() {
    let mut encoder = FakeGpuEncoder::new(2);
    assert_eq!(encoder.submit(frame(1)), EncodeSubmit::Queued);
    assert_eq!(encoder.submit(frame(2)), EncodeSubmit::Queued);
    assert_eq!(encoder.submit(frame(3)), EncodeSubmit::Dropped);
    let counters = encoder.counters();
    assert_eq!(counters.dropped_input, 1, "{counters:?}");
    // Nothing was encoded yet: a dropped input cannot break a reference chain.
    assert_eq!(counters.encoded, 0);
    let first = encoder.next_packet().expect("the first queued frame");
    assert!(first.keyframe);
}

/// A PLI/FIR must produce a new keyframe: the WebRTC receiver cannot decode a
/// delta frame after a lost one.
#[test]
fn a_keyframe_request_produces_a_new_keyframe() {
    let mut encoder = FakeGpuEncoder::new(4);
    encoder.submit(frame(1));
    assert!(encoder.next_packet().expect("keyframe").keyframe);

    encoder.submit(frame(2));
    let delta = encoder.next_packet().expect("delta frame");
    assert!(!delta.keyframe);
    assert_eq!(delta.timestamp_us, 2);

    encoder.request_keyframe();
    encoder.submit(frame(3));
    let key = encoder.next_packet().expect("recovered keyframe");
    assert!(key.keyframe);
    assert_eq!(encoder.counters().keyframe_requests, 1);
}

/// The output queue is bounded too. A packet the engine never receives is a
/// lost reference frame, so the session asks for a keyframe instead of
/// pretending the stream is still decodable.
#[test]
fn a_dropped_encoded_packet_requests_a_keyframe() {
    let mut encoder = FakeGpuEncoder::new(1);
    for timestamp in 0..9 {
        encoder.submit(frame(timestamp));
        encoder.pump();
    }
    assert_eq!(encoder.counters().dropped_output, 1);
    // Drain the packets produced before the drop; the next encoded frame must
    // be a keyframe because the receiver missed one reference frame.
    while encoder.next_packet().is_some() {}
    encoder.submit(frame(20));
    let packet = encoder.next_packet().expect("recovered packet");
    assert!(packet.keyframe, "a lost encoded frame needs a new keyframe");
}

#[test]
fn the_rate_control_target_reaches_the_encoder() {
    let mut encoder = FakeGpuEncoder::new(1);
    encoder.set_bitrate(1_200_000);
    let counters = encoder.counters();
    assert_eq!(counters.bitrate_bps, 1_200_000);
    assert!(counters.bitrate_applied);
}

#[test]
fn shutdown_closes_the_session_and_its_queue() {
    let mut encoder = FakeGpuEncoder::new(2);
    encoder.submit(frame(1));
    encoder.shutdown();
    assert_eq!(encoder.submit(frame(2)), EncodeSubmit::Closed);
    assert!(encoder.next_packet().is_none(), "the queue was released");
}

/// The vendor attribute of an open MFT is reported as `"VEN_10DE"`. Parsing is
/// strict: a foreign or malformed value must not be read as "no mismatch".
#[test]
fn the_hardware_vendor_attribute_is_parsed_strictly() {
    assert_eq!(super::hardware_vendor("VEN_10DE"), Some(0x10de));
    assert_eq!(super::hardware_vendor("VEN_10de\0"), Some(0x10de));
    assert_eq!(super::hardware_vendor("VEN_1002"), Some(0x1002));
    assert_eq!(super::hardware_vendor("ven_10de"), None);
    assert_eq!(super::hardware_vendor("10DE"), None);
    assert_eq!(super::hardware_vendor("VEN_"), None);
    assert_eq!(super::hardware_vendor("VEN_0000"), None);
    assert_eq!(super::hardware_vendor("VEN_ZZZZ"), None);
}

/// A driver failure is reported once, with the device-loss flag the publisher
/// uses for its one explicit message.
#[test]
fn a_terminal_failure_is_reported_with_its_kind() {
    let mut encoder = FakeGpuEncoder::failing("устройство отключено", true);
    assert_eq!(encoder.submit(frame(1)), EncodeSubmit::Closed);
    let failure = encoder.failure().expect("failure");
    assert!(failure.device_lost);
    assert!(failure.message.contains("отключено"));
    // The failure is not reported as a fresh session.
    assert!(encoder.failure().is_some());
}
