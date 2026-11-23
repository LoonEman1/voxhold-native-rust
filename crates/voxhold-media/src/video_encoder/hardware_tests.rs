//! G08: the manual hardware acceptance tests.
//!
//! They are not automatable in CI: each needs a real GPU driver and a real
//! encoder session. Run them locally with
//! `cargo test -p voxhold-media hardware_ -- --ignored --nocapture --test-threads=1`.
//!
//! Every test owns a [`WorkerPlatform`] on its own thread. Without it each
//! Media Foundation call fails with `CO_E_NOTINITIALIZED` (`0x800401f0`), and a
//! green "ok" would prove nothing.

use std::time::Duration;

use crate::publisher::SdrProfile;
use crate::video_encoder::windows::WorkerPlatform;
use crate::video_encoder::{
    AdapterInfo, AdapterInventory, AdapterSelection, Codec, EncodeQuery, EncodedAccessUnit,
    EncoderProbeService, EncoderProbeTarget, EncoderRequest, GpuEncoder, HardwareProbe, Nv12Frame,
    PlatformProbe, ProbeCancel, ProbeOutcome, VideoEncoderPreference, driver_version,
    open_gpu_encoder,
};

fn q720() -> EncodeQuery {
    EncodeQuery::new(1280, 720, 30)
}

/// COM apartment and Media Foundation of one test thread.
struct Platform(#[allow(dead_code)] WorkerPlatform);

fn platform() -> Platform {
    let platform = WorkerPlatform::new();
    assert!(
        platform.is_ready(),
        "COM/Media Foundation не инициализировались на тестовом потоке"
    );
    Platform(platform)
}

fn inventory(probe: &PlatformProbe) -> AdapterInventory {
    let inventory = probe.inventory().expect("DXGI enumeration failed");
    for adapter in inventory.adapters() {
        eprintln!(
            "adapter luid={} name={} vendor=0x{:04x} device=0x{:04x} driver=0x{:x}",
            adapter.luid,
            adapter.name,
            adapter.vendor_id,
            adapter.device_id,
            adapter.driver_version
        );
    }
    inventory
}

/// `None` means the machine has no hardware adapter at all: the test then
/// reports an explicit SKIP instead of a green run without proof.
fn probe_one(codec: Codec, query: EncodeQuery) -> Option<ProbeOutcome> {
    let _platform = platform();
    let probe = PlatformProbe;
    let inventory = inventory(&probe);
    if inventory.adapters().is_empty() {
        eprintln!("SKIP: на машине нет аппаратного адаптера, проверять нечего");
        return None;
    }
    let adapter = inventory
        .resolve(AdapterSelection::Auto)
        .expect("no suitable hardware adapter");
    assert_driver_identity(adapter);
    Some(probe.probe(adapter, codec, query, &ProbeCancel::never()))
}

/// The driver identity is the packed dotted version. Zero is legitimate only
/// when neither the UMD query nor the display-class registry entry answers, so
/// a zero with a reachable registry entry is a bug, not a skipped check. The
/// registry value is printed: it is the fallback an adapter without the D3D10
/// interface gets, and it shares the encoding with the UMD value.
fn assert_driver_identity(adapter: &AdapterInfo) {
    let registry = driver_version::registry(adapter.vendor_id, adapter.device_id);
    if adapter.driver_version == 0 {
        match registry {
            Some(version) => panic!(
                "версия драйвера {} равна 0, хотя реестр отдаёт 0x{version:x}",
                adapter.name
            ),
            None => eprintln!(
                "SKIP driver version: ни UMD, ни реестр не вернули версию для {}",
                adapter.name
            ),
        }
        return;
    }
    match registry {
        Some(version) => eprintln!(
            "driver identity {}: UMD=0x{:x} registry=0x{version:x}",
            adapter.name, adapter.driver_version
        ),
        None => eprintln!(
            "driver identity {}: UMD=0x{:x} registry недоступен",
            adapter.name, adapter.driver_version
        ),
    }
}

#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_h264_mft_720p30() {
    let Some(outcome) = probe_one(Codec::H264, q720()) else {
        return;
    };
    match outcome {
        ProbeOutcome::Confirmed(SdrProfile::H264 { baseline, level }) => {
            eprintln!("confirmed codec=H264 baseline={baseline:?} level=0x{level:02x} 1280x720@30");
        }
        ProbeOutcome::Confirmed(other) => {
            panic!("H.264 подтверждён другим SDR-профилем: {other:?}");
        }
        ProbeOutcome::Unavailable(reason) => {
            panic!("H.264 720p30 не подтверждён: {} ({reason})", reason.kind())
        }
        ProbeOutcome::Cancelled => panic!("the never-cancelled probe reported cancel"),
    }
}

