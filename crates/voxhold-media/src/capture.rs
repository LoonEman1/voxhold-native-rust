//! Microphone capture for the software DSP path.
//!
//! Ported from the local Go `media/src/capture.rs` (blob
//! `eac53c0e9883cc53c262815ae971fc1366d3d37e`, base `09c82b9`).
//!
//! Windows is captured with `waveIn` at 48 kHz mono 16-bit. The settings list
//! stores the Core Audio endpoint id (`DRV_QUERYFUNCTIONINSTANCEID`); a legacy
//! `index:name` or older PnP interface id is migrated by a unique name. WebRTC's
//! platform ADM keeps only the playout side
//! here: capture is ours so the gate, gain and mute are provable over the exact
//! PCM that reaches Opus. Platform ADM is used only when no WinMM input opens.

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
    voice::CaptureGate,
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

/// Whether a saved microphone id still resolves to a current WinMM input.
/// `Session::check_devices` uses this because a working WinMM capture is not
/// enumerated by Core Audio; an unplugged selected microphone must move to the
/// system device instead of staying silently dead until the next join.
#[cfg(windows)]
pub fn input_available(id: &str) -> bool {
    winmm::choose_device(Some(id), &winmm::list()).is_ok()
}

#[cfg(not(windows))]
pub fn input_available(_id: &str) -> bool {
    false
}
/// The software WinMM path has no render reference, so it cannot run echo
/// cancellation and reports `false`. The platform ADM fallback keeps its own
/// render/playout reference and may apply its default AEC, but nothing in this
/// build measures it, so neither path reports AEC as applied.
pub const ECHO_CANCELLATION_AVAILABLE: bool = false;

/// Bounded sample queue between the WinMM buffers and the 10 ms DSP blocks.
/// The audio thread never grows it: an overflow discards the oldest block,
/// which is exactly the latency that would otherwise accumulate.
#[derive(Default)]
pub struct PcmBuffer {
    samples: VecDeque<i16>,
    dropped_blocks: u64,
    /// Permission generation of every queued sample. Blocks from an older
    /// generation are dropped instead of being replayed under the new one.
    generation: u64,
}

impl PcmBuffer {
    pub fn with_capacity() -> Self {
        Self {
            samples: VecDeque::with_capacity(MAX_PENDING + BLOCK),
            dropped_blocks: 0,
            generation: 0,
        }
    }

