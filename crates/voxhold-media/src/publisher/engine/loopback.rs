//! In-process publisher ↔ SFU proof.
//!
//! A fake SFU adds one recvonly video transceiver, exactly like
//! `internal/stream/manager.go` does for a single SDR rendition, and creates
//! the offer. The real `PublisherEngine` answers it. The test then decodes on
//! the SFU side, so a broken transceiver binding fails instead of only
//! reporting "no error".

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use libwebrtc::{
    MediaType,
    ice_candidate::IceCandidate,
    media_stream_track::MediaStreamTrack,
    peer_connection::{OfferOptions, PeerConnection},
    peer_connection_factory::PeerConnectionFactory,
    rtp_transceiver::{RtpTransceiverDirection, RtpTransceiverInit},
    session_description::{SdpType, SessionDescription},
    stats::RtcStats,
    video_stream::native::NativeVideoStream,
    video_track::RtcVideoTrack,
};
use tokio::time::sleep;
use tokio_stream::StreamExt;

use super::PublisherEngine;
use crate::publisher::{Codec, PublishRequest, Quality, Resolution};
use crate::screen_capture::{ScreenSource, SourceKind};

type Candidates = Arc<Mutex<Vec<(String, String, i32)>>>;

fn candidates(peer: &PeerConnection) -> Candidates {
    let store = Arc::new(Mutex::new(Vec::new()));
    let sink = store.clone();
    peer.on_ice_candidate(Some(Box::new(move |ice| {
        sink.lock()
            .unwrap()
            .push((ice.to_string(), ice.sdp_mid(), ice.sdp_mline_index()));
    })));
    store
}

async fn drain_to_sfu(store: &Candidates, sfu: &PeerConnection) {
    let items = std::mem::take(&mut *store.lock().unwrap());
    for (text, mid, index) in items {
        if let Ok(candidate) = IceCandidate::parse(&mid, index, &text) {
            let _ = sfu.add_ice_candidate(candidate).await;
        }
    }
}

fn drain_to_engine(store: &Candidates, engine: &PublisherEngine) {
    let items = std::mem::take(&mut *store.lock().unwrap());
    for (candidate, sdp_mid, sdp_mline_index) in items {
        let _ = engine.ice(crate::Candidate {
            candidate,
            sdp_mid: Some(sdp_mid).filter(|mid| !mid.is_empty()),
            sdp_mline_index: u16::try_from(sdp_mline_index).ok(),
            username_fragment: None,
        });
    }
}

/// The first `a=mid:` of an SDP, which is the offered video m-line here.
fn first_mid(sdp: &str) -> Option<String> {
    sdp.replace("\r\n", "\n")
        .lines()
        .find_map(|line| line.strip_prefix("a=mid:").map(str::to_owned))
}

fn section<'a>(sdp: &'a str, prefix: &str) -> Option<&'a str> {
    let start = sdp.find(prefix)?;
    let rest = &sdp[start..];
    let end = rest.find("\r\nm=").or_else(|| rest.find("\nm="));
    Some(match end {
        Some(end) => &rest[..end],
        None => rest,
    })
}

fn request() -> PublishRequest {
    PublishRequest {
        source: ScreenSource {
            id: 0,
            kind: SourceKind::Monitor,
            title: "synthetic".into(),
            display_id: -1,
            width: 1280,
            height: 720,
        },
        quality: Quality {
            resolution: Resolution::P720,
            fps: 30,
            bitrate_kbps: 2500,
            audio_bitrate_kbps: 128,
        },
        codec: Codec::Vp8,
        audio: false,
        audio_scope: crate::system_audio::AudioScope::Device,
    }
}

#[test]
fn the_publisher_answer_keeps_the_offered_mid_and_sends_frames() {
    loopback_with_frame_size((1280, 720));
}

/// A 21:9 monitor inside a 720p publication is encoded at 1280×540. The
/// declared source resolution stays 720p, so this proves the encoder accepts
/// a frame size that differs from the advertised quality.
#[test]
fn a_smaller_21_by_9_frame_still_reaches_the_sfu() {
    loopback_with_frame_size((1280, 540));
}

