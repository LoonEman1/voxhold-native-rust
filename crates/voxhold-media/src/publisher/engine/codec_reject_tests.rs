//! C04: an incompatible negotiation is refused once, the peer is released and
//! the same channel can start a new attempt.
//!
//! Each case reaches the real engine: a foreign primary codec, no compatible
//! H.264 profile, an inactive video m-line and — separately — an ICE connection
//! that never produced an encoded frame. ICE and the answer must never be
//! mistaken for a transmission, and only the first error of an attempt is
//! delivered; the released worker cannot accept another offer.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use libwebrtc::{
    MediaType,
    ice_candidate::IceCandidate,
    peer_connection::{OfferOptions, PeerConnection, PeerConnectionState},
    peer_connection_factory::PeerConnectionFactory,
    rtp_transceiver::{RtpTransceiverDirection, RtpTransceiverInit},
    session_description::{SdpType, SessionDescription},
    stats::RtcStats,
};
use tokio::time::sleep;

use super::{
    PublisherEngine,
    codec_tests::{candidates, drain, drain_to_engine, media_mid, request},
    negotiation,
};
use crate::publisher::{Codec, EngineEvent, H264Baseline, SdrProfile};

struct Refusal {
    errors: Vec<String>,
    answers: u32,
    connected: u32,
    /// The worker stopped accepting commands (its peer and capture are gone).
    released: bool,
    sfu_connected: bool,
    inbound_video_packets: u64,
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("codec rejection runtime")
        .block_on(future)
}

/// A peer whose video m-line is pinned to `codec` with `profile`.
fn sfu_peer(factory: &PeerConnectionFactory, codec: Codec, profile: SdrProfile) -> PeerConnection {
    let sfu = factory
        .create_peer_connection(Default::default())
        .expect("sfu peer");
    let capabilities = factory.get_rtp_sender_capabilities(MediaType::Video);
    let video = sfu
        .add_transceiver_for_media(
            MediaType::Video,
            RtpTransceiverInit {
                direction: RtpTransceiverDirection::RecvOnly,
                stream_ids: Vec::new(),
                send_encodings: Vec::new(),
            },
        )
        .expect("video transceiver");
    video
        .set_codec_preferences(negotiation::preferences(
            &capabilities.codecs,
            codec,
            profile,
        ))
        .expect("the SFU pins the rendition codec");
    sfu
}

async fn offer_of(sfu: &PeerConnection) -> String {
    let offer = sfu
        .create_offer(OfferOptions {
            offer_to_receive_audio: None,
            offer_to_receive_video: None,
            ..OfferOptions::default()
        })
        .await
        .expect("offer");
    sfu.set_local_description(offer.clone())
        .await
        .expect("sfu local offer");
    offer.to_string()
}