#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_av1_mft_720p30() {
    let Some(outcome) = probe_one(Codec::Av1, q720()) else {
        return;
    };
    match outcome {
        ProbeOutcome::Confirmed(profile) => {
            eprintln!("confirmed codec=AV1 profile={profile:?} 1280x720@30");
        }
        ProbeOutcome::Unavailable(reason) => {
            assert!(
                !reason.detail().contains("0x800401f0"),
                "COM не инициализирован, это не ответ драйвера: {reason}"
            );
            eprintln!(
                "not confirmed codec=AV1 reason={} ({reason})",
                reason.kind()
            );
        }
        ProbeOutcome::Cancelled => panic!("the never-cancelled probe reported cancel"),
    }
}

#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_inventory_is_stable_and_excludes_warp() {
    let _platform = platform();
    let probe = PlatformProbe;
    let first = inventory(&probe);
    let second = probe.inventory().expect("DXGI enumeration failed");
    assert_eq!(first.adapters(), second.adapters());
    assert!(
        first.adapters().iter().all(|adapter| !adapter.software),
        "software adapters must be filtered by the inventory"
    );
    if first.adapters().is_empty() {
        eprintln!("SKIP: на машине нет аппаратного адаптера, проверять нечего");
        return;
    }
    for adapter in first.adapters() {
        assert_driver_identity(adapter);
    }
}

/// The real end-to-end path: the service worker owns the platform, probes the
/// automatic adapter and publishes a snapshot for the dialog.
#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_service_confirms_h264() {
    let service = EncoderProbeService::platform().expect("probe service");
    let generation = service
        .set_video_encoder_backend(EncoderRequest::new(
            VideoEncoderPreference::gpu(AdapterSelection::Auto, false),
            q720(),
            EncoderProbeTarget::default(),
        ))
        .expect("request");
    let snapshot = service
        .wait_for(generation, Duration::from_secs(60))
        .expect("the probe did not finish");
    for adapter in snapshot.adapters() {
        eprintln!(
            "service adapter luid={} name={} driver=0x{:x}",
            adapter.luid, adapter.name, adapter.driver_version
        );
    }
    if !snapshot.has_hardware_adapter() {
        eprintln!("SKIP: на машине нет аппаратного адаптера, проверять нечего");
        service.shutdown();
        return;
    }
    let adapter = snapshot.adapter().expect("adapter");
    assert_driver_identity(adapter);
    for codec in Codec::ALL {
        let slot = snapshot.gpu(codec).expect("codec row");
        assert!(slot.is_checked(), "GPU-путь не проверялся");
        match (slot.profile(), slot.reason()) {
            (Some(profile), _) => {
                eprintln!("gpu codec={} confirmed profile={profile:?}", codec.label());
            }
            (None, Some(reason)) => eprintln!(
                "gpu codec={} reason={} ({reason})",
                codec.label(),
                reason.kind()
            ),
            (None, None) => eprintln!("gpu codec={} не проверялся", codec.label()),
        }
    }
    match snapshot.gpu(Codec::H264).and_then(|slot| slot.profile()) {
        Some(profile) => eprintln!(
            "confirmed codec=H264 adapter={} profile={profile:?} 1280x720@30",
            adapter.name
        ),
        None => panic!(
            "H.264 не подтверждён через сервис: {:?}",
            snapshot
                .gpu(Codec::H264)
                .and_then(|slot| slot.reason())
                .map(|reason| (reason.kind(), reason.to_string()))
        ),
    }
    service.shutdown();
}

/// A neutral black NV12 frame, never captured content.
fn nv12(query: EncodeQuery, timestamp_us: i64) -> Nv12Frame {
    let luma = query.width as usize * query.height as usize;
    let mut data = vec![0x10u8; luma + luma / 2];
    data[luma..].fill(0x80);
    Nv12Frame {
        width: query.width,
        height: query.height,
        data,
        timestamp_us,
    }
}

fn hardware_encoder(codec: Codec, query: EncodeQuery) -> Option<Box<dyn GpuEncoder>> {
    let _platform = platform();
    let probe = PlatformProbe;
    let inventory = inventory(&probe);
    let Some(adapter) = inventory.resolve(AdapterSelection::Auto).ok().cloned() else {
        eprintln!("SKIP: на машине нет аппаратного адаптера, проверять нечего");
        return None;
    };
    assert_driver_identity(&adapter);
    match open_gpu_encoder(&adapter, codec, query, 2_500_000) {
        Ok(encoder) => {
            eprintln!(
                "session codec={} adapter={} luid={} driver=0x{:x}",
                codec.label(),
                adapter.name,
                adapter.luid,
                adapter.driver_version
            );
            Some(encoder)
        }
        Err(message) => {
            eprintln!("SKIP session codec={} reason={message}", codec.label());
            None
        }
    }
}

