//! In-process proof of the system-audio track.
//!
//! A fake SFU offers one recvonly video m-line and, when `has_audio` is true,
//! one recvonly audio m-line after it — exactly the order of the backend. The
//! real `PublisherEngine` answers. Deterministic producers replace WASAPI, so
//! the tests prove the Opus/RTP path, the m-line binding, two-tone fidelity,
//! silence continuity and the separation from a microphone track without a
//! device or a server.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use libwebrtc::{
    MediaType,
    audio_frame::AudioFrame,
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    audio_stream::native::NativeAudioStream,
    ice_candidate::IceCandidate,
    media_stream_track::MediaStreamTrack,
    peer_connection::{OfferOptions, PeerConnection, PeerConnectionState},
    peer_connection_factory::{PeerConnectionFactory, native::PeerConnectionFactoryExt},
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
use crate::system_audio::{AudioProducer, BLOCK_FRAMES, BLOCK_SAMPLES, CHANNELS, SAMPLE_RATE};

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

async fn apply_candidates(store: &Candidates, peer: &PeerConnection) {
    let items = std::mem::take(&mut *store.lock().unwrap());
    for (text, mid, index) in items {
        if let Ok(candidate) = IceCandidate::parse(&mid, index, &text) {
            let _ = peer.add_ice_candidate(candidate).await;
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

/// The mid of the m-line whose section starts with `m=video` / `m=audio`.
fn media_mid(sdp: &str, media: &str) -> Option<String> {
    let section = section(sdp, &format!("m={media}"))?;
    section
        .replace("\r\n", "\n")
        .lines()
        .find_map(|line| line.strip_prefix("a=mid:").map(str::to_owned))
}

fn request(audio: bool) -> PublishRequest {
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
        audio,
    }
}

/// Two sine tones mixed into one stereo signal. The phase is carried block to
/// block, so the frequency at the receiver is exact.
struct ToneProducer {
    frequencies: Vec<f64>,
    phases: Vec<f64>,
}

impl ToneProducer {
    fn new(frequencies: &[f64]) -> Self {
        Self {
            frequencies: frequencies.to_vec(),
            phases: vec![0.0; frequencies.len()],
        }
    }
}

impl AudioProducer for ToneProducer {
    fn next_block(&mut self) -> Option<Vec<i16>> {
        let mut data = vec![0_i16; BLOCK_SAMPLES];
        let amplitude = 0.25 / self.frequencies.len().max(1) as f64;
        for frame in 0..BLOCK_FRAMES as usize {
            let mut value = 0.0;
            for (index, frequency) in self.frequencies.iter().enumerate() {
                value += (self.phases[index].sin()) * amplitude;
                self.phases[index] +=
                    2.0 * std::f64::consts::PI * frequency / f64::from(SAMPLE_RATE);
            }
            let sample = (value * 32_767.0).clamp(-32_768.0, 32_767.0) as i16;
            for channel in 0..CHANNELS as usize {
                data[frame * CHANNELS as usize + channel] = sample;
            }
        }
        Some(data)
    }

    fn failure(&self) -> Option<String> {
        None
    }

    fn dropped(&self) -> u64 {
        0
    }
}

/// Nothing is playing: the engine must emit silence, not stop the track.
struct SilentProducer;

impl AudioProducer for SilentProducer {
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

struct Run {
    answer: String,
    offer_video_mid: String,
    offer_audio_mid: Option<String>,
    audio_track_id: Option<String>,
    video_track_id: Option<String>,
    decoded: Vec<i16>,
    decoded_frames: usize,
    audio_packets_first: u64,
    audio_packets_last: u64,
    video_frames: u64,
}

async fn run(
    audio: Option<Box<dyn AudioProducer>>,
    with_audio_line: bool,
    duration: Duration,
) -> Result<Run, String> {
    let factory = PeerConnectionFactory::default();
    let sfu = factory
        .create_peer_connection(Default::default())
        .map_err(|_| "sfu peer")?;
    sfu.add_transceiver_for_media(
        MediaType::Video,
        RtpTransceiverInit {
            direction: RtpTransceiverDirection::RecvOnly,
            stream_ids: Vec::new(),
            send_encodings: Vec::new(),
        },
    )
    .map_err(|_| "video transceiver")?;
    if with_audio_line {
        sfu.add_transceiver_for_media(
            MediaType::Audio,
            RtpTransceiverInit {
                direction: RtpTransceiverDirection::RecvOnly,
                stream_ids: Vec::new(),
                send_encodings: Vec::new(),
            },
        )
        .map_err(|_| "audio transceiver")?;
    }
    let offer = sfu
        .create_offer(OfferOptions {
            offer_to_receive_audio: None,
            offer_to_receive_video: None,
            ..OfferOptions::default()
        })
        .await
        .map_err(|_| "offer")?;
    sfu.set_local_description(offer.clone())
        .await
        .map_err(|_| "sfu local offer")?;
    let offer_sdp = offer.to_string();
    let offer_video_mid = media_mid(&offer_sdp, "video").ok_or("offered video mid")?;
    let offer_audio_mid = media_mid(&offer_sdp, "audio");

    let audio_track: Arc<Mutex<Option<libwebrtc::audio_track::RtcAudioTrack>>> =
        Arc::new(Mutex::new(None));
    let video_track: Arc<Mutex<Option<RtcVideoTrack>>> = Arc::new(Mutex::new(None));
    let audio_slot = audio_track.clone();
    let video_slot = video_track.clone();
    sfu.on_track(Some(Box::new(move |event| match event.track {
        MediaStreamTrack::Audio(track) => *audio_slot.lock().unwrap() = Some(track),
        MediaStreamTrack::Video(track) => *video_slot.lock().unwrap() = Some(track),
    })));
    let sfu_candidates = candidates(&sfu);

    let engine = PublisherEngine::start_synthetic(
        request(audio.is_some()),
        Vec::new(),
        "all".into(),
        false,
        (320, 180),
        audio,
    )
    .map_err(|error| format!("publisher engine: {error}"))?;
    engine
        .offer(offer_sdp.clone())
        .map_err(|_| "offer command")?;

    let decoded = Arc::new(Mutex::new(Vec::<i16>::new()));
    let decoded_frames = Arc::new(AtomicU64::new(0));
    let video_frames = Arc::new(AtomicU64::new(0));
    let mut audio_reader: Option<tokio::task::JoinHandle<()>> = None;
    let mut video_reader: Option<tokio::task::JoinHandle<()>> = None;
    let mut answer: Option<String> = None;
    let mut remote_answer_set = false;
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut audio_packets_first = 0;
    let mut audio_packets_last = 0;
    let mut measured_at: Option<Instant> = None;

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
                    return Err(format!("publisher engine error: {message}"));
                }
                crate::publisher::EngineEvent::Closed => {
                    return Err("publisher engine closed".into());
                }
                _ => {}
            }
        }
        apply_candidates(&sfu_candidates, &sfu).await;
        drain_to_engine(&sfu_candidates, &engine);
        if !remote_answer_set {
            if let Some(sdp) = &answer {
                let parsed =
                    SessionDescription::parse(sdp, SdpType::Answer).map_err(|_| "answer")?;
                sfu.set_remote_description(parsed)
                    .await
                    .map_err(|_| "sfu remote answer")?;
                remote_answer_set = true;
            }
        }
        if remote_answer_set {
            if let Some(track) = audio_track.lock().unwrap().clone() {
                if audio_reader.is_none() {
                    let store = decoded.clone();
                    let counter = decoded_frames.clone();
                    audio_reader = Some(tokio::spawn(async move {
                        let mut stream = NativeAudioStream::new(track, SAMPLE_RATE as i32, 2);
                        while let Some(frame) = stream.next().await {
                            let channels = frame.num_channels as usize;
                            let mut samples = store.lock().unwrap();
                            for sample in frame.data.chunks(channels.max(1)) {
                                samples.push(sample[0]);
                            }
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                }
            }
            if let Some(track) = video_track.lock().unwrap().clone() {
                if video_reader.is_none() {
                    let counter = video_frames.clone();
                    video_reader = Some(tokio::spawn(async move {
                        let mut stream = NativeVideoStream::new(track);
                        while stream.next().await.is_some() {
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                    }));
                }
            }
        }
        if remote_answer_set {
            let elapsed = measured_at.get_or_insert_with(Instant::now).elapsed();
            if elapsed >= duration / 2 {
                if which_packets(&sfu).await > 0 && audio_packets_first == 0 {
                    audio_packets_first = which_packets(&sfu).await;
                }
            }
            if elapsed >= duration {
                audio_packets_last = which_packets(&sfu).await;
                break;
            }
        }
        sleep(Duration::from_millis(20)).await;
    }

    let answer = answer.ok_or("the publisher never produced an answer")?;
    assert!(
        remote_answer_set,
        "the fake SFU never applied the publisher answer"
    );
    let audio_track_id = audio_track.lock().unwrap().as_ref().map(|track| track.id());
    let video_track_id = video_track.lock().unwrap().as_ref().map(|track| track.id());
    if let Some(reader) = audio_reader {
        reader.abort();
        let _ = reader.await;
    }
    if let Some(reader) = video_reader {
        reader.abort();
        let _ = reader.await;
    }
    if audio_packets_first == 0 {
        audio_packets_first = audio_packets_last;
    }
    let decoded = decoded.lock().unwrap().clone();
    let decoded_frames = decoded_frames.load(Ordering::SeqCst) as usize;
    let video_frames = video_frames.load(Ordering::SeqCst);
    drop(engine);
    sfu.close();
    Ok(Run {
        answer,
        offer_video_mid,
        offer_audio_mid,
        audio_track_id,
        video_track_id,
        decoded,
        decoded_frames,
        audio_packets_first,
        audio_packets_last,
        video_frames,
    })
}

async fn which_packets(sfu: &PeerConnection) -> u64 {
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

fn goertzel(samples: &[i16], frequency: f64) -> f64 {
    let omega = 2.0 * std::f64::consts::PI * frequency / f64::from(SAMPLE_RATE);
    let coefficient = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for sample in samples {
        let value = f64::from(*sample) / 32_768.0;
        let s0 = value + coefficient * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coefficient * s1 * s2
}

fn peak(samples: &[i16]) -> i32 {
    samples
        .iter()
        .map(|sample| i32::from(*sample).abs())
        .max()
        .unwrap_or(0)
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("audio test runtime")
        .block_on(future)
}

#[test]
fn the_audio_m_line_follows_video_and_carries_both_tones() {
    block_on(async {
        let run = run(
            Some(Box::new(ToneProducer::new(&[400.0, 800.0]))),
            true,
            Duration::from_millis(1600),
        )
        .await
        .expect("run");
        assert!(
            run.offer_video_mid != run.offer_audio_mid.clone().unwrap_or_default(),
            "distinct offered mids"
        );
        assert_eq!(
            run.answer.matches("m=video").count(),
            1,
            "one video m-line: {}",
            run.answer
        );
        assert_eq!(
            run.answer.matches("m=audio").count(),
            1,
            "one audio m-line: {}",
            run.answer
        );
        let video = section(&run.answer, "m=video").expect("video section");
        let audio = section(&run.answer, "m=audio").expect("audio section");
        assert!(video.contains("a=sendonly"), "{video}");
        assert!(audio.contains("a=sendonly"), "{audio}");
        assert_eq!(
            first_mid(&run.answer).as_deref(),
            Some(run.offer_video_mid.as_str())
        );
        assert_eq!(
            media_mid(&run.answer, "audio"),
            run.offer_audio_mid,
            "the audio track binds to the offered audio m-line"
        );
        println!(
            "audio loopback: decoded {} frames, {} samples, audio packets {}->{}, peak {}",
            run.decoded_frames,
            run.decoded.len(),
            run.audio_packets_first,
            run.audio_packets_last,
            peak(&run.decoded)
        );
        assert!(
            run.audio_packets_last > 0,
            "the SFU received no audio RTP packet"
        );
        assert!(
            run.decoded_frames >= 70,
            "too few decoded audio frames: {}",
            run.decoded_frames
        );
        assert!(
            peak(&run.decoded) > 2000,
            "decoded audio is silent, peak {}",
            peak(&run.decoded)
        );
        let low = goertzel(&run.decoded, 400.0);
        let high = goertzel(&run.decoded, 800.0);
        println!("tones: 400 Hz {low:.1}, 800 Hz {high:.1}");
        assert!(low > 1.0, "the 400 Hz tone is missing: {low}");
        assert!(high > 1.0, "the 800 Hz tone is missing: {high}");
        let duration = run.decoded.len() as f64 / f64::from(SAMPLE_RATE);
        assert!(
            (0.7..=3.0).contains(&duration),
            "decoded duration {duration:.2}s does not match the played 1.6s"
        );
        assert!(
            run.video_frames > 0,
            "the video track must still reach the same SFU"
        );
        assert!(run.audio_track_id.is_some());
        assert_ne!(run.audio_track_id, run.video_track_id);
    });
}

#[test]
fn without_audio_the_answer_has_no_audio_m_line() {
    block_on(async {
        let run = run(None, false, Duration::from_millis(700))
            .await
            .expect("run");
        assert_eq!(run.answer.matches("m=video").count(), 1);
        assert_eq!(
            run.answer.matches("m=audio").count(),
            0,
            "has_audio:false must not create an audio m-line: {}",
            run.answer
        );
        assert_eq!(
            first_mid(&run.answer).as_deref(),
            Some(run.offer_video_mid.as_str())
        );
        assert!(run.audio_track_id.is_none());
        assert_eq!(run.audio_packets_last, 0);
        assert!(run.video_frames > 0);
    });
}

#[test]
fn silence_keeps_the_audio_track_flowing() {
    block_on(async {
        let run = run(
            Some(Box::new(SilentProducer)),
            true,
            Duration::from_millis(1800),
        )
        .await
        .expect("run");
        println!(
            "silence: decoded {} frames, packets {}->{}",
            run.decoded_frames, run.audio_packets_first, run.audio_packets_last
        );
        assert!(
            run.audio_packets_last > run.audio_packets_first,
            "audio packets must keep growing during silence: {} -> {}",
            run.audio_packets_first,
            run.audio_packets_last
        );
        assert!(
            run.decoded_frames >= 100,
            "the decoder must keep producing frames: {}",
            run.decoded_frames
        );
        assert!(
            peak(&run.decoded) < 500,
            "the silent source must stay silent"
        );
    });
}

#[test]
fn the_system_audio_and_a_microphone_are_different_tracks() {
    block_on(async {
        let system = run(
            Some(Box::new(ToneProducer::new(&[400.0]))),
            true,
            Duration::from_millis(1200),
        )
        .await
        .expect("system audio run");
        let microphone = microphone_run(2000.0, Duration::from_millis(1200))
            .await
            .expect("microphone run");
        let system_id = system.audio_track_id.clone().expect("system audio track");
        assert_ne!(
            system_id, microphone.track_id,
            "the system audio and the microphone must never share a track"
        );
        let system_low = goertzel(&system.decoded, 400.0);
        let system_mic = goertzel(&system.decoded, 2000.0);
        let mic_low = goertzel(&microphone.decoded, 400.0);
        let mic_high = goertzel(&microphone.decoded, 2000.0);
        println!(
            "separation: system 400 {system_low:.1} / 2000 {system_mic:.1}; mic 400 {mic_low:.1} / 2000 {mic_high:.1}"
        );
        assert!(
            system_low > system_mic * 4.0,
            "the system track must not contain the microphone tone"
        );
        assert!(
            mic_high > mic_low * 4.0,
            "the microphone track must not contain the system tone"
        );
    });
}

struct MicrophoneRun {
    track_id: String,
    decoded: Vec<i16>,
}

/// A plain sender/receiver pair like the voice peer: one microphone track,
/// no screen engine. Proves the two paths cannot bleed into each other.
async fn microphone_run(frequency: f64, duration: Duration) -> Result<MicrophoneRun, String> {
    let factory = PeerConnectionFactory::default();
    let sender = factory
        .create_peer_connection(Default::default())
        .map_err(|_| "mic sender")?;
    let receiver = factory
        .create_peer_connection(Default::default())
        .map_err(|_| "mic receiver")?;
    let source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: false,
            noise_suppression: false,
            auto_gain_control: false,
        },
        SAMPLE_RATE,
        2,
        200,
    );
    let track = factory.create_audio_track("voxhold-microphone", source.clone());
    let track_id = track.id();
    sender
        .add_track(MediaStreamTrack::Audio(track), &["voxhold-voice"])
        .map_err(|_| "mic add track")?;
    let remote: Arc<Mutex<Option<libwebrtc::audio_track::RtcAudioTrack>>> =
        Arc::new(Mutex::new(None));
    let slot = remote.clone();
    receiver.on_track(Some(Box::new(move |event| {
        if let MediaStreamTrack::Audio(track) = event.track {
            *slot.lock().unwrap() = Some(track);
        }
    })));
    receiver
        .add_transceiver_for_media(
            MediaType::Audio,
            RtpTransceiverInit {
                direction: RtpTransceiverDirection::RecvOnly,
                stream_ids: Vec::new(),
                send_encodings: Vec::new(),
            },
        )
        .ok();
    let sender_candidates = candidates(&sender);
    let receiver_candidates = candidates(&receiver);
    let offer = sender
        .create_offer(OfferOptions::default())
        .await
        .map_err(|_| "mic offer")?;
    sender
        .set_local_description(offer.clone())
        .await
        .map_err(|_| "mic local offer")?;
    receiver
        .set_remote_description(offer)
        .await
        .map_err(|_| "mic remote offer")?;
    let answer = receiver
        .create_answer(Default::default())
        .await
        .map_err(|_| "mic answer")?;
    receiver
        .set_local_description(answer.clone())
        .await
        .map_err(|_| "mic local answer")?;
    sender
        .set_remote_description(answer)
        .await
        .map_err(|_| "mic remote answer")?;

    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        apply_candidates(&sender_candidates, &receiver).await;
        apply_candidates(&receiver_candidates, &sender).await;
        if sender.connection_state() == PeerConnectionState::Connected
            && receiver.connection_state() == PeerConnectionState::Connected
            && remote.lock().unwrap().is_some()
        {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let track = remote.lock().unwrap().take().ok_or("no mic track")?;
    let decoded = Arc::new(Mutex::new(Vec::<i16>::new()));
    let sink = decoded.clone();
    let reader = tokio::spawn(async move {
        let mut stream = NativeAudioStream::new(track, SAMPLE_RATE as i32, 2);
        while let Some(frame) = stream.next().await {
            let channels = frame.num_channels as usize;
            let mut samples = sink.lock().unwrap();
            for sample in frame.data.chunks(channels.max(1)) {
                samples.push(sample[0]);
            }
        }
    });

    let mut tone = ToneProducer::new(&[frequency]);
    let started = Instant::now();
    while started.elapsed() < duration {
        if let Some(block) = tone.next_block() {
            let frame = AudioFrame {
                data: std::borrow::Cow::Owned(block),
                sample_rate: SAMPLE_RATE,
                num_channels: CHANNELS,
                samples_per_channel: BLOCK_SAMPLES as u32 / CHANNELS,
            };
            let _ = source.capture_frame(&frame).await;
        }
        sleep(Duration::from_millis(10)).await;
    }
    sleep(Duration::from_millis(250)).await;
    reader.abort();
    let _ = reader.await;
    let decoded = decoded.lock().unwrap().clone();
    sender.close();
    receiver.close();
    Ok(MicrophoneRun { track_id, decoded })
}
