//! GPU attempt decisions: identity confirmation, device loss and the explicit
//! CPU continuation (G04–G06). No driver and no WebRTC run here.

use super::*;
use crate::video_encoder::{
    AdapterInfo, AdapterLuid, EncodeQuery, EncoderBackend, EncoderPlan, HardwareEncoderIdentity,
    codec_profile,
};

fn adapter(luid: u64) -> AdapterInfo {
    AdapterInfo {
        luid: AdapterLuid::new(luid as u32, (luid >> 32) as u32),
        name: "NVIDIA GeForce RTX 4070 Ti".into(),
        vendor_id: 0x10de,
        device_id: 0x2782,
        driver_version: 0x0020_0000_0010_069c,
        dedicated_video_memory: 12 << 30,
        software: false,
    }
}

fn gpu_request(codec: Codec) -> PublishRequest {
    let query = EncodeQuery::default();
    PublishRequest {
        codec,
        encoder: EncoderPlan::gpu(adapter(1), codec_profile(codec, query), 7),
        ..request()
    }
}

/// A publisher with a live GPU attempt.
fn live_gpu(codec: Codec, now: Instant) -> Publisher {
    let mut publisher = publisher();
    publisher.begin_picker(1);
    publisher.publish(1, gpu_request(codec), false, now);
    publisher.engine_ready(now, false, None);
    publisher.on_engine(
        EngineEvent::EncoderOpened(HardwareEncoderIdentity {
            luid: adapter(1).luid,
            adapter: adapter(1).name,
            driver_version: adapter(1).driver_version,
            codec,
            generation: 7,
        }),
        now,
    );
    publisher.on_engine(EngineEvent::State(Connection::Connected), now);
    publisher.on_engine(EngineEvent::State(Connection::FirstFrame), now);
    assert_eq!(publisher.phase(), Phase::Live);
    publisher
}

#[test]
fn the_open_hardware_session_is_required_before_the_stream_is_live() {
    let now = Instant::now();
    let mut publisher = publisher();
    publisher.begin_picker(1);
    publisher.publish(1, gpu_request(Codec::H264), false, now);
    publisher.engine_ready(now, false, None);
    publisher.on_engine(EngineEvent::State(Connection::Connected), now);
    publisher.on_engine(EngineEvent::State(Connection::FirstFrame), now);
    assert_eq!(
        publisher.phase(),
        Phase::Publishing,
        "encoded frames alone must not confirm the GPU identity"
    );
    assert!(!publisher.sharing().live);

    publisher.on_engine(
        EngineEvent::EncoderOpened(HardwareEncoderIdentity {
            luid: adapter(1).luid,
            adapter: adapter(1).name,
            driver_version: adapter(1).driver_version,
            codec: Codec::H264,
            generation: 7,
        }),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Live);
    let sharing = publisher.sharing();
    assert_eq!(
        sharing.hardware.as_ref().map(|identity| identity.luid),
        Some(adapter(1).luid)
    );
    assert_eq!(
        sharing
            .confirmed
            .as_ref()
            .map(|confirmed| confirmed.encoder.backend),
        Some(EncoderBackend::Gpu)
    );
}

/// G05: another adapter, another generation or another codec never becomes the
/// GPU status; the attempt fails with an explicit error.
#[test]
fn a_mismatched_hardware_identity_is_refused_instead_of_confirmed() {
    let now = Instant::now();
    let mismatches = [
        HardwareEncoderIdentity {
            luid: adapter(2).luid,
            adapter: adapter(2).name,
            driver_version: adapter(2).driver_version,
            codec: Codec::H264,
            generation: 7,
        },
        HardwareEncoderIdentity {
            luid: adapter(1).luid,
            adapter: adapter(1).name,
            driver_version: adapter(1).driver_version,
            codec: Codec::H264,
            generation: 6,
        },
        HardwareEncoderIdentity {
            luid: adapter(1).luid,
            adapter: adapter(1).name,
            driver_version: adapter(1).driver_version,
            codec: Codec::Vp8,
            generation: 7,
        },
    ];
    for identity in mismatches {
        let mut publisher = publisher();
        publisher.begin_picker(1);
        publisher.publish(1, gpu_request(Codec::H264), false, now);
        publisher.engine_ready(now, false, None);
        let outcomes = publisher.on_engine(EngineEvent::EncoderOpened(identity.clone()), now);
        assert_eq!(publisher.phase(), Phase::Failed, "{identity:?}");
        assert!(
            outcomes.iter().any(|outcome| matches!(
                outcome,
                Outcome::Send(signal) if signal.kind == "stream.stop"
            )),
            "the refused session must close the publication: {outcomes:?}"
        );
        let sharing = publisher.sharing();
        assert!(sharing.hardware.is_none());
        assert!(sharing.confirmed.is_none());
        assert!(
            sharing
                .error
                .as_deref()
                .is_some_and(|error| error.contains("не совпал")),
            "{sharing:?}"
        );
    }
}