    /// Append one completed driver buffer. The generation is captured before
    /// the driver is read, so a buffer that started before a mute/unmute edge
    /// cannot inherit the new permission just because the epoch changed while
    /// `completed()` was running.
    pub fn push(&mut self, chunk: &[i16], generation: u64) {
        if generation != self.generation {
            // The driver queue may still contain a block from the previous
            // state; discard it together with everything already copied.
            self.samples.clear();
            self.generation = generation;
        }
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

    pub fn pop_block(&mut self, block: &mut [i16; BLOCK], generation: u64) -> bool {
        if self.generation != generation {
            self.samples.clear();
            return false;
        }
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
    /// Capture restart, mute transition or a permission epoch change: the
    /// gate must not carry a hold from the old state.
    pub fn reset(&mut self) {
        self.gate.reset();
        self.muted = true;
    }

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
    /// Unique software capture instance. A gate acknowledgement is only valid
    /// for the track currently attached to this instance.
    id: u64,
    source: NativeAudioSource,
    params: Arc<Mutex<AudioParams>>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    /// Optional gate acknowledgement. A software track stays disabled until
    /// the capture thread has flushed the generation it just observed.
    gate: Option<Arc<CaptureGate>>,
    events: Arc<Events>,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    progress: Arc<std::sync::atomic::AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl MicCapture {
    pub fn new(
        source: NativeAudioSource,
        params: Arc<Mutex<AudioParams>>,
        epoch: Arc<std::sync::atomic::AtomicU64>,
        gate: Option<Arc<CaptureGate>>,
        events: Arc<Events>,
    ) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::SeqCst),
            source,
            params,
            epoch,
            gate,
            events,
            stop: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            thread: None,
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// The driver stopped delivering or requeueing buffers. The session polls
    /// this together with `progress` and switches to the system input.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Monotonic count of processed blocks, used to detect a stalled driver.
    pub fn progress(&self) -> u64 {
        self.progress.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn test_fail(&self) {
        self.failed.store(true, Ordering::Release);
    }

    /// Opens the requested WinMM device before stopping the previous capture,
    /// so a bad selection leaves the working microphone untouched.
    pub fn start(&mut self, input: Option<&str>) -> Result<(), String> {
        let device = WinIn::open(input)?;
        self.stop();
        self.stop = Arc::new(AtomicBool::new(false));
        self.failed = Arc::new(AtomicBool::new(false));
        let id = self.id;
        let source = self.source.clone();
        let params = self.params.clone();
        let epoch = self.epoch.clone();
        let gate = self.gate.clone();
        let events = self.events.clone();
        let stop = self.stop.clone();
        let failed = self.failed.clone();
        let progress = self.progress.clone();
        self.thread = Some(
            thread::Builder::new()
                .name("voxhold-mic".into())
                .spawn(move || {
                    capture_loop(
                        id, device, source, params, epoch, gate, events, stop, failed, progress,
                    )
                })
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
    capture: u64,
    mut device: WinIn,
    source: NativeAudioSource,
    params: Arc<Mutex<AudioParams>>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    gate: Option<Arc<CaptureGate>>,
    events: Arc<Events>,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    progress: Arc<std::sync::atomic::AtomicU64>,
) {
    let mut processor = BlockProcessor::default();
    let mut pending = PcmBuffer::with_capacity();
    let mut block = [0_i16; BLOCK];
    let mut source_dropped = 0_u64;
    let mut ticks = 0_u32;
    let mut permission = PermissionEpoch::new(&epoch);
    let mut acknowledged = false;
    while !stop.load(Ordering::Acquire) {
        // Bind the permission generation before touching the driver queue.
        // Everything read in this pass belongs to this generation even if a
        // mute/PTT edge advances the epoch while `completed()` is running.
        let generation = epoch.load(Ordering::Acquire);
        if !acknowledged || permission.changed(generation) {
            // The control decision changed (including a fast PTT press that
            // returned to the same boolean). Throw away Rust and driver
            // buffers captured under the previous state before telling the
            // gate that this generation is safe to transmit. The initial
            // acknowledgement also covers a capture started while the gate
            // was already open.
            pending.clear();
            source.clear_buffer();
            processor.reset();
            if !device.flush() {
                failed.store(true, Ordering::Release);
                break;
            }
            acknowledged = true;
        }
        // Acknowledge every pass, not only a changed generation. A new capture
        // may start before its track replaces the sender; its acknowledgement
        // must become effective only after `attach_source` names this instance.
        if let Some(gate) = &gate {
            gate.acknowledge(capture, generation);
        }
        device.completed(|samples| pending.push(samples, generation));
        if device.failed() {
            failed.store(true, Ordering::Release);
            break;
        }
        loop {
            // Do not hand any block to Opus after a generation change until
            // the driver queue has been flushed at the top of the outer loop.
            if epoch.load(Ordering::Acquire) != generation {
                pending.clear();
                source.clear_buffer();
                processor.reset();
                break;
            }
            if !pending.pop_block(&mut block, generation) {
                break;
            }
            let current = params
                .lock()
                .map(|value| value.clone())
                .unwrap_or_else(|_| AudioParams {
                    // A poisoned parameter lock must never unmute the mic.
                    muted: true,
                    ..AudioParams::default()
                });
            let (level, muted) = processor.process(&mut block, &current);
            if epoch.load(Ordering::Acquire) != generation {
                dsp::silence(&mut block);
                pending.clear();
                source.clear_buffer();
                processor.reset();
                break;
            }
            let frame = AudioFrame {
                data: std::borrow::Cow::Borrowed(&block),
                sample_rate: SAMPLE_RATE,
                num_channels: CHANNELS,
                samples_per_channel: BLOCK as u32,
            };
            if poll_once(source.capture_frame(&frame)).is_none() {
                source_dropped += 1;
            }
            progress.fetch_add(1, Ordering::Release);
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
    if device.failed() {
        failed.store(true, Ordering::Release);
    }
}

/// Tracks the last permission epoch so the capture loop can drop queued audio
/// before the first block of a new control state is processed.
struct PermissionEpoch {
    seen: u64,
}

impl PermissionEpoch {
    fn new(epoch: &std::sync::atomic::AtomicU64) -> Self {
        Self {
            seen: epoch.load(Ordering::Acquire),
        }
    }

    fn changed(&mut self, current: u64) -> bool {
        if current == self.seen {
            return false;
        }
        self.seen = current;
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
            buffer.push(&block, 0);
        }
        assert_eq!(buffer.len(), MAX_PENDING);
        assert_eq!(buffer.dropped_blocks(), 1);
        assert_eq!(
            buffer.first(),
            Some(1),
            "the oldest block was discarded, not the newest"
        );
        let mut block = [0_i16; BLOCK];
        assert!(buffer.pop_block(&mut block, 0));
        assert!(block.iter().all(|sample| *sample == 1));
    }

    #[test]
    fn clearing_the_queue_removes_stale_samples() {
        let mut buffer = PcmBuffer::with_capacity();
        buffer.push(&vec![9_000_i16; BLOCK], 0);
        buffer.clear();
        // A discontinuity/restart starts from the next captured block, not
        // from the samples that were already queued.
        buffer.push(&vec![222_i16; BLOCK], 0);
        let mut block = [0_i16; BLOCK];
        assert!(buffer.pop_block(&mut block, 0));
        assert!(block.iter().all(|sample| *sample == 222));
        assert!(!buffer.pop_block(&mut block, 0));
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
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut mic = MicCapture::new(source, params, epoch, None, events.clone());
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
    fn permission_epoch_discards_queued_audio_across_fast_ptt() {
        let epoch = std::sync::atomic::AtomicU64::new(0);
        let mut permission = PermissionEpoch::new(&epoch);
        let mut pending = PcmBuffer::with_capacity();
        pending.push(&vec![9_000_i16; BLOCK], 0);
        // Press and release between two polls restores the boolean state but
        // still advances the epoch twice.
        epoch.fetch_add(1, Ordering::SeqCst);
        epoch.fetch_add(1, Ordering::SeqCst);
        assert!(permission.changed(epoch.load(Ordering::Acquire)));
        pending.clear();
        let mut block = [0_i16; BLOCK];
        assert!(!pending.pop_block(&mut block, 2), "old audio was discarded");
        pending.push(&vec![222_i16; BLOCK], 2);
        assert!(pending.pop_block(&mut block, 2));
        assert!(block.iter().all(|sample| *sample == 222));
        // End-to-end view of the same transition: the first processed block
        // after the flush comes from the post-transition buffer, not from the
        // 9 000-amplitude audio captured before unmute.
        let mut processor = BlockProcessor::default();
        processor.reset();
        for (index, sample) in block.iter_mut().enumerate() {
            *sample = if index % 2 == 0 { 222 } else { -222 };
        }
        let params = AudioParams {
            mode: NoiseMode::Threshold,
            gate_percent: 0,
            auto_gain_control: false,
            ..AudioParams::default()
        };
        processor.process(&mut block, &params);
        assert!(
            block.iter().all(|sample| sample.abs() < 400),
            "stale pre-unmute audio reached the output: {:?}",
            &block[..4]
        );
        assert!(!permission.changed(epoch.load(Ordering::Acquire)));
    }

    #[test]
    fn a_block_read_before_a_generation_change_is_not_sent_after_unmute() {
        let mut pending = PcmBuffer::with_capacity();
        // The driver buffer was captured while muted, so the loop tagged it
        // with generation 7 before calling `completed()`. The mute is released
        // while that call is still running; the generation reaches 8.
        pending.push(&vec![12_345_i16; BLOCK], 7);
        let mut block = [0_i16; BLOCK];
        assert!(
            !pending.pop_block(&mut block, 8),
            "a buffer tagged with the old generation must not pass the new epoch check"
        );
        assert!(
            block.iter().all(|sample| *sample == 0),
            "no stale sample may be left in the output block"
        );
        // After the queue is reset, only a block read under the new
        // generation is eligible.
        pending.push(&vec![222_i16; BLOCK], 8);
        assert!(pending.pop_block(&mut block, 8));
        assert!(block.iter().all(|sample| *sample == 222));
    }

    #[test]
    #[cfg(windows)]
    fn saved_winmm_device_is_resolved_by_endpoint_or_unique_name() {
        let devices = vec![
            winmm::DeviceInfo {
                index: 0,
                name: "Speakers".into(),
                endpoint: Some("endpoint-0".into()),
                legacy_interface: Some("interface-0".into()),
            },
            winmm::DeviceInfo {
                index: 1,
                name: "USB Headset".into(),
                endpoint: Some("endpoint-1".into()),
                legacy_interface: Some("interface-1".into()),
            },
        ];
        assert_eq!(
            winmm::choose_device(Some("0:USB Headset"), &devices)
                .unwrap()
                .index,
            1,
            "indexes shift; the saved name still identifies the device"
        );
        assert_eq!(
            winmm::choose_device(Some("endpoint-1"), &devices)
                .unwrap()
                .index,
            1,
            "new preferences use the stable endpoint id"
        );
        assert_eq!(
            winmm::choose_device(Some("interface-1"), &devices)
                .unwrap()
                .index,
            1,
            "a legacy PnP interface id saved by an older build still migrates"
        );
        assert_eq!(
            winmm::choose_device(Some("1:USB"), &devices).unwrap().index,
            1,
            "legacy truncated names are resolved by a unique prefix"
        );
        assert_eq!(
            winmm::choose_device(None, &devices).unwrap().index,
            winmm::WAVE_MAPPER,
            "an empty preference is the Windows default"
        );
        assert!(winmm::choose_device(Some("0:Gone"), &devices).is_err());
        assert!(
            winmm::choose_device(Some("0:"), &devices).is_err(),
            "a legacy id without a name must not open a reused index"
        );
        let same_name = vec![
            winmm::DeviceInfo {
                index: 3,
                name: "USB Mic".into(),
                endpoint: Some("endpoint-3".into()),
                legacy_interface: Some("interface-3".into()),
            },
            winmm::DeviceInfo {
                index: 0,
                name: "USB Mic".into(),
                endpoint: Some("endpoint-0".into()),
                legacy_interface: Some("interface-0".into()),
            },
        ];
        assert!(
            winmm::choose_device(Some("3:USB Mic"), &same_name).is_err(),
            "a legacy duplicate name must ask the user to choose again"
        );
        assert_eq!(
            winmm::choose_device(Some("endpoint-0"), &same_name)
                .unwrap()
                .index,
            0,
            "the endpoint id distinguishes identical names"
        );
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "opens the default Windows microphone; run manually"]
    fn winmm_flush_restarts_the_driver_queue() {
        let mut device = WinIn::open(None).expect("default microphone");
        thread::sleep(Duration::from_millis(120));
        let mut before = 0;
        device.completed(|samples| before += samples.len());
        assert!(before > 0, "the driver returned no samples before flush");
        assert!(device.flush(), "flush must restart the capture queue");
        thread::sleep(Duration::from_millis(120));
        let mut after = 0;
        device.completed(|samples| after += samples.len());
        assert!(after > 0, "the driver returned no samples after flush");
        device.close();
        assert!(!device.failed(), "a normal close must succeed");
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