/// Drive the real encoder until it produced `count` access units. The driver
/// decides how many inputs precede the first keyframe, so more frames are
/// submitted while the output is empty; the loop is bounded.
fn packets(
    encoder: &mut Box<dyn GpuEncoder>,
    query: EncodeQuery,
    count: usize,
    start_timestamp: i64,
) -> Vec<EncodedAccessUnit> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut packets = Vec::new();
    let mut timestamp = start_timestamp;
    let mut submitted = 0;
    while std::time::Instant::now() < deadline && packets.len() < count {
        if submitted < 64 {
            encoder.submit(nv12(query, timestamp));
            timestamp += 33_000;
            submitted += 1;
        }
        while let Some(packet) = encoder.next_packet() {
            packets.push(packet);
            if packets.len() >= count {
                break;
            }
        }
        if let Some(failure) = encoder.failure() {
            panic!("the hardware session failed: {failure}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        packets.len(),
        count,
        "no encoded access unit was produced; counters={:?} failure={:?}",
        encoder.counters(),
        encoder.failure()
    );
    packets
}

/// Collect access units until the encoder really emitted a keyframe. A
/// keyframe request may still be preceded by an access unit for a frame that
/// was already accepted, so the stream is scanned, not assumed.
fn packets_until_key(
    encoder: &mut Box<dyn GpuEncoder>,
    query: EncodeQuery,
    start_timestamp: i64,
) -> EncodedAccessUnit {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut timestamp = start_timestamp;
    let mut submitted = 0;
    while std::time::Instant::now() < deadline {
        if submitted < 64 {
            encoder.submit(nv12(query, timestamp));
            timestamp += 33_000;
            submitted += 1;
        }
        while let Some(packet) = encoder.next_packet() {
            if packet.keyframe {
                return packet;
            }
        }
        if let Some(failure) = encoder.failure() {
            panic!("the hardware session failed: {failure}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "no keyframe after the request; counters={:?}",
        encoder.counters()
    );
}

fn assert_h264_access_unit(packet: &EncodedAccessUnit, keyframe: bool) {
    assert!(
        packet.payload.windows(3).any(|window| window == [0, 0, 1]),
        "H.264 output is not an Annex B byte stream"
    );
    let types = crate::video_encoder::h264::nal_types(&packet.payload);
    assert!(
        types.contains(&7) && types.contains(&8),
        "a keyframe must carry SPS/PPS: {types:?}"
    );
    assert_eq!(
        types.contains(&5),
        keyframe,
        "the reported frame type does not match the NAL units: {types:?}"
    );
}

/// G08 (H.264) and G07 (keyframe, bitrate, shutdown) on the real card.
#[test]
#[ignore = "needs real Windows hardware; G07/G08 manual run"]
fn hardware_h264_session_encodes_keyframes_and_rate_control() {
    let query = q720();
    let Some(mut encoder) = hardware_encoder(Codec::H264, query) else {
        return;
    };
    let first = packets(&mut encoder, query, 1, 1_000);
    assert!(
        first[0].keyframe,
        "the first access unit must be a keyframe"
    );
    assert_h264_access_unit(&first[0], true);

    let delta = packets(&mut encoder, query, 1, 1_000_000);
    assert_h264_access_unit(&delta[0], delta[0].keyframe);
    assert!(
        delta[0].timestamp_us > first[0].timestamp_us,
        "timestamps must increase: {first:?} {delta:?}"
    );

    encoder.request_keyframe();
    let key = packets_until_key(&mut encoder, query, 2_000_000);
    assert!(key.keyframe, "PLI recovery must be a keyframe");
    assert_h264_access_unit(&key, true);

    encoder.set_bitrate(1_200_000);
    let _ = packets(&mut encoder, query, 1, 3_000_000);
    let counters = encoder.counters();
    eprintln!("h264 session counters {counters:?}");
    assert_eq!(counters.bitrate_bps, 1_200_000);
    assert!(
        counters.bitrate_applied,
        "the driver did not accept CODECAPI_AVEncCommonMeanBitRate"
    );
    assert!(counters.delivered(), "no frame was delivered");

    let started = std::time::Instant::now();
    encoder.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "shutdown did not release the session"
    );
}

/// G08 (AV1) on a driver that registers an AV1 encoder (the RTX 40 cards do).
#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_av1_session_encodes_a_sequence_header_and_keyframes() {
    let query = q720();
    let Some(mut encoder) = hardware_encoder(Codec::Av1, query) else {
        return;
    };
    let first = packets(&mut encoder, query, 1, 1_000);
    assert!(
        first[0].keyframe,
        "the first AV1 temporal unit is a keyframe"
    );
    assert!(
        crate::video_encoder::av1::has_sequence_header(&first[0].payload),
        "the keyframe must carry the AV1 sequence header"
    );
    encoder.request_keyframe();
    let key = packets_until_key(&mut encoder, query, 2_000_000);
    assert!(key.keyframe, "PLI recovery must be a keyframe");
    encoder.shutdown();
}
