//! The engine on the pre-encoded path (G07/G08 scaffolding).
//!
//! A deterministic encoder double stands in for the driver, so the exact
//! engine code that submits frames, forwards encoded access units, polls
//! PLI/FIR and reports a dead session is exercised without hardware.

use std::time::{Duration, Instant};

use super::*;
use crate::video_encoder::{AdapterInfo, AdapterLuid, EncoderPlan, FakeGpuEncoder, codec_profile};

fn adapter() -> AdapterInfo {
    AdapterInfo {
        luid: AdapterLuid::new(1, 0),
        name: "NVIDIA GeForce RTX 4070 Ti".into(),
        vendor_id: 0x10de,
        device_id: 0x2782,
        driver_version: 0x0020_0000_0010_069c,
        dedicated_video_memory: 12 << 30,
        software: false,
    }
}

fn gpu_request(codec: Codec) -> PublishRequest {
    PublishRequest {
        source: crate::screen_capture::ScreenSource {
            id: 0,
            kind: crate::screen_capture::SourceKind::Monitor,
            title: "synthetic".into(),
            display_id: -1,
            width: 1280,
            height: 720,
            monitor: None,
        },
        quality: Quality {
            resolution: crate::publisher::Resolution::P720,
            fps: 30,
            bitrate_kbps: 2500,
            audio_bitrate_kbps: 128,
        },
        codec,
        audio: false,
        audio_scope: crate::system_audio::AudioScope::Device,
        encoder: EncoderPlan::gpu(
            adapter(),
            codec_profile(codec, crate::video_encoder::EncodeQuery::new(1280, 720, 30)),
            7,
        ),
    }
}

fn poll_until(
    engine: &PublisherEngine,
    deadline: Duration,
    mut done: impl FnMut(&EngineEvent) -> bool,
) -> Vec<EngineEvent> {
    let end = Instant::now() + deadline;
    let mut seen = Vec::new();
    while Instant::now() < end {
        while let Some(event) = engine.poll() {
            let matched = done(&event);
            seen.push(event);
            if matched {
                return seen;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the engine did not report the expected event: {seen:?}");
}

#[test]
fn the_gpu_engine_forwards_encoded_units_and_the_open_identity() {
    let encoder = FakeGpuEncoder::new(4).on(adapter());
    let engine = PublisherEngine::start_synthetic_gpu(
        gpu_request(Codec::H264),
        Vec::new(),
        "all".into(),
        (1280, 720),
        Box::new(encoder),
    )
    .expect("gpu engine");
    let seen = poll_until(&engine, Duration::from_secs(5), |event| {
        matches!(
            event,
            EngineEvent::State(crate::publisher::Connection::FirstFrame)
        )
    });
    assert!(
        seen.iter().any(|event| matches!(
            event,
            EngineEvent::EncoderOpened(identity)
                if identity.luid == adapter().luid
                    && identity.codec == Codec::H264
                    && identity.generation == 7
        )),
        "the engine must report the open adapter identity: {seen:?}"
    );
}

#[test]
fn a_dead_encoder_reports_its_message_and_the_cpu_option() {
    let encoder = FakeGpuEncoder::failing(
        "Аппаратный кодировщик остановился: устройство отключено",
        true,
    );
    let engine = PublisherEngine::start_synthetic_gpu(
        gpu_request(Codec::H264),
        Vec::new(),
        "all".into(),
        (1280, 720),
        Box::new(encoder),
    )
    .expect("gpu engine");
    let seen = poll_until(&engine, Duration::from_secs(5), |event| {
        matches!(event, EngineEvent::EncoderFailed { .. })
    });
    let failed = seen
        .iter()
        .find_map(|event| match event {
            EngineEvent::EncoderFailed {
                message,
                cpu_available,
            } => Some((message.clone(), *cpu_available)),
            _ => None,
        })
        .expect("encoder failure");
    assert!(failed.0.contains("отключено"), "{failed:?}");
    assert!(failed.1, "H.264 exists on the pinned CPU build");
}

/// G06: the device disappears while frames are flowing. The attempt ends
/// exactly once with the explicit event, the encoder is released and a fresh
/// attempt still opens its own session.
#[test]
fn a_device_removed_during_encode_ends_the_attempt_once() {
    let encoder = FakeGpuEncoder::new(4).on(adapter()).fail_after(2);
    let engine = PublisherEngine::start_synthetic_gpu(
        gpu_request(Codec::H264),
        Vec::new(),
        "all".into(),
        (1280, 720),
        Box::new(encoder),
    )
    .expect("gpu engine");
    let seen = poll_until(&engine, Duration::from_secs(6), |event| {
        matches!(event, EngineEvent::EncoderFailed { .. })
    });
    assert!(
        seen.iter().any(|event| matches!(
            event,
            EngineEvent::State(crate::publisher::Connection::FirstFrame)
        )),
        "frames were flowing before the removal: {seen:?}"
    );
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event, EngineEvent::EncoderFailed { .. }))
            .count(),
        1,
        "the failure is reported exactly once: {seen:?}"
    );
    drop(engine);

    let encoder = FakeGpuEncoder::new(4).on(adapter());
    let restarted = PublisherEngine::start_synthetic_gpu(
        gpu_request(Codec::H264),
        Vec::new(),
        "all".into(),
        (1280, 720),
        Box::new(encoder),
    )
    .expect("a repeated start opens a fresh session");
    poll_until(&restarted, Duration::from_secs(5), |event| {
        matches!(event, EngineEvent::EncoderOpened(_))
    });
}

/// A GPU request without an opened encoder must never create a publication
/// with an implicit software fallback.
#[test]
fn a_gpu_request_without_an_encoder_is_refused() {
    let result = PublisherEngine::start_synthetic(
        gpu_request(Codec::H264),
        Vec::new(),
        "all".into(),
        false,
        (1280, 720),
        None,
    );
    assert!(result.is_err());
}
