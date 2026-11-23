//! The engine on the pre-encoded path (G07/G08 scaffolding).
//!
//! A deterministic encoder double stands in for the driver, so the exact
//! engine code that submits frames, forwards encoded access units, polls
//! PLI/FIR and reports a dead session is exercised without hardware.

use std::time::{Duration, Instant};

use libwebrtc::{
    MediaType,
    ice_candidate::IceCandidate,
    peer_connection::OfferOptions,
    rtp_transceiver::{RtpTransceiverDirection, RtpTransceiverInit},
    session_description::{SdpType, SessionDescription},
};
use tokio::time::sleep;

use super::codec_tests::{candidates, drain, drain_to_engine};
use super::*;
use crate::video_encoder::{
    AdapterInfo, AdapterLuid, EncodeSubmit, EncodedAccessUnit, EncoderCounters, EncoderPlan,
    FakeGpuEncoder, GpuEncoder, GpuEncoderFailure, Nv12Frame, codec_profile,
};

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

/// An encoder that accepts input and never emits an access unit, like a driver
/// that opened the transform but never produced a frame.
struct SilentEncoder {
    adapter: AdapterInfo,
}

impl GpuEncoder for SilentEncoder {
    fn submit(&mut self, _frame: Nv12Frame) -> EncodeSubmit {
        EncodeSubmit::Queued
    }

    fn next_packet(&mut self) -> Option<EncodedAccessUnit> {
        None
    }

    fn request_keyframe(&mut self) {}

    fn set_bitrate(&mut self, _target_bps: u32) {}

    fn counters(&self) -> EncoderCounters {
        EncoderCounters::default()
    }

    fn adapter(&self) -> Option<AdapterInfo> {
        Some(self.adapter.clone())
    }

    fn failure(&self) -> Option<GpuEncoderFailure> {
        None
    }

    fn shutdown(&mut self) {}
}

/// M7: a GPU attempt whose codec never confirms is a failed hardware session,
/// not an ordinary codec error. The engine reports `EncoderFailed` with the
/// CPU availability, which is the one message behind «Продолжить на
/// процессоре»; it still never switches by itself.
#[test]
fn an_unconfirmed_gpu_codec_reports_the_cpu_option_instead_of_a_plain_error() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let factory = PeerConnectionFactory::default();
        let sfu = factory
            .create_peer_connection(Default::default())
            .expect("sfu peer");
        let capabilities = factory.get_rtp_sender_capabilities(MediaType::Video);
        let profile = negotiation::sender_profile(Codec::H264).expect("checked H.264 profile");
        let video = sfu
            .add_transceiver_for_media(
                MediaType::Video,
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::RecvOnly,
                    stream_ids: Vec::new(),
                    send_encodings: Vec::new(),
                },
            )
            .expect("recvonly transceiver");
        video
            .set_codec_preferences(negotiation::preferences(
                &capabilities.codecs,
                Codec::H264,
                profile,
            ))
            .expect("the fake SFU pins H.264");
        let offer = sfu
            .create_offer(OfferOptions {
                offer_to_receive_audio: None,
                offer_to_receive_video: None,
                ..OfferOptions::default()
            })
            .await
            .expect("offer");
        let sdp = offer.to_string();
        sfu.set_local_description(offer)
            .await
            .expect("sfu local offer");

        let engine = PublisherEngine::start_synthetic_gpu(
            gpu_request(Codec::H264),
            Vec::new(),
            "all".into(),
            (1280, 720),
            Box::new(SilentEncoder { adapter: adapter() }),
        )
        .expect("gpu engine");
        assert!(engine.set_codec_confirm_timeout(Duration::from_secs(2)));
        let sfu_candidates = candidates(&sfu);
        engine.offer(sdp).expect("offer command");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failure = None;
        let mut plain_error = None;
        while Instant::now() < deadline && failure.is_none() && plain_error.is_none() {
            while let Some(event) = engine.poll() {
                match event {
                    EngineEvent::Answer(answer) => {
                        let parsed = SessionDescription::parse(&answer, SdpType::Answer)
                            .expect("answer sdp");
                        let _ = sfu.set_remote_description(parsed).await;
                    }
                    EngineEvent::Ice(candidate) => {
                        let mid = candidate.sdp_mid.clone().unwrap_or_default();
                        let index = candidate.sdp_mline_index.map(i32::from).unwrap_or(-1);
                        if let Ok(parsed) = IceCandidate::parse(&mid, index, &candidate.candidate) {
                            let _ = sfu.add_ice_candidate(parsed).await;
                        }
                    }
                    EngineEvent::EncoderFailed {
                        message,
                        cpu_available,
                    } => failure = Some((message, cpu_available)),
                    EngineEvent::Error(message) => plain_error = Some(message),
                    _ => {}
                }
            }
            drain(&sfu_candidates, &sfu).await;
            drain_to_engine(&sfu_candidates, &engine);
            sleep(Duration::from_millis(20)).await;
        }
        sfu.close();
        assert!(
            plain_error.is_none(),
            "a GPU attempt must not report a plain codec error: {plain_error:?}"
        );
        let (message, cpu_available) =
            failure.expect("the engine never reported the unconfirmed GPU codec");
        assert!(message.contains("не подтверждён"), "{message}");
        assert!(
            cpu_available,
            "H.264 exists on the pinned CPU build, so the continuation is offered"
        );
    });
}