/// G05: a lost GPU encoder produces one message and never switches by itself.
#[test]
fn a_lost_gpu_encoder_waits_for_the_explicit_cpu_action() {
    let now = Instant::now();
    let mut publisher = live_gpu(Codec::H264, now);
    let outcomes = publisher.on_engine(
        EngineEvent::EncoderFailed {
            message: "Аппаратный кодировщик остановился: устройство отключено".into(),
            cpu_available: true,
        },
        now,
    );
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "nothing may restart automatically: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StopEngine)),
        "{outcomes:?}"
    );
    let sharing = publisher.sharing();
    assert_eq!(sharing.phase, Phase::Failed);
    assert!(sharing.cpu_fallback);
    assert!(sharing.error.is_some());

    let outcomes = publisher.continue_on_cpu(now);
    let starts: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::StartEngine { request, .. } => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1, "{outcomes:?}");
    assert_eq!(starts[0].codec, Codec::H264, "the codec never changes");
    assert_eq!(starts[0].encoder.backend, EncoderBackend::Cpu);
    assert!(starts[0].encoder.adapter.is_none());
    assert_eq!(publisher.phase(), Phase::Publishing);
    assert!(!publisher.sharing().cpu_fallback);
}

/// G05: without a CPU encoder for the same codec the action is not offered and
/// cannot be forced.
#[test]
fn a_cpu_continuation_needs_the_same_codec_on_the_cpu_build() {
    let now = Instant::now();
    let mut publisher = live_gpu(Codec::H264, now);
    let outcomes = publisher.on_engine(
        EngineEvent::EncoderFailed {
            message: "Аппаратный кодировщик остановился".into(),
            cpu_available: false,
        },
        now,
    );
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(!publisher.sharing().cpu_fallback);
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "{outcomes:?}"
    );
    assert!(publisher.continue_on_cpu(now).is_empty());
}

/// G06: a delayed failure of an attempt that is already closed cannot start a
/// second capture, resurrect `Live` or keep the fallback armed.
#[test]
fn a_late_gpu_failure_of_a_closed_attempt_is_ignored() {
    let now = Instant::now();
    let mut publisher = live_gpu(Codec::H264, now);
    publisher.abandon();
    assert_eq!(publisher.phase(), Phase::Idle);
    let late = publisher.on_engine(
        EngineEvent::EncoderFailed {
            message: "поздний отчёт".into(),
            cpu_available: true,
        },
        now,
    );
    assert!(late.is_empty(), "{late:?}");
    assert_eq!(publisher.phase(), Phase::Idle);
    assert!(!publisher.sharing().cpu_fallback);
    assert!(publisher.continue_on_cpu(now).is_empty());
}

/// The CPU path keeps its exact previous behaviour: no identity event and no
/// fallback are involved.
#[test]
fn a_cpu_attempt_does_not_need_a_hardware_identity() {
    let now = Instant::now();
    let mut publisher = publisher();
    publisher.begin_picker(1);
    publisher.publish(1, request(), false, now);
    publisher.engine_ready(now, false, None);
    publisher.on_engine(EngineEvent::State(Connection::Connected), now);
    publisher.on_engine(EngineEvent::State(Connection::FirstFrame), now);
    assert_eq!(publisher.phase(), Phase::Live);
    let sharing = publisher.sharing();
    assert!(sharing.hardware.is_none());
    assert_eq!(
        sharing
            .confirmed
            .as_ref()
            .map(|confirmed| confirmed.encoder.backend),
        Some(EncoderBackend::Cpu)
    );
}
