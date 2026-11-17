//! Microphone capture for the software DSP path.
//!
//! Ported from the local Go `media/src/capture.rs` (blob
//! `eac53c0e9883cc53c262815ae971fc1366d3d37e`, base `09c82b9`).
//!
//! Windows is captured with `waveIn` at 48 kHz mono 16-bit. The same device
//! list (WinMM index + display name) feeds the settings dialog, so the index in
//! the saved preference is used directly. WebRTC's platform ADM keeps only the
//! playout side here: capture is ours so the gate, gain and mute are provable
//! over the exact PCM that reaches Opus.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use libwebrtc::{
    audio_frame::AudioFrame,
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    native::apm::AudioProcessingModule,
};

use crate::{
    Event, Events,
    dsp::{self, AudioParams, NoiseGate, NoiseMode, ProcessingKey},
};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 1;
pub const BLOCK: usize = (SAMPLE_RATE / 100) as usize;
const BUFFERS: usize = 4;
/// Bounded PCM queue: twenty 10 ms blocks (200 ms). A stalled consumer drops
/// whole blocks and counts them instead of growing latency without bound.
const MAX_PENDING_BLOCKS: usize = 20;
const MAX_PENDING: usize = BLOCK * MAX_PENDING_BLOCKS;
const LEVEL_INTERVAL: u32 = 5;
/// The software WinMM path has no render reference, so it cannot run echo
/// cancellation and reports `false`. The platform ADM fallback keeps its own
/// render/playout reference and may apply its default AEC; that value is
/// reported as the platform default, not as a measured software claim.
pub const ECHO_CANCELLATION_AVAILABLE: bool = false;

/// Bounded sample queue between the WinMM buffers and the 10 ms DSP blocks.
/// The audio thread never grows it: an overflow discards the oldest block,
/// which is exactly the latency that would otherwise accumulate.
#[derive(Default)]
pub struct PcmBuffer {
    samples: VecDeque<i16>,
    dropped_blocks: u64,
}

impl PcmBuffer {
    pub fn with_capacity() -> Self {
        Self {
            samples: VecDeque::with_capacity(MAX_PENDING + BLOCK),
            dropped_blocks: 0,
        }
    }

    /// Append one completed driver buffer. Every overflow removes one whole
    /// 10 ms block and increments the counter, so the meter can report a
    /// stalled consumer instead of silently hiding it.
    pub fn push(&mut self, chunk: &[i16]) {
        for sample in chunk {
            while self.samples.len() >= MAX_PENDING {
                for _ in 0..BLOCK {
                    self.samples.pop_front();
                }
                self.dropped_blocks += 1;
            }
            self.samples.push_back(*sample);
        }
    }

    pub fn pop_block(&mut self, block: &mut [i16; BLOCK]) -> bool {
        if self.samples.len() < BLOCK {
            return false;
        }
        for sample in block.iter_mut() {
            *sample = self.samples.pop_front().unwrap_or(0);
        }
        true
    }

    /// Mute and capture restart must not replay samples captured before the
    /// control change.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn dropped_blocks(&self) -> u64 {
        self.dropped_blocks
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[cfg(test)]
    pub fn first(&self) -> Option<i16> {
        self.samples.front().copied()
    }
}

/// One APM owns NS/AGC/HPF for the whole software path. A parameter change
/// rebuilds it only when the processing key changes, so moving the gate slider
/// does not reset the noise suppressor.
pub struct BlockProcessor {
    gate: NoiseGate,
    apm: Option<AudioProcessingModule>,
    key: Option<ProcessingKey>,
    muted: bool,
}

impl Default for BlockProcessor {
    fn default() -> Self {
        Self {
            gate: NoiseGate::default(),
            apm: None,
            key: None,
            muted: false,
        }
    }
}