/// A real engine that is expected to refuse `offer_sdp`.
async fn refuse(engine: PublisherEngine, sfu: &PeerConnection, offer_sdp: &str) -> Refusal {
    let sfu_candidates = candidates(sfu);
    engine.offer(offer_sdp.to_owned()).expect("offer command");
    let mut errors = Vec::new();
    let mut answers = 0_u32;
    let mut connected = 0_u32;
    let mut sfu_connected = false;
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        while let Some(event) = engine.poll() {
            match event {
                EngineEvent::Answer(sdp) => {
                    answers += 1;
                    let parsed =
                        SessionDescription::parse(&sdp, SdpType::Answer).expect("answer sdp");
                    let _ = sfu.set_remote_description(parsed).await;
                }
                EngineEvent::Ice(candidate) => {
                    let mid = candidate.sdp_mid.clone().unwrap_or_default();
                    let index = candidate.sdp_mline_index.map(i32::from).unwrap_or(-1);
                    if let Ok(parsed) = IceCandidate::parse(&mid, index, &candidate.candidate) {
                        let _ = sfu.add_ice_candidate(parsed).await;
                    }
                }
                EngineEvent::State(crate::publisher::Connection::Connected) => connected += 1,
                EngineEvent::State(_) => {}
                EngineEvent::Error(message) => errors.push(message),
                EngineEvent::Closed => errors.push("engine closed".into()),
                _ => {}
            }
        }
        sfu_connected |= sfu.connection_state() == PeerConnectionState::Connected;
        drain(&sfu_candidates, sfu).await;
        drain_to_engine(&sfu_candidates, &engine);
        if !errors.is_empty() {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    // The one error must stay the only one: a second push would end the
    // attempt twice.
    let extra = Instant::now() + Duration::from_millis(400);
    while Instant::now() < extra {
        while let Some(event) = engine.poll() {
            match event {
                EngineEvent::Error(message) => errors.push(message),
                EngineEvent::State(crate::publisher::Connection::Connected) => connected += 1,
                EngineEvent::Answer(_) => answers += 1,
                _ => {}
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    // The worker thread ends the loop, closes its peer and drops the command
    // receiver; a later offer is refused by the released engine.
    let mut released = false;
    let release_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < release_deadline {
        if engine.offer("v=0 released".into()).is_err() {
            released = true;
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let inbound_video_packets = inbound_packets(sfu).await;
    Refusal {
        errors,
        answers,
        connected,
        released,
        sfu_connected,
        inbound_video_packets,
    }
}

async fn inbound_packets(sfu: &PeerConnection) -> u64 {
    let Ok(stats) = sfu.get_stats().await else {
        return 0;
    };
    stats
        .into_iter()
        .filter_map(|stat| match stat {
            RtcStats::InboundRtp(inbound) if inbound.stream.kind == "video" => {
                Some(inbound.received.packets_received)
            }
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn checked_profile(codec: Codec) -> SdrProfile {
    negotiation::sender_profile(codec).expect("checked sender profile")
}

#[test]
fn an_offer_with_another_primary_codec_is_refused_once_and_released() {
    block_on(async {
        let factory = PeerConnectionFactory::default();
        let sfu = sfu_peer(&factory, Codec::H264, checked_profile(Codec::H264));
        let offer = offer_of(&sfu).await;
        // The publisher asked for VP8; the SFU offers H.264 only.
        let engine = PublisherEngine::start_synthetic(
            request(Codec::Vp8, false),
            Vec::new(),
            "all".into(),
            false,
            (640, 360),
            None,
        )
        .expect("publisher engine");
        let refusal = refuse(engine, &sfu, &offer).await;
        sfu.close();
        assert_eq!(
            refusal.errors.len(),
            1,
            "exactly one error per attempt: {:?}",
            refusal.errors
        );
        assert!(
            refusal.errors[0].contains("не предложил кодек VP8"),
            "the reason names the requested codec: {:?}",
            refusal.errors
        );
        assert_eq!(refusal.answers, 0, "an incompatible offer is not answered");
        assert_eq!(refusal.connected, 0, "the transmission is not confirmed");
        assert!(refusal.released, "the incompatible peer is released once");
        assert_eq!(refusal.inbound_video_packets, 0);
    });
}

#[test]
fn an_h264_offer_without_the_constrained_profile_is_refused() {
    block_on(async {
        let factory = PeerConnectionFactory::default();
        let legacy = SdrProfile::H264 {
            baseline: H264Baseline::Legacy,
            level: 0x1f,
        };
        let sfu = sfu_peer(&factory, Codec::H264, legacy);
        let offer = offer_of(&sfu).await;
        let engine = PublisherEngine::start_synthetic(
            request(Codec::H264, false),
            Vec::new(),
            "all".into(),
            false,
            (640, 360),
            None,
        )
        .expect("publisher engine");
        let refusal = refuse(engine, &sfu, &offer).await;
        sfu.close();
        assert_eq!(refusal.errors.len(), 1, "{:?}", refusal.errors);
        assert!(
            refusal.errors[0].contains("несовместимым профилем"),
            "the reason names the profile: {:?}",
            refusal.errors
        );
        assert_eq!(refusal.answers, 0);
        assert!(refusal.released);
    });
}

#[test]
fn an_inactive_video_line_is_refused() {
    block_on(async {
        let factory = PeerConnectionFactory::default();
        let sfu = sfu_peer(&factory, Codec::Vp8, checked_profile(Codec::Vp8));
        let offer = offer_of(&sfu).await;
        let inactive = offer.replace("a=recvonly", "a=inactive");
        assert_ne!(inactive, offer, "the test offer really changed");
        let engine = PublisherEngine::start_synthetic(
            request(Codec::Vp8, false),
            Vec::new(),
            "all".into(),
            false,
            (640, 360),
            None,
        )
        .expect("publisher engine");
        let refusal = refuse(engine, &sfu, &inactive).await;
        sfu.close();
        assert_eq!(refusal.errors.len(), 1, "{:?}", refusal.errors);
        assert!(
            refusal.errors[0].contains("выключил видеодорожку"),
            "{:?}",
            refusal.errors
        );
        assert_eq!(refusal.answers, 0);
        assert!(refusal.released);
    });
}

#[test]
fn an_ice_connection_without_encoded_frames_is_not_confirmed() {
    block_on(async {
        let factory = PeerConnectionFactory::default();
        let sfu = sfu_peer(&factory, Codec::Vp8, checked_profile(Codec::Vp8));
        let offer = offer_of(&sfu).await;
        let _mid = media_mid(&offer, "video").expect("offered video mid");
        // The capture never delivers a frame: ICE can connect, the encoder has
        // nothing to produce.
        let engine = PublisherEngine::start_synthetic_stalled(
            request(Codec::Vp8, false),
            Vec::new(),
            "all".into(),
        )
        .expect("publisher engine");
        assert!(
            engine.set_codec_confirm_timeout(Duration::from_secs(3)),
            "the test shortens the codec confirmation window"
        );
        let refusal = refuse(engine, &sfu, &offer).await;
        sfu.close();
        assert_eq!(refusal.errors.len(), 1, "{:?}", refusal.errors);
        assert!(
            refusal.errors[0].contains("не подтверждён"),
            "the reason is the missing transmission: {:?}",
            refusal.errors
        );
        assert_eq!(
            refusal.answers, 1,
            "the engine answered before the frames were due"
        );
        assert_eq!(
            refusal.connected, 0,
            "ICE and the answer never confirm the transmission"
        );
        assert!(
            refusal.sfu_connected,
            "the scenario needs a really connected peer with no encoded frames"
        );
        assert_eq!(
            refusal.inbound_video_packets, 0,
            "no video packet may reach the SFU"
        );
        assert!(refusal.released);
    });
}

/// The same channel slot can start a new attempt after a refused one.
#[test]
fn a_new_attempt_starts_after_a_refused_negotiation() {
    block_on(async {
        let factory = PeerConnectionFactory::default();
        let foreign = sfu_peer(&factory, Codec::H264, checked_profile(Codec::H264));
        let foreign_offer = offer_of(&foreign).await;
        let failed = PublisherEngine::start_synthetic(
            request(Codec::Vp8, false),
            Vec::new(),
            "all".into(),
            false,
            (640, 360),
            None,
        )
        .expect("first engine");
        let refusal = refuse(failed, &foreign, &foreign_offer).await;
        foreign.close();
        assert_eq!(refusal.errors.len(), 1);
        assert!(refusal.released);

        // A new attempt on the same request: only the offer changes.
        let sfu = sfu_peer(&factory, Codec::Vp8, checked_profile(Codec::Vp8));
        let offer = offer_of(&sfu).await;
        let sfu_candidates = candidates(&sfu);
        let engine = PublisherEngine::start_synthetic(
            request(Codec::Vp8, false),
            Vec::new(),
            "all".into(),
            false,
            (640, 360),
            None,
        )
        .expect("second engine");
        engine.offer(offer).expect("second offer command");
        let answered = Arc::new(AtomicU32::new(0));
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut errors = Vec::new();
        while Instant::now() < deadline && answered.load(Ordering::SeqCst) == 0 {
            while let Some(event) = engine.poll() {
                match event {
                    EngineEvent::Answer(_) => {
                        answered.fetch_add(1, Ordering::SeqCst);
                    }
                    EngineEvent::Error(message) => errors.push(message),
                    _ => {}
                }
            }
            drain(&sfu_candidates, &sfu).await;
            drain_to_engine(&sfu_candidates, &engine);
            sleep(Duration::from_millis(20)).await;
        }
        drop(engine);
        sfu.close();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            answered.load(Ordering::SeqCst),
            1,
            "the new attempt answers exactly once"
        );
    });
}