fn loopback_with_frame_size(frame_size: (u32, u32)) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let factory = PeerConnectionFactory::default();
        let sfu = factory
            .create_peer_connection(Default::default())
            .expect("sfu peer");
        // One recvonly video transceiver, like the backend's SDR rendition.
        sfu.add_transceiver_for_media(
            MediaType::Video,
            RtpTransceiverInit {
                direction: RtpTransceiverDirection::RecvOnly,
                stream_ids: Vec::new(),
                send_encodings: Vec::new(),
            },
        )
        .expect("recvonly transceiver");
        // `OfferOptions::default()` sets offerToReceiveVideo=Some(false), which
        // would turn the recvonly m-line inactive; the SFU keeps directions.
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
        let offer_sdp = offer.to_string();
        assert_eq!(offer_sdp.matches("m=video").count(), 1);
        assert!(
            section(&offer_sdp, "m=video")
                .unwrap_or_default()
                .contains("a=recvonly"),
            "the fake SFU must offer a recvonly video m-line"
        );
        let offer_mid = first_mid(&offer_sdp).expect("offered mid");

        let frames = Arc::new(AtomicU64::new(0));
        let track_slot: Arc<Mutex<Option<RtcVideoTrack>>> = Arc::new(Mutex::new(None));
        let track_sink = track_slot.clone();
        sfu.on_track(Some(Box::new(move |event| {
            if let MediaStreamTrack::Video(track) = event.track {
                *track_sink.lock().unwrap() = Some(track);
            }
        })));
        let sfu_candidates = candidates(&sfu);

        let engine = PublisherEngine::start_synthetic(
            request(),
            Vec::new(),
            "all".into(),
            false,
            frame_size,
            None,
        )
        .expect("publisher engine");
        engine.offer(offer_sdp.clone()).expect("offer command");

        let mut answer: Option<String> = None;
        let mut remote_answer_set = false;
        let mut reader = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            while let Some(event) = engine.poll() {
                match event {
                    crate::publisher::EngineEvent::Answer(sdp) => answer = Some(sdp),
                    crate::publisher::EngineEvent::Ice(candidate) => {
                        let mid = candidate.sdp_mid.clone().unwrap_or_default();
                        let index = candidate.sdp_mline_index.map(i32::from).unwrap_or(-1);
                        if let Ok(parsed) = IceCandidate::parse(&mid, index, &candidate.candidate) {
                            let _ = sfu.add_ice_candidate(parsed).await;
                        }
                    }
                    crate::publisher::EngineEvent::Error(message) => {
                        panic!("publisher engine error: {message}")
                    }
                    crate::publisher::EngineEvent::Closed => panic!("publisher engine closed"),
                    _ => {}
                }
            }
            drain_to_sfu(&sfu_candidates, &sfu).await;
            drain_to_engine(&sfu_candidates, &engine);
            if !remote_answer_set {
                if let Some(sdp) = &answer {
                    // The answer must keep the offered mid and be sendonly: a
                    // wrong transceiver binding produces recvonly/inactive or a
                    // different mid, and the SFU would never see a track.
                    assert_eq!(first_mid(sdp).as_deref(), Some(offer_mid.as_str()));
                    let video = section(sdp, "m=video").expect("video section");
                    assert!(
                        video.contains("a=sendonly"),
                        "the answer m-line is not sendonly: {video}"
                    );
                    assert!(!video.contains("a=inactive"), "{video}");
                    let parsed =
                        SessionDescription::parse(sdp, SdpType::Answer).expect("answer sdp");
                    sfu.set_remote_description(parsed)
                        .await
                        .expect("sfu remote answer");
                    remote_answer_set = true;
                }
            }
            if reader.is_none() && remote_answer_set {
                if let Some(track) = track_slot.lock().unwrap().clone() {
                    let counter = frames.clone();
                    reader = Some(tokio::spawn(async move {
                        let mut stream = NativeVideoStream::new(track);
                        while stream.next().await.is_some() {
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                }
            }
            if remote_answer_set && frames.load(Ordering::SeqCst) > 0 {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }

        let decoded = frames.load(Ordering::SeqCst);
        let inbound = inbound_video_packets(&sfu).await;
        println!(
            "loopback: answer_mid={:?} decoded_frames={decoded} inbound_packets={inbound}",
            answer.as_deref().and_then(first_mid)
        );
        assert!(answer.is_some(), "the publisher never produced an answer");
        assert!(
            remote_answer_set,
            "the SFU never applied the publisher answer"
        );
        assert!(
            inbound > 0,
            "no RTP packet reached the SFU: the transceiver binding is broken"
        );
        assert!(
            decoded > 0,
            "on_track fired but no frame was decoded on the SFU"
        );

        if let Some(reader) = reader {
            reader.abort();
            let _ = reader.await;
        }
        drop(engine);
        sfu.close();
    });
}

async fn inbound_video_packets(sfu: &PeerConnection) -> u64 {
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
