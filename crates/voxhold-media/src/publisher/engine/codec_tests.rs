//! C03/C05: the four SDR codecs over a real publisher loopback.
//!
//! A fake SFU creates the offer the backend creates: one recvonly video m-line
//! whose codec preferences are limited to the SDR rendition (plus RTX) and,
//! optionally, one recvonly Opus m-line after it. The real `PublisherEngine`
//! answers; the SFU decodes the result and its statistics name the codec that
//! really arrived. The publisher proves the same codec through its own outbound
//! statistics. A frozen or single-colour stream would fail the "changing
//! frames" check, so both the encoder and the decoder are exercised.
//!
//! C05 reuses the same harness: a live bitrate change is applied without a new
//! start, then a real ICE restart re-offers the same publication. The codec,
//! mid, RTX pair and limit must survive; frames keep arriving.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use libwebrtc::{
    MediaType,
    audio_stream::native::NativeAudioStream,
    ice_candidate::IceCandidate,
    media_stream_track::MediaStreamTrack,
    peer_connection::{OfferOptions, PeerConnection},
    peer_connection_factory::PeerConnectionFactory,
    rtp_transceiver::{RtpTransceiverDirection, RtpTransceiverInit},
    session_description::{SdpType, SessionDescription},
    stats::RtcStats,
    video_frame::BoxVideoFrame,
    video_stream::native::NativeVideoStream,
    video_track::RtcVideoTrack,
};
use tokio::time::sleep;
use tokio_stream::StreamExt;

use super::{BitrateSend, PublisherEngine, negotiation};
use crate::publisher::{
    Codec, EngineEvent, H264Baseline, PublishRequest, Quality, Resolution, SdrProfile,
};
use crate::screen_capture::{ScreenSource, SourceKind};
use crate::system_audio::AudioProducer;

type Candidates = Arc<Mutex<Vec<(String, String, i32)>>>;

pub(super) fn candidates(peer: &PeerConnection) -> Candidates {
    let store = Arc::new(Mutex::new(Vec::new()));
    let sink = store.clone();
    peer.on_ice_candidate(Some(Box::new(move |ice| {
        sink.lock()
            .unwrap()
            .push((ice.to_string(), ice.sdp_mid(), ice.sdp_mline_index()));
    })));
    store
}

pub(super) async fn drain(store: &Candidates, peer: &PeerConnection) {
    let items = std::mem::take(&mut *store.lock().unwrap());
    for (text, mid, index) in items {
        if let Ok(candidate) = IceCandidate::parse(&mid, index, &text) {
            let _ = peer.add_ice_candidate(candidate).await;
        }
    }
}