impl BlockProcessor {
    /// Returns the pre-gate level (0..1) and whether the block was silenced.
    pub fn process(&mut self, block: &mut [i16], params: &AudioParams) -> (f32, bool) {
        let params = params.clone().clamped();
        if self.muted != params.muted {
            // Close the door on the old hold in both directions: a mute must
            // not leave the gate open, and an unmute must not replay the tail.
            self.gate.reset();
            self.muted = params.muted;
        }
        let wanted = ProcessingKey::from_params(&params);
        if self.key.as_ref() != Some(&wanted) {
            self.apm = Some(AudioProcessingModule::new(
                wanted.echo_cancellation && ECHO_CANCELLATION_AVAILABLE,
                wanted.auto_gain_control,
                true,
                wanted.noise_suppression,
            ));
            self.key = Some(wanted);
        }
        if let Some(apm) = self.apm.as_mut() {
            let _ = apm.process_stream(block, SAMPLE_RATE as i32, CHANNELS as i32);
        }
        let level = dsp::meter_level(block);
        if params.muted {
            self.gate.reset();
            dsp::silence(block);
            return (level, true);
        }
        if params.mode == NoiseMode::Threshold {
            self.gate.process(block, SAMPLE_RATE, params.threshold());
        } else {
            self.gate.reset();
        }
        dsp::apply_gain(block, params.input_gain);
        (level, false)
    }
}

