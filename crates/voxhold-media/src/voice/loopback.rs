//! End-to-end proof that the software DSP reaches Opus and the remote decoder.
//! Ported from the local Go `media/src/voice_loopback.rs` (blob
//! `e7433e9b8c0a69ab5cd9097bad1b6b458e947fb3`, base `09c82b9`).
//!
//! Two peer connections live in one process and exchange SDP/ICE directly, so
//! the test exercises encode, RTP and decode without a server. The fixture is
//! the same control sequence used by the unit tests.

use std::sync::{Arc, Mutex};

use libwebrtc::{
    audio_frame::AudioFrame,
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    audio_stream::native::NativeAudioStream,
    ice_candidate::IceCandidate,
    media_stream_track::MediaStreamTrack,
    peer_connection::{AnswerOptions, OfferOptions, PeerConnection, PeerConnectionState},
    peer_connection_factory::{PeerConnectionFactory, native::PeerConnectionFactoryExt},
    stats::RtcStats,
};
use tokio::time::{Duration, Instant, sleep};
use tokio_stream::StreamExt;

use crate::capture::BlockProcessor;
use crate::dsp::{self, AudioParams, NoiseMode};

const RATE: u32 = 48_000;
const BLOCK: usize = (RATE / 100) as usize;

type Candidates = Arc<Mutex<Vec<(String, String, i32)>>>;

fn candidates(pc: &PeerConnection) -> Candidates {
    let store = Arc::new(Mutex::new(Vec::new()));
    let sink = store.clone();
    pc.on_ice_candidate(Some(Box::new(move |ice| {
        sink.lock()
            .unwrap()
            .push((ice.to_string(), ice.sdp_mid(), ice.sdp_mline_index()));
    })));
    store
}

async fn drain(store: &Candidates, target: &PeerConnection) {
    let items = std::mem::take(&mut *store.lock().unwrap());
    for (text, mid, index) in items {
        if let Ok(candidate) = IceCandidate::parse(&mid, index, &text) {
            let _ = target.add_ice_candidate(candidate).await;
        }
    }
}

async fn connect(offerer: &PeerConnection, answerer: &PeerConnection) -> Result<(), &'static str> {
    let offer = offerer
        .create_offer(OfferOptions::default())
        .await
        .map_err(|_| "offer")?;
    offerer
        .set_local_description(offer.clone())
        .await
        .map_err(|_| "local offer")?;
    answerer
        .set_remote_description(offer)
        .await
        .map_err(|_| "remote offer")?;
    let answer = answerer
        .create_answer(AnswerOptions::default())
        .await
        .map_err(|_| "answer")?;
    answerer
        .set_local_description(answer.clone())
        .await
        .map_err(|_| "local answer")?;
    offerer
        .set_remote_description(answer)
        .await
        .map_err(|_| "remote answer")?;
    Ok(())
}