pub(super) fn drain_to_engine(store: &Candidates, engine: &PublisherEngine) {
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

/// The text of the `m=<media>` section, newline-normalized.
fn section(sdp: &str, media: &str) -> Option<String> {
    let normalized = sdp.replace("\r\n", "\n");
    let mut lines: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in normalized.lines() {
        if let Some(rest) = line.strip_prefix("m=") {
            if inside {
                break;
            }
            inside = rest.starts_with(&format!("{media} "));
            if inside {
                lines.push(line);
            }
            continue;
        }
        if inside {
            lines.push(line);
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

pub(super) fn media_mid(sdp: &str, media: &str) -> Option<String> {
    section(sdp, media)?
        .lines()
        .find_map(|line| line.strip_prefix("a=mid:").map(str::to_owned))
}

pub(super) fn ice_ufrag(sdp: &str) -> Option<String> {
    sdp.replace("\r\n", "\n")
        .lines()
        .find_map(|line| line.strip_prefix("a=ice-ufrag:").map(str::to_owned))
}

/// The requested SDR codec of a test publication.
pub(super) fn request(codec: Codec, audio: bool) -> PublishRequest {
    PublishRequest {
        source: ScreenSource {
            id: 0,
            kind: SourceKind::Monitor,
            title: "synthetic".into(),
            display_id: -1,
            width: 640,
            height: 360,
            monitor: None,
        },
        quality: Quality {
            resolution: Resolution::P720,
            fps: 30,
            bitrate_kbps: 1200,
            audio_bitrate_kbps: 128,
        },
        codec,
        audio,
        audio_scope: crate::system_audio::AudioScope::Device,
    }
}

/// Nothing is playing; the audio loop still emits silent 10 ms frames, which
/// proves the Opus track separately from the video codec.
struct SilentAudio;

impl AudioProducer for SilentAudio {
    fn next_block(&mut self) -> Option<Vec<i16>> {
        None
    }

    fn failure(&self) -> Option<String> {
        None
    }

    fn dropped(&self) -> u64 {
        0
    }
}

/// The SDP MIME subtype of a codec (`video/H264` -> `H264`).
fn sdp_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H264",
        Codec::Vp8 => "VP8",
        Codec::Vp9 => "VP9",
        Codec::Av1 => "AV1",
    }
}

/// The fake SFU creates the offer the backend creates: one recvonly video
/// m-line limited to `codec` (plus RTX), then one recvonly audio m-line when
/// asked. The H.264 `baseline` rendition of the backend registers Constrained
/// Baseline *and* the legacy `42001f` payload on separate types, so the offer
/// carries both; the answer must pick the preferred one.
fn sfu_peer(factory: &PeerConnectionFactory, codec: Codec, with_audio: bool) -> PeerConnection {
    let sfu = factory
        .create_peer_connection(Default::default())
        .expect("sfu peer");
    let profile = negotiation::sender_profile(codec).expect("checked sender profile");
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
    let mut preferences = negotiation::preferences(&capabilities.codecs, codec, profile);
    if codec == Codec::H264 {
        let legacy = SdrProfile::H264 {
            baseline: H264Baseline::Legacy,
            level: 0x1f,
        };
        let legacy_primary = negotiation::preferences(&capabilities.codecs, codec, legacy)
            .into_iter()
            .next()
            .expect("the build declares the legacy H.264 payload");
        preferences.insert(1, legacy_primary);
    }
    video
        .set_codec_preferences(preferences)
        .expect("the SFU pins the rendition codec");
    if with_audio {
        sfu.add_transceiver_for_media(
            MediaType::Audio,
            RtpTransceiverInit {
                direction: RtpTransceiverDirection::RecvOnly,
                stream_ids: Vec::new(),
                send_encodings: Vec::new(),
            },
        )
        .expect("audio transceiver");
    }
    sfu
}

/// A checksum of one decoded frame. Any plane the decoder produced is enough
/// to tell two pictures apart.
fn frame_hash(frame: &BoxVideoFrame) -> u64 {
    if let Some(buffer) = frame.buffer.as_i420() {
        let (y, _, _) = buffer.data();
        return checksum(y);
    }
    if let Some(buffer) = frame.buffer.as_nv12() {
        let (y, _) = buffer.data();
        return checksum(y);
    }
    if let Some(buffer) = frame.buffer.as_i444() {
        let (y, _, _) = buffer.data();
        return checksum(y);
    }
    if let Some(buffer) = frame.buffer.as_i422() {
        let (y, _, _) = buffer.data();
        return checksum(y);
    }
    0
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes.iter().step_by(3) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One run of the real publisher against the fake SFU.
struct Run {
    offer: String,
    answer: String,
    offer_video_mid: String,
    inbound_codec_mime: Option<String>,
    confirmed_codec: Option<Codec>,
    decoded: Vec<u64>,
    audio_packets: u64,
    audio_frames: u64,
    connected: u32,
    answers: u32,
    answer_ufrags: Vec<String>,
    errors: Vec<String>,
    frames_at_restart: Option<u64>,
    frames_at_end: u64,
}

/// Publish `codec` through the real engine and decode it on the fake SFU.
///
/// `change` is a live bitrate-only request; when set, the run also applies it
/// and then re-offers with a real ICE restart.
async fn run(codec: Codec, with_audio: bool, change: Option<Quality>) -> Run {
    let factory = PeerConnectionFactory::default();
    let sfu = sfu_peer(&factory, codec, with_audio);
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
    let offer_video_mid = media_mid(&offer_sdp, "video").expect("offered video mid");
    let initial_ufrag = ice_ufrag(&offer_sdp);

    let video_track: Arc<Mutex<Option<RtcVideoTrack>>> = Arc::new(Mutex::new(None));
    let audio_track: Arc<Mutex<Option<libwebrtc::audio_track::RtcAudioTrack>>> =
        Arc::new(Mutex::new(None));
    let video_slot = video_track.clone();
    let audio_slot = audio_track.clone();
    sfu.on_track(Some(Box::new(move |event| match event.track {
        MediaStreamTrack::Video(track) => *video_slot.lock().unwrap() = Some(track),
        MediaStreamTrack::Audio(track) => *audio_slot.lock().unwrap() = Some(track),
    })));
    let sfu_candidates = candidates(&sfu);

    let audio: Option<Box<dyn AudioProducer>> = with_audio.then(|| Box::new(SilentAudio) as Box<_>);
    let engine = PublisherEngine::start_synthetic(
        request(codec, with_audio),
        Vec::new(),
        "all".into(),
        false,
        (640, 360),
        audio,
    )
    .expect("publisher engine");
    engine.offer(offer_sdp.clone()).expect("offer command");

    let decoded = Arc::new(Mutex::new(Vec::<u64>::new()));
    let audio_frames = Arc::new(AtomicU64::new(0));
    let frame_count = Arc::new(AtomicU64::new(0));
    let mut video_reader: Option<tokio::task::JoinHandle<()>> = None;
    let mut audio_reader: Option<tokio::task::JoinHandle<()>> = None;
    let mut answer: Option<String> = None;
    let mut answers = 0_u32;
    let mut answer_ufrags = Vec::new();
    let mut remote_answer_set = false;
    let mut errors = Vec::new();
    let mut connected = 0_u32;
    let mut confirmed_codec = None;
    let mut change_applied = false;
    let mut change_verified = false;
    let mut frames_at_restart = None;
    let budget = if change.is_some() { 30 } else { 20 };
    let deadline = Instant::now() + Duration::from_secs(budget);

    while Instant::now() < deadline {
        while let Some(event) = engine.poll() {
            match event {
                EngineEvent::Answer(sdp) => {
                    answers += 1;
                    if let Some(ufrag) = ice_ufrag(&sdp) {
                        answer_ufrags.push(ufrag);
                    }
                    answer = Some(sdp);
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
                EngineEvent::Stats(stats) => {
                    if stats.confirmed_codec.is_some() {
                        confirmed_codec = stats.confirmed_codec;
                    }
                }
                EngineEvent::Bitrate { result, .. } => {
                    change_verified = matches!(result, crate::publisher::BitrateResult::Applied);
                }
                EngineEvent::Error(message) => errors.push(message),
                EngineEvent::Closed => errors.push("engine closed".into()),
            }
        }
        if !errors.is_empty() {
            break;
        }
        drain(&sfu_candidates, &sfu).await;
        drain_to_engine(&sfu_candidates, &engine);
        if let Some(sdp) = answer.as_ref() {
            if !remote_answer_set {
                sfu.set_remote_description(
                    SessionDescription::parse(sdp, SdpType::Answer).expect("answer sdp"),
                )
                .await
                .expect("sfu remote answer");
                remote_answer_set = true;
            }
        }
        if remote_answer_set {
            if let Some(track) = video_track.lock().unwrap().clone() {
                if video_reader.is_none() {
                    let store = decoded.clone();
                    let counter = frame_count.clone();
                    video_reader = Some(tokio::spawn(async move {
                        let mut stream = NativeVideoStream::new(track);
                        while let Some(frame) = stream.next().await {
                            let hash = frame_hash(&frame);
                            let mut frames = store.lock().unwrap();
                            if frames.len() < 16 {
                                frames.push(hash);
                            }
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                }
            }
            if let Some(track) = audio_track.lock().unwrap().clone() {
                if audio_reader.is_none() {
                    let counter = audio_frames.clone();
                    audio_reader = Some(tokio::spawn(async move {
                        let mut stream = NativeAudioStream::new(
                            track,
                            crate::system_audio::SAMPLE_RATE as i32,
                            2,
                        );
                        while stream.next().await.is_some() {
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                }
            }
        }
        let decoded_len = decoded.lock().unwrap().len();
        if confirmed_codec.is_some() && decoded_len >= 3 {
            match change {
                None => break,
                Some(next) if !change_applied => {
                    assert_eq!(
                        engine.set_bitrate(1, 1, next),
                        BitrateSend::Sent,
                        "the live command must reach the worker"
                    );
                    change_applied = true;
                }
                Some(_) if !change_verified => {}
                Some(next) if frames_at_restart.is_none() => {
                    let expected = u64::from(next.bitrate_kbps) * 1000;
                    if engine.applied_bitrate_limits().0 != expected {
                        sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                    // A real ICE restart on the same m-line.
                    let restart = sfu
                        .create_offer(OfferOptions {
                            ice_restart: true,
                            offer_to_receive_audio: None,
                            offer_to_receive_video: None,
                        })
                        .await
                        .expect("ICE restart offer");
                    let restart_sdp = restart.to_string();
                    assert_ne!(
                        ice_ufrag(&restart_sdp),
                        initial_ufrag,
                        "the re-offer is not an ICE restart"
                    );
                    assert_eq!(
                        media_mid(&restart_sdp, "video").as_deref(),
                        Some(offer_video_mid.as_str()),
                        "the ICE restart moved the video m-line"
                    );
                    sfu.set_local_description(restart)
                        .await
                        .expect("sfu restart local offer");
                    engine.offer(restart_sdp).expect("restart offer command");
                    frames_at_restart = Some(frame_count.load(Ordering::SeqCst));
                }
                Some(next) if answers >= 2 => {
                    let expected = u64::from(next.bitrate_kbps) * 1000;
                    if frame_count.load(Ordering::SeqCst) > frames_at_restart.unwrap_or(0)
                        && engine.applied_bitrate_limits().0 == expected
                    {
                        break;
                    }
                }
                Some(_) => {}
            }
        }
        sleep(Duration::from_millis(20)).await;
    }

    let inbound_codec_mime = inbound_codec(&sfu, &offer_video_mid).await;
    let audio_packets = audio_packets(&sfu, with_audio).await;
    let decoded = decoded.lock().unwrap().clone();
    let audio_frames = audio_frames.load(Ordering::SeqCst);
    let frames_at_end = frame_count.load(Ordering::SeqCst);
    if let Some(reader) = video_reader {
        reader.abort();
        let _ = reader.await;
    }
    if let Some(reader) = audio_reader {
        reader.abort();
        let _ = reader.await;
    }
    drop(engine);
    sfu.close();
    Run {
        offer: offer_sdp,
        answer: answer.unwrap_or_default(),
        offer_video_mid,
        inbound_codec_mime,
        confirmed_codec,
        decoded,
        audio_packets,
        audio_frames,
        connected,
        answers,
        answer_ufrags,
        errors,
        frames_at_restart,
        frames_at_end,
    }
}

async fn audio_packets(sfu: &PeerConnection, with_audio: bool) -> u64 {
    if !with_audio {
        return 0;
    }
    let Ok(stats) = sfu.get_stats().await else {
        return 0;
    };
    stats
        .into_iter()
        .filter_map(|stat| match stat {
            RtcStats::InboundRtp(inbound) if inbound.stream.kind == "audio" => {
                Some(inbound.received.packets_received)
            }
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// The codec of the video stream that really reached the SFU, joined through
/// `InboundRtpStats.stream.codec_id` and `RtcStats::Codec`.
async fn inbound_codec(sfu: &PeerConnection, video_mid: &str) -> Option<String> {
    let stats = sfu.get_stats().await.ok()?;
    let mut codecs: BTreeMap<String, String> = BTreeMap::new();
    let mut codec_id = None;
    for stat in stats {
        match stat {
            RtcStats::Codec(codec) => {
                codecs.insert(codec.rtc.id, codec.codec.mime_type);
            }
            RtcStats::InboundRtp(inbound)
                if inbound.stream.kind == "video" && inbound.inbound.mid == video_mid =>
            {
                codec_id = Some(inbound.stream.codec_id.clone());
            }
            _ => {}
        }
    }
    codecs.remove(&codec_id?)
}

/// Prove the negotiated SDP: the offer really was limited to the codec, and the
/// answer kept the requested codec, mid and RTX `apt` pair.
fn verify_sdp(codec: Codec, with_audio: bool, run: &Run) {
    let offer_video = section(&run.offer, "video").expect("offered video section");
    let offer_payloads = if codec == Codec::H264 {
        // Constrained Baseline and the legacy payload, each with its RTX.
        4
    } else {
        2
    };
    assert_eq!(
        offer_video.matches("a=rtpmap:").count(),
        offer_payloads,
        "the fake SFU offers {} for {}:\n{offer_video}",
        sdp_name(codec),
        offer_payloads / 2
    );
    assert!(
        offer_video.contains(&format!(" {}/90000", sdp_name(codec))),
        "{offer_video}"
    );
    if codec == Codec::H264 {
        assert!(
            offer_video.contains("profile-level-id=42e01f")
                && offer_video.contains("profile-level-id=42001f"),
            "the backend's H.264 rendition offers both SDR baselines:\n{offer_video}"
        );
    }
    let offer_primary = payload_named(&offer_video, sdp_name(codec))
        .expect("the offered video section names the codec");
    let answer_video = section(&run.answer, "video").expect("answer video section");
    assert_eq!(
        media_mid(&run.answer, "video").as_deref(),
        Some(run.offer_video_mid.as_str()),
        "the answer keeps the offered video mid:\n{answer_video}"
    );
    assert!(
        answer_video.contains("a=sendonly"),
        "the answer sends the screen:\n{answer_video}"
    );
    assert!(!answer_video.contains("a=inactive"), "{answer_video}");
    assert_eq!(
        answer_video.matches("a=rtpmap:").count(),
        2,
        "the answer carries exactly {} and its RTX:\n{answer_video}",
        sdp_name(codec)
    );
    let primary = payload_named(&answer_video, sdp_name(codec))
        .expect("the answer names the requested codec");
    let rtx = payload_named(&answer_video, "rtx").expect("the answer carries an RTX payload");
    // The answer picked the locally preferred SDR profile, not merely any
    // payload of the codec: for H.264 the Constrained Baseline one.
    let answer_fmtp = answer_video
        .lines()
        .find_map(|line| line.strip_prefix(&format!("a=fmtp:{primary} ")))
        .unwrap_or_default();
    let answer_profile = crate::publisher::sdr_profile(codec, Some(answer_fmtp))
        .expect("the answer payload describes an SDR profile");
    if codec == Codec::H264 {
        assert!(
            matches!(
                answer_profile,
                SdrProfile::H264 {
                    baseline: H264Baseline::Constrained,
                    ..
                }
            ),
            "the answer chose the legacy H.264 payload: {answer_fmtp}"
        );
    }
    assert!(
        answer_video
            .lines()
            .any(|line| line == format!("a=fmtp:{rtx} apt={primary}")),
        "the answer pairs RTX with the primary payload through apt:\n{answer_video}"
    );
    // The negotiated feedback of the primary payload is preserved: the answer
    // keeps every rtcp-fb line the offer listed for it.
    let offered_feedback: Vec<&str> = offer_video
        .lines()
        .filter_map(|line| line.strip_prefix(&format!("a=rtcp-fb:{offer_primary} ")))
        .collect();
    assert!(
        !offered_feedback.is_empty(),
        "the offered payload carries feedback:\n{offer_video}"
    );
    for feedback in offered_feedback {
        assert!(
            answer_video
                .lines()
                .any(|line| line == format!("a=rtcp-fb:{primary} {feedback}")),
            "the answer dropped the `{feedback}` feedback of {primary}:\n{answer_video}"
        );
    }
    if with_audio {
        let audio = section(&run.answer, "audio").expect("answer audio section");
        assert!(audio.contains("a=sendonly"), "{audio}");
        assert!(audio.to_ascii_lowercase().contains("opus/48000"), "{audio}");
        let offer_audio = section(&run.offer, "audio").expect("offered audio section");
        assert_eq!(
            media_mid(&offer_audio, "audio"),
            media_mid(&run.answer, "audio"),
            "the audio track binds to the offered audio m-line"
        );
        assert!(
            run.audio_packets > 0,
            "the separate Opus track reached the SFU"
        );
        assert!(
            run.audio_frames >= 3,
            "the SFU decoded {} audio frames",
            run.audio_frames
        );
    }
}

/// The payload type of `a=rtpmap:<pt> <name>/90000`.
fn payload_named(section: &str, name: &str) -> Option<String> {
    section.lines().find_map(|line| {
        let rest = line.strip_prefix("a=rtpmap:")?;
        let (pt, encoding) = rest.split_once(' ')?;
        encoding.starts_with(name).then(|| pt.to_owned())
    })
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("codec test runtime")
        .block_on(future)
}

#[test]
fn h264_publishes_constrained_baseline_and_decodes_changing_frames() {
    check_codec(Codec::H264);
}

#[test]
fn vp8_publishes_and_decodes_changing_frames() {
    check_codec(Codec::Vp8);
}

#[test]
fn vp9_publishes_profile_zero_and_decodes_changing_frames() {
    check_codec(Codec::Vp9);
}

#[test]
fn av1_publishes_main_and_decodes_changing_frames() {
    check_codec(Codec::Av1);
}

fn check_codec(codec: Codec) {
    block_on(async {
        let run = run(codec, true, None).await;
        assert!(
            run.errors.is_empty(),
            "{codec:?} failed the real publication: {:?}",
            run.errors
        );
        verify_sdp(codec, true, &run);
        assert_eq!(run.answers, 1, "one offer needs exactly one answer");
        assert_eq!(
            run.confirmed_codec,
            Some(codec),
            "the publisher statistics must confirm the requested codec"
        );
        assert_eq!(
            run.connected, 1,
            "the transmission is confirmed once by frames and statistics"
        );
        let inbound = run
            .inbound_codec_mime
            .as_deref()
            .expect("the SFU names the codec it received");
        assert_eq!(
            Codec::parse(inbound),
            Some(codec),
            "the SFU decoded {inbound} instead of {}",
            codec.label()
        );
        let distinct: std::collections::BTreeSet<u64> = run.decoded.iter().copied().collect();
        println!(
            "{codec:?}: decoded {} frames ({} distinct), inbound {inbound}, audio packets {}",
            run.decoded.len(),
            distinct.len(),
            run.audio_packets
        );
        assert!(
            run.decoded.len() >= 3,
            "{codec:?} decoded only {} frames",
            run.decoded.len()
        );
        assert!(
            distinct.len() >= 2,
            "{codec:?} decoded {} identical frames: the screen is frozen",
            run.decoded.len()
        );
    });
}

/// C05: a live bitrate change reaches the sender without a new start, an ICE
/// restart keeps the codec, mid and limit, and frames continue afterwards.
#[test]
fn a_live_bitrate_change_survives_an_ice_restart_without_losing_the_codec() {
    block_on(async {
        let next = Quality {
            bitrate_kbps: 700,
            audio_bitrate_kbps: 96,
            ..request(Codec::H264, true).quality
        };
        let run = run(Codec::H264, true, Some(next)).await;
        assert!(run.errors.is_empty(), "{:?}", run.errors);
        assert_eq!(
            run.answers, 2,
            "the ICE restart needs exactly one new answer"
        );
        assert_eq!(run.answer_ufrags.len(), 2);
        assert_ne!(
            run.answer_ufrags[0], run.answer_ufrags[1],
            "the answer of the restart must restart ICE"
        );
        assert_eq!(
            run.confirmed_codec,
            Some(Codec::H264),
            "the restart kept the codec"
        );
        assert!(run.frames_at_restart.is_some(), "the restart happened");
        assert!(
            run.frames_at_end > run.frames_at_restart.unwrap_or(0),
            "frames did not continue after the restart"
        );
        verify_sdp(Codec::H264, true, &run);
    });
}