pub struct MicCapture {
    source: NativeAudioSource,
    params: Arc<Mutex<AudioParams>>,
    events: Arc<Events>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MicCapture {
    pub fn new(
        source: NativeAudioSource,
        params: Arc<Mutex<AudioParams>>,
        events: Arc<Events>,
    ) -> Self {
        Self {
            source,
            params,
            events,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }

    /// Opens the requested WinMM device before stopping the previous capture,
    /// so a bad selection leaves the working microphone untouched.
    pub fn start(&mut self, input: Option<&str>) -> Result<(), String> {
        let device = WinIn::open(input)?;
        self.stop();
        self.stop = Arc::new(AtomicBool::new(false));
        let source = self.source.clone();
        let params = self.params.clone();
        let events = self.events.clone();
        let stop = self.stop.clone();
        self.thread = Some(
            thread::Builder::new()
                .name("voxhold-mic".into())
                .spawn(move || capture_loop(device, source, params, events, stop))
                .map_err(|_| "Не удалось запустить поток микрофона.".to_owned())?,
        );
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn capture_loop(
    mut device: WinIn,
    source: NativeAudioSource,
    params: Arc<Mutex<AudioParams>>,
    events: Arc<Events>,
    stop: Arc<AtomicBool>,
) {
    let mut processor = BlockProcessor::default();
    let mut pending = PcmBuffer::with_capacity();
    let mut transition = MuteTransition::default();
    let mut block = [0_i16; BLOCK];
    let mut source_dropped = 0_u64;
    let mut ticks = 0_u32;
    while !stop.load(Ordering::Acquire) {
        device.completed(|samples| pending.push(samples));
        loop {
            let current = params
                .lock()
                .map(|value| value.clone())
                .unwrap_or_else(|_| AudioParams {
                    // A poisoned parameter lock must never unmute the mic.
                    muted: true,
                    ..AudioParams::default()
                });
            // Check the transition before taking a block: on unmute the
            // already queued pre-mute audio must not reach Opus.
            if transition.changed(current.muted) {
                pending.clear();
                source.clear_buffer();
            }
            if !pending.pop_block(&mut block) {
                break;
            }
            let (level, muted) = processor.process(&mut block, &current);
            let frame = AudioFrame {
                data: std::borrow::Cow::Borrowed(&block),
                sample_rate: SAMPLE_RATE,
                num_channels: CHANNELS,
                samples_per_channel: BLOCK as u32,
            };
            if poll_once(source.capture_frame(&frame)).is_none() {
                source_dropped += 1;
            }
            ticks += 1;
            if ticks % LEVEL_INTERVAL == 0 {
                events.push(Event::InputLevel {
                    level: f64::from(level),
                    muted,
                    dropped: pending.dropped_blocks() + source_dropped,
                });
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    // The shared gate source must not replay a stale block after a restart or
    // a mute that raced with the last capture.
    source.clear_buffer();
    device.close();
}

/// Tracks the last control decision so the capture loop can drop queued audio
/// *before* the first block of the new state is processed.
#[derive(Default)]
struct MuteTransition {
    muted: bool,
}

impl MuteTransition {
    fn changed(&mut self, muted: bool) -> bool {
        if self.muted == muted {
            return false;
        }
        self.muted = muted;
        true
    }
}

/// `queue_size_ms == 0` makes `capture_frame` synchronous. Polling once keeps
/// the audio thread free of a runtime and turns an unexpected wait into a
/// counted drop instead of blocking the sample clock. `Waker::noop` is static,
/// so this path allocates nothing per block.
fn poll_once<F: std::future::Future>(future: F) -> Option<F::Output> {
    use std::{
        pin::pin,
        task::{Context, Poll},
    };
    let mut context = Context::from_waker(std::task::Waker::noop());
    let mut future = pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

#[cfg(windows)]
mod winmm;

#[cfg(windows)]
use winmm::WinIn;

#[cfg(not(windows))]
struct WinIn;

#[cfg(not(windows))]
impl WinIn {
    fn open(_input: Option<&str>) -> Result<Self, String> {
        Err("Захват микрофона доступен только в Windows.".into())
    }

    fn completed(&mut self, _sink: impl FnMut(&[i16])) {}

    fn close(&mut self) {}
}

/// Audio source options for the software path: all platform processing is off
/// so the single APM in `BlockProcessor` owns NS/AGC and nothing is applied
/// twice.
pub fn source_options() -> AudioSourceOptions {
    AudioSourceOptions {
        echo_cancellation: false,
        noise_suppression: false,
        auto_gain_control: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_buffer_is_bounded_and_counts_dropped_blocks() {
        let mut buffer = PcmBuffer::with_capacity();
        for block_index in 0..=MAX_PENDING_BLOCKS {
            let block = vec![block_index as i16; BLOCK];
            buffer.push(&block);
        }
        assert_eq!(buffer.len(), MAX_PENDING);
        assert_eq!(buffer.dropped_blocks(), 1);
        assert_eq!(
            buffer.first(),
            Some(1),
            "the oldest block was discarded, not the newest"
        );
        let mut block = [0_i16; BLOCK];
        assert!(buffer.pop_block(&mut block));
        assert!(block.iter().all(|sample| *sample == 1));
    }

    #[test]
    fn clearing_the_queue_removes_stale_samples() {
        let mut buffer = PcmBuffer::with_capacity();
        buffer.push(&vec![9_000_i16; BLOCK]);
        buffer.clear();
        // A discontinuity/restart starts from the next captured block, not
        // from the samples that were already queued.
        buffer.push(&vec![222_i16; BLOCK]);
        let mut block = [0_i16; BLOCK];
        assert!(buffer.pop_block(&mut block));
        assert!(block.iter().all(|sample| *sample == 222));
        assert!(!buffer.pop_block(&mut block));
    }

    #[test]
    fn mute_silences_the_next_block_and_does_not_replay_the_tail() {
        let speech = vec![8_000_i16; BLOCK];
        let background = vec![20_i16; BLOCK];
        let mut processor = BlockProcessor::default();
        let params = AudioParams {
            mode: NoiseMode::Threshold,
            gate_percent: 15,
            auto_gain_control: false,
            ..AudioParams::default()
        };
        for _ in 0..10 {
            let mut block = speech.clone();
            processor.process(&mut block, &params);
        }
        let muted = AudioParams {
            muted: true,
            ..params.clone()
        };
        let mut block = background.clone();
        let (_, silenced) = processor.process(&mut block, &muted);
        assert!(silenced);
        assert!(block.iter().all(|sample| *sample == 0));
        // Unmuting starts from a closed gate, even though the previous block
        // was speech: the background must not leak old energy.
        let mut block = background;
        processor.process(&mut block, &params);
        assert!(block.iter().all(|sample| *sample == 0));
    }

    #[test]
    fn processing_key_rebuilds_the_apm_only_when_processing_changes() {
        let mut processor = BlockProcessor::default();
        let mut block = [0_i16; BLOCK];
        processor.process(&mut block, &AudioParams::default());
        let key = processor.key.clone();
        processor.process(
            &mut block,
            &AudioParams {
                gate_percent: 42,
                ..AudioParams::default()
            },
        );
        assert_eq!(
            processor.key, key,
            "a gate change must keep the APM instance"
        );
        processor.process(
            &mut block,
            &AudioParams {
                mode: NoiseMode::Threshold,
                ..AudioParams::default()
            },
        );
        assert!(!processor.key.as_ref().unwrap().noise_suppression);
        assert!(
            !ECHO_CANCELLATION_AVAILABLE,
            "AEC is only reported when a render reference exists"
        );
    }

    #[test]
    fn every_supported_gain_level_is_clamped_and_bounded() {
        for percent in [0_u32, 50, 100, 200] {
            let mut block = vec![10_000_i16; BLOCK];
            let peak = dsp::apply_gain(&mut block, percent as f32 / 100.0);
            let expected = 10_000.0 / 32_768.0 * percent as f32 / 100.0;
            assert!((peak - expected).abs() < 0.001, "{percent}%: {peak}");
            assert!(
                block
                    .iter()
                    .all(|sample| (-32_768..=32_767).contains(sample))
            );
        }
        let mut saturated = vec![30_000_i16; BLOCK];
        dsp::apply_gain(&mut saturated, 2.0);
        assert!(
            saturated.iter().all(|sample| *sample == 32_767),
            "200% must clamp at full scale"
        );
    }

    #[test]
    fn synthetic_capture_matrix_matches_the_report() {
        let input = crate::dsp::fixture(SAMPLE_RATE);
        let rms = |samples: &[i16]| crate::dsp::dbfs(samples).unwrap_or(-120.0);
        let section = |from: f32, to: f32| {
            let start = (SAMPLE_RATE as f32 * from) as usize;
            let end = (SAMPLE_RATE as f32 * to) as usize;
            input[start..end].to_vec()
        };
        let run = |params: &AudioParams, muted_from: Option<f32>| {
            let mut processor = BlockProcessor::default();
            let mut output = Vec::with_capacity(input.len());
            let mut clipped = 0_usize;
            let mut block = [0_i16; BLOCK];
            for (index, chunk) in input.chunks(BLOCK).enumerate() {
                if chunk.len() < BLOCK {
                    break;
                }
                block.copy_from_slice(chunk);
                let mut params = params.clone();
                if muted_from.is_some_and(|from| index as f32 * 0.01 >= from) {
                    params.muted = true;
                }
                processor.process(&mut block, &params);
                clipped += block.iter().filter(|sample| **sample == 32_767).count();
                output.extend_from_slice(&block);
            }
            (output, clipped)
        };
        let report = |name: &str, params: &AudioParams| {
            let (output, clipped) = run(params, None);
            let out = |from, to| rms(&section_of(&output, from, to, SAMPLE_RATE));
            println!(
                "{name:10} dry {:6.1} / out noise1 {:6.1}, tone {:6.1}, noise2 {:6.1}, clipped {clipped}",
                rms(&section(0.5, 1.5)),
                out(0.6, 1.4),
                out(1.6, 2.4),
                out(2.9, 3.4)
            );
            output
        };
        let auto = report(
            "auto",
            &AudioParams {
                auto_gain_control: false,
                ..AudioParams::default()
            },
        );
        assert!(rms(&section_of(&auto, 0.6, 1.4, SAMPLE_RATE)) < rms(&section(0.5, 1.5)) - 3.0);
        let bypass = report(
            "threshold0",
            &AudioParams {
                mode: NoiseMode::Threshold,
                gate_percent: 0,
                auto_gain_control: false,
                ..AudioParams::default()
            },
        );
        assert!(
            (rms(&section_of(&bypass, 0.6, 1.4, SAMPLE_RATE)) - rms(&section(0.5, 1.5))).abs()
                < 4.0,
            "threshold=0 bypasses the gate; only the always-on HPF may change the noise level"
        );
        let gated = report(
            "threshold15",
            &AudioParams {
                mode: NoiseMode::Threshold,
                gate_percent: 15,
                auto_gain_control: false,
                ..AudioParams::default()
            },
        );
        assert!(rms(&section_of(&gated, 0.6, 1.4, SAMPLE_RATE)) < -80.0);
        assert!(rms(&section_of(&gated, 1.6, 2.4, SAMPLE_RATE)) > -20.0);
        assert!(
            rms(&section_of(&gated, 2.9, 3.4, SAMPLE_RATE)) < -80.0,
            "the gate must close after the 180 ms hold and 60 ms release"
        );
        for percent in [0_u32, 50, 100, 200] {
            let (output, _) = run(
                &AudioParams {
                    mode: NoiseMode::Threshold,
                    gate_percent: 0,
                    input_gain: percent as f32 / 100.0,
                    auto_gain_control: false,
                    ..AudioParams::default()
                },
                None,
            );
            let tone = rms(&section_of(&output, 1.6, 2.4, SAMPLE_RATE));
            println!("gain {percent:3}% tone {tone:6.1} dBFS");
            let expected = -15.1 + 20.0 * (percent as f32 / 100.0).max(0.000_1).log10();
            if percent == 0 {
                assert!(tone < -100.0, "0 % must be silence");
            } else {
                assert!(
                    (tone - expected).abs() < 1.0,
                    "gain {percent}%: {tone} vs {expected}"
                );
            }
        }
        let (muted, clipped) = run(&AudioParams::default(), Some(1.0));
        assert!(clipped == 0);
        assert!(
            muted
                .iter()
                .skip(SAMPLE_RATE as usize)
                .all(|sample| *sample == 0),
            "PTT release, deafen and server mute must be silent after the last block"
        );
    }

    fn section_of(samples: &[i16], from: f32, to: f32, rate: u32) -> Vec<i16> {
        let start = (rate as f32 * from) as usize;
        let end = (rate as f32 * to) as usize;
        samples[start..end].to_vec()
    }

    #[test]
    fn dsp_timing_report() {
        use std::time::Instant;
        let mut processor = BlockProcessor::default();
        let params = AudioParams {
            mode: NoiseMode::Auto,
            auto_gain_control: false,
            ..AudioParams::default()
        };
        let mut timings = Vec::with_capacity(2_000);
        let mut block = [0_i16; BLOCK];
        for _ in 0..2_100 {
            // Warm-up is included in the first 100 samples and discarded.
            for sample in block.iter_mut() {
                *sample = 1_000;
            }
            let started = Instant::now();
            processor.process(&mut block, &params);
            timings.push(started.elapsed().as_secs_f64() * 1e6);
        }
        timings.drain(..100);
        timings.sort_by(f64::total_cmp);
        let pick = |p: f64| timings[((timings.len() - 1) as f64 * p) as usize];
        println!(
            "DSP block 10 ms: p50 {:.1} us, p95 {:.1} us, p99 {:.1} us, max {:.1} us",
            pick(0.50),
            pick(0.95),
            pick(0.99),
            timings.last().unwrap()
        );
        println!(
            "Algorithmic input latency: one 10 ms block + WinMM 20 ms buffers + no extra NativeAudioSource queue (queue_size_ms = 0); the bounded 200 ms queue is a stall guard, not steady-state latency"
        );
        assert!(pick(0.99) < 5_000.0, "DSP block must fit in real time");
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "opens the default Windows microphone; run manually"]
    fn software_capture_reaches_the_native_source() {
        let params = Arc::new(Mutex::new(AudioParams {
            muted: false,
            ..AudioParams::default()
        }));
        let events = Arc::new(Events::default());
        let source = NativeAudioSource::new(source_options(), SAMPLE_RATE, CHANNELS, 0);
        let mut mic = MicCapture::new(source, params, events.clone());
        mic.start(None).expect("default microphone");
        thread::sleep(Duration::from_millis(600));
        mic.stop();
        let mut levels = 0;
        let mut dropped = 0;
        while let Some(event) = events.poll(Duration::ZERO) {
            if let Event::InputLevel { dropped: count, .. } = event {
                levels += 1;
                dropped = dropped.max(count);
            }
        }
        assert!(levels > 0, "capture produced no input_level events");
        println!("input levels {levels}, dropped blocks {dropped}");
    }

    #[test]
    fn mute_transition_drops_queued_audio_before_the_first_new_block() {
        let mut pending = PcmBuffer::with_capacity();
        let mut transition = MuteTransition::default();
        pending.push(&vec![9_000_i16; BLOCK]);
        assert!(transition.changed(true));
        pending.clear();
        let mut block = [0_i16; BLOCK];
        assert!(!pending.pop_block(&mut block), "old audio was discarded");
        pending.push(&vec![222_i16; BLOCK]);
        assert!(pending.pop_block(&mut block));
        assert!(block.iter().all(|sample| *sample == 222));
        // Unmute: samples captured while muted must not be replayed.
        pending.push(&vec![9_000_i16; BLOCK]);
        assert!(transition.changed(false));
        pending.clear();
        assert!(!pending.pop_block(&mut block));
        pending.push(&vec![333_i16; BLOCK]);
        assert!(pending.pop_block(&mut block));
        assert!(block.iter().all(|sample| *sample == 333));
    }

    #[test]
    #[cfg(windows)]
    fn saved_winmm_device_is_resolved_by_name_not_by_index() {
        let devices = vec![(0, "Speakers".to_owned()), (1, "USB Headset".to_owned())];
        assert_eq!(
            winmm::choose_device(Some("0:USB Headset"), &devices).unwrap(),
            1,
            "indexes shift; the saved name still identifies the device"
        );
        assert_eq!(
            winmm::choose_device(Some("1:USB"), &devices).unwrap(),
            1,
            "legacy truncated names are resolved by a unique prefix"
        );
        assert_eq!(
            winmm::choose_device(None, &devices).unwrap(),
            winmm::WAVE_MAPPER,
            "an empty preference is the Windows default"
        );
        assert!(winmm::choose_device(Some("0:Gone"), &devices).is_err());
        let ambiguous = vec![(0, "Mic A".to_owned()), (1, "Mic B".to_owned())];
        assert!(
            winmm::choose_device(Some("0:Mic"), &ambiguous).is_err(),
            "an ambiguous prefix must not open an arbitrary device"
        );
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "opens the default Windows microphone; run manually"]
    fn winmm_capture_delivers_blocks() {
        let mut device = WinIn::open(None).expect("default microphone");
        let started = std::time::Instant::now();
        let mut samples = 0;
        while started.elapsed() < Duration::from_millis(600) {
            device.completed(|chunk| samples += chunk.len());
            thread::sleep(Duration::from_millis(5));
        }
        device.close();
        println!("captured {samples} samples in 600 ms");
        assert!(
            samples >= (SAMPLE_RATE as usize / 5),
            "captured only {samples} samples"
        );
        assert!(
            samples <= SAMPLE_RATE as usize,
            "captured duplicate buffers: {samples} samples"
        );
    }
}