async fn play(source: &NativeAudioSource, params: &AudioParams, samples: &[i16]) {
    let mut processor = BlockProcessor::default();
    let mut block = [0_i16; BLOCK];
    for chunk in samples.chunks(BLOCK) {
        if chunk.len() < BLOCK {
            break;
        }
        block.copy_from_slice(chunk);
        processor.process(&mut block, params);
        let frame = AudioFrame {
            data: std::borrow::Cow::Borrowed(&block),
            sample_rate: RATE,
            num_channels: 1,
            samples_per_channel: BLOCK as u32,
        };
        if source.capture_frame(&frame).await.is_err() {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
}

fn frame_db(values: &[i16]) -> f32 {
    dsp::dbfs(values).unwrap_or(-120.0)
}

fn energy_db(frames: &[f32]) -> f32 {
    if frames.is_empty() {
        return -120.0;
    }
    let energy = frames
        .iter()
        .map(|db| 10.0_f32.powf(db / 10.0))
        .sum::<f32>();
    (10.0 * (energy / frames.len() as f32).log10()).max(-120.0)
}

/// The decoder may start late, so segments are found by level instead of by
/// wall-clock windows: the tone is the longest loud run, noise is everything
/// outside it after the gate/decoder margins.
fn analyse(frames: &[f32]) -> (f32, f32, f32) {
    let loud = frames
        .iter()
        .enumerate()
        .filter(|(_, db)| **db > -40.0)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if loud.is_empty() {
        return (-120.0, -120.0, -120.0);
    }
    let (mut start, mut previous) = (loud[0], loud[0]);
    let mut best = (start, previous);
    for index in loud.into_iter().skip(1) {
        if index == previous + 1 {
            previous = index;
            continue;
        }
        if previous - start > best.1 - best.0 {
            best = (start, previous);
        }
        (start, previous) = (index, index);
    }
    if previous - start > best.1 - best.0 {
        best = (start, previous);
    }
    let (run_start, run_end) = best;
    let before = energy_db(&frames[..run_start.saturating_sub(15)]);
    let after = energy_db(&frames[(run_end + 60).min(frames.len())..]);
    let tone = energy_db(&frames[run_start..=run_end]);
    (before, tone, after)
}

fn captured_duration(stats: Vec<RtcStats>) -> f64 {
    stats
        .into_iter()
        .filter_map(|stat| match stat {
            RtcStats::MediaSource(source) => Some(source.audio.total_samples_duration),
            _ => None,
        })
        .fold(0.0_f64, f64::max)
}

async fn run(mode: NoiseMode) -> Result<(f32, f32, f32, f64, f64), &'static str> {
    let factory = PeerConnectionFactory::default();
    let source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: false,
            noise_suppression: false,
            auto_gain_control: false,
        },
        RATE,
        1,
        0,
    );
    let sender = factory
        .create_peer_connection(Default::default())
        .map_err(|_| "sender peer")?;
    let receiver = factory
        .create_peer_connection(Default::default())
        .map_err(|_| "receiver peer")?;
    let track = factory.create_audio_track("loopback", source.clone());
    sender
        .add_track(MediaStreamTrack::Audio(track), &["voxhold-voice"])
        .map_err(|_| "add track")?;
    let remote = Arc::new(Mutex::new(None));
    let slot = remote.clone();
    receiver.on_track(Some(Box::new(move |event| {
        if let MediaStreamTrack::Audio(track) = event.track {
            *slot.lock().unwrap() = Some(track);
        }
    })));
    let sender_candidates = candidates(&sender);
    let receiver_candidates = candidates(&receiver);
    connect(&sender, &receiver).await?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        drain(&sender_candidates, &receiver).await;
        drain(&receiver_candidates, &sender).await;
        if sender.connection_state() == PeerConnectionState::Connected
            && receiver.connection_state() == PeerConnectionState::Connected
            && remote.lock().unwrap().is_some()
        {
            break;
        }
        if Instant::now() > deadline {
            return Err("loopback did not connect");
        }
        sleep(Duration::from_millis(25)).await;
    }

    let captured_before = captured_duration(sender.get_stats().await.map_err(|_| "initial stats")?);
    let track = remote.lock().unwrap().take().ok_or("no remote track")?;
    let mut stream = NativeAudioStream::new(track, RATE as i32, 1);
    let frames = Arc::new(Mutex::new(Vec::new()));
    let sink = frames.clone();
    let reader = tokio::spawn(async move {
        while let Some(frame) = stream.next().await {
            sink.lock().unwrap().push(frame_db(&frame.data));
        }
    });

    // Skip the leading silence and play noise, tone, noise at real time.
    let fixture = dsp::fixture(RATE);
    let start = (RATE / 2) as usize;
    let params = AudioParams {
        mode,
        gate_percent: 15,
        input_gain: 1.0,
        auto_gain_control: false,
        ..AudioParams::default()
    };
    play(&source, &params, &fixture[start..]).await;
    sleep(Duration::from_millis(900)).await;
    let captured_after = captured_duration(sender.get_stats().await.map_err(|_| "final stats")?);
    reader.abort();
    let _ = reader.await;
    let frames = frames.lock().unwrap().clone();
    sender.close();
    receiver.close();
    if frames.len() < 200 {
        return Err("too few decoded frames");
    }
    let (before, tone, after) = analyse(&frames);
    Ok((before, tone, after, captured_before, captured_after))
}

#[test]
fn threshold_gate_and_auto_noise_survive_opus_round_trip() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (threshold_noise, tone, threshold_tail, threshold_before, threshold_after) =
        runtime.block_on(run(NoiseMode::Threshold)).unwrap();
    let (auto_noise, auto_tone, auto_tail, auto_before, auto_after) =
        runtime.block_on(run(NoiseMode::Auto)).unwrap();
    println!(
        "threshold noise {threshold_noise:.1} dBFS, tone {tone:.1} dBFS, tail {threshold_tail:.1} dBFS, captured {threshold_before:.3}->{threshold_after:.3} s; auto noise {auto_noise:.1} dBFS, tone {auto_tone:.1} dBFS, tail {auto_tail:.1} dBFS, captured {auto_before:.3}->{auto_after:.3} s"
    );
    assert!(
        threshold_after > threshold_before && auto_after > auto_before,
        "the pinned WebRTC stats must report growing total_samples_duration: {threshold_before}->{threshold_after}, {auto_before}->{auto_after}"
    );
    assert!(
        threshold_noise < -60.0,
        "gated noise leaked: {threshold_noise}"
    );
    assert!(tone > -30.0, "speech did not survive Opus: {tone}");
    assert!(
        threshold_tail < -55.0,
        "gate reopened after speech: {threshold_tail}"
    );
    assert!(
        auto_tone > -30.0,
        "auto speech did not survive Opus: {auto_tone}"
    );
    assert!(
        auto_noise < -55.0,
        "auto NS must keep the background low, got {auto_noise}"
    );
    // Auto keeps some background (NS only); threshold transmits nearly nothing.
    assert!(
        auto_noise > threshold_noise + 10.0,
        "auto noise {auto_noise} vs threshold noise {threshold_noise} (auto tail {auto_tail})"
    );
}
