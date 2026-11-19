//! Microphone capture for the software DSP path.
//!
//! Ported from the local Go `media/src/capture.rs` (blob
//! `eac53c0e9883cc53c262815ae971fc1366d3d37e`, base `09c82b9`).
//!
//! Windows is captured with `waveIn` at 48 kHz mono 16-bit. The driver signals
//! a Win32 event when its 20 ms buffers complete; that readiness only fills a
//! bounded jitter buffer. A separate 10 ms deadline in [`timeline`] decides
//! when exactly one 480-sample block leaves, so a late poll cannot reorder or
//! burst the frames. The settings list stores the Core Audio endpoint id
//! (`DRV_QUERYFUNCTIONINSTANCEID`); a legacy `index:name` or older PnP
//! interface id is migrated by a unique name. WebRTC's
//! platform ADM keeps only the playout side
//! here: capture is ours so the gate, gain and mute are provable over the exact
//! PCM that reaches Opus. Platform ADM is used only when no WinMM input opens.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
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
/// Four 20 ms buffers are queued in the driver. They are only readiness: the
/// 10 ms output cadence belongs to [`timeline::PcmJitter`].
const BUFFERS: usize = 4;
const LEVEL_INTERVAL: u32 = 5;

mod timeline;

pub use timeline::CaptureStats;
use timeline::{PcmJitter, Tick};

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

/// Distinct WinMM readiness results. A wait that returns anything else is an
/// error instead of being mistaken for "nothing completed yet".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitOutcome {
    Signaled,
    Timeout,
    Error,
}

/// Outcome of one synchronous `NativeAudioSource::capture_frame` poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceSend {
    Ok,
    Error,
    Pending,
}

fn classify_source<T, E>(result: Option<Result<T, E>>) -> SourceSend {
    match result {
        Some(Ok(_)) => SourceSend::Ok,
        Some(Err(_)) => SourceSend::Error,
        // `queue_size_ms == 0` documents a synchronous path. A `Pending`
        // future is unexpected but counted, never silently dropped.
        None => SourceSend::Pending,
    }
}

fn record_source(stats: &mut CaptureStats, send: SourceSend) {
    match send {
        SourceSend::Ok => stats.source_ok += 1,
        SourceSend::Error => stats.source_errors += 1,
        SourceSend::Pending => stats.source_pending += 1,
    }
}

/// Count a wait result. Only `Signaled` and the normal `Timeout` keep the
/// capture alive; an error is returned so the caller can report a driver
/// failure and let the session run its recovery ladder.
fn record_wait(stats: &mut CaptureStats, outcome: WaitOutcome) -> bool {
    match outcome {
        WaitOutcome::Signaled => true,
        WaitOutcome::Timeout => {
            stats.wait_timeouts += 1;
            true
        }
        WaitOutcome::Error => {
            stats.wait_errors += 1;
            false
        }
    }
}

/// Counters shared with `MicCapture::stats()` without an audio-thread lock.
#[derive(Default)]
struct SharedStats {
    captured_blocks: AtomicU64,
    source_ok: AtomicU64,
    source_errors: AtomicU64,
    source_pending: AtomicU64,
    underruns: AtomicU64,
    overflow_blocks: AtomicU64,
    dropped_blocks: AtomicU64,
    late_ticks: AtomicU64,
    wait_timeouts: AtomicU64,
    wait_errors: AtomicU64,
    driver_errors: AtomicU64,
}

impl SharedStats {
    fn publish(&self, stats: &CaptureStats) {
        self.captured_blocks
            .store(stats.captured_blocks, Ordering::Relaxed);
        self.source_ok.store(stats.source_ok, Ordering::Relaxed);
        self.source_errors
            .store(stats.source_errors, Ordering::Relaxed);
        self.source_pending
            .store(stats.source_pending, Ordering::Relaxed);
        self.underruns.store(stats.underruns, Ordering::Relaxed);
        self.overflow_blocks
            .store(stats.overflow_blocks, Ordering::Relaxed);
        self.dropped_blocks
            .store(stats.dropped_blocks, Ordering::Relaxed);
        self.late_ticks.store(stats.late_ticks, Ordering::Relaxed);
        self.wait_timeouts
            .store(stats.wait_timeouts, Ordering::Relaxed);
        self.wait_errors.store(stats.wait_errors, Ordering::Relaxed);
        self.driver_errors
            .store(stats.driver_errors, Ordering::Relaxed);
    }

    fn snapshot(&self) -> CaptureStats {
        CaptureStats {
            captured_blocks: self.captured_blocks.load(Ordering::Relaxed),
            source_ok: self.source_ok.load(Ordering::Relaxed),
            source_errors: self.source_errors.load(Ordering::Relaxed),
            source_pending: self.source_pending.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            overflow_blocks: self.overflow_blocks.load(Ordering::Relaxed),
            dropped_blocks: self.dropped_blocks.load(Ordering::Relaxed),
            late_ticks: self.late_ticks.load(Ordering::Relaxed),
            wait_timeouts: self.wait_timeouts.load(Ordering::Relaxed),
            wait_errors: self.wait_errors.load(Ordering::Relaxed),
            driver_errors: self.driver_errors.load(Ordering::Relaxed),
        }
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
    /// Lock-free counters published by the capture thread. `progress` used to
    /// be a separate block count, which let a driver stall hide behind
    /// synthesized blocks; the real-PCM count lives in `CaptureStats`.
    stats: Arc<SharedStats>,
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
            stats: Arc::new(SharedStats::default()),
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

    /// Monotonic count of real captured 480-sample blocks, used to detect a
    /// stalled driver. Fade/silence blocks never advance it.
    pub fn progress(&self) -> u64 {
        self.stats().captured_blocks
    }

    /// Aggregate capture counters, including underruns, overflow, late ticks
    /// and source/wait errors. Contains no PCM or personal content.
    pub fn stats(&self) -> CaptureStats {
        self.stats.snapshot()
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
        self.stats = Arc::new(SharedStats::default());
        let id = self.id;
        let source = self.source.clone();
        let params = self.params.clone();
        let epoch = self.epoch.clone();
        let gate = self.gate.clone();
        let events = self.events.clone();
        let stop = self.stop.clone();
        let failed = self.failed.clone();
        let stats = self.stats.clone();
        self.thread = Some(
            thread::Builder::new()
                .name("voxhold-mic".into())
                .spawn(move || {
                    capture_loop(
                        id, device, source, params, epoch, gate, events, stop, failed, stats,
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
    shared: Arc<SharedStats>,
) {
    let mut processor = BlockProcessor::default();
    let mut jitter = PcmJitter::new();
    let mut block = [0_i16; BLOCK];
    let mut counters = CaptureStats::default();
    let mut ticks = 0_u32;
    // Both are assigned by every tick that emits a block; `Tick::None` never
    // reads them.
    let mut level: f64;
    let mut muted: bool;
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
            jitter.reset(generation);
            source.clear_buffer();
            processor.reset();
            if !device.flush() {
                counters.driver_errors += 1;
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
        // Driver readiness only fills the bounded jitter buffer. It never
        // decides when a block leaves: that is the 10 ms tick below.
        device.completed(|samples| jitter.push(samples, generation));
        if device.failed() {
            counters.driver_errors += 1;
            failed.store(true, Ordering::Release);
            break;
        }
        if epoch.load(Ordering::Acquire) != generation {
            // The control edge happened while the driver queue was being
            // read. Everything just tagged with the old generation is stale:
            // drop it and flush the driver before the next emission.
            jitter.reset(epoch.load(Ordering::Acquire));
            source.clear_buffer();
            processor.reset();
            publish_stats(&shared, &counters, &jitter);
            continue;
        }
        match jitter.tick(Instant::now(), &mut block) {
            Tick::None => {}
            Tick::Send => {
                let current = params
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_else(|_| AudioParams {
                        // A poisoned parameter lock must never unmute the mic.
                        muted: true,
                        ..AudioParams::default()
                    });
                let (metered, silenced) = processor.process(&mut block, &current);
                if epoch.load(Ordering::Acquire) != generation {
                    // The epoch changed while the block was being processed.
                    // Do not hand it to Opus; the next pass flushes the driver
                    // and rebuilds the reserve under the new permission.
                    dsp::silence(&mut block);
                    jitter.reset(epoch.load(Ordering::Acquire));
                    source.clear_buffer();
                    processor.reset();
                    publish_stats(&shared, &counters, &jitter);
                    continue;
                }
                jitter.note_last_sample(block[BLOCK - 1]);
                deliver(&source, &block, &mut counters);
                counters.captured_blocks += 1;
                level = f64::from(metered);
                muted = silenced;
                report_level(&events, &mut ticks, level, muted, &counters, &jitter);
            }
            Tick::Fade => {
                // Synthesized fade: count it as an output block so a source
                // error is not lost, but never as physical capture progress.
                let silenced = params.lock().map(|value| value.muted).unwrap_or(true);
                if silenced {
                    dsp::silence(&mut block);
                }
                deliver(&source, &block, &mut counters);
                level = 0.0;
                muted = silenced;
                report_level(&events, &mut ticks, level, muted, &counters, &jitter);
            }
        }
        publish_stats(&shared, &counters, &jitter);
        // The event wait is bounded so a mute/PTT epoch is noticed promptly;
        // the deadline is still the output clock, not this timeout.
        let wait = jitter.wait_duration(Instant::now());
        if !record_wait(&mut counters, device.wait(wait)) {
            failed.store(true, Ordering::Release);
            break;
        }
    }
    // The shared gate source must not replay a stale block after a restart or
    // a mute that raced with the last capture.
    source.clear_buffer();
    device.close();
    if device.failed() {
        counters.driver_errors += 1;
        failed.store(true, Ordering::Release);
    }
    publish_stats(&shared, &counters, &jitter);
}

/// One synchronous source poll, with Ok/Err/Pending counted separately.
fn deliver(source: &NativeAudioSource, block: &[i16; BLOCK], counters: &mut CaptureStats) {
    let frame = AudioFrame {
        data: std::borrow::Cow::Borrowed(block),
        sample_rate: SAMPLE_RATE,
        num_channels: CHANNELS,
        samples_per_channel: BLOCK as u32,
    };
    record_source(
        counters,
        classify_source(poll_once(source.capture_frame(&frame))),
    );
}

fn report_level(
    events: &Events,
    ticks: &mut u32,
    level: f64,
    muted: bool,
    counters: &CaptureStats,
    jitter: &PcmJitter,
) {
    *ticks += 1;
    if *ticks % LEVEL_INTERVAL != 0 {
        return;
    }
    events.push(Event::InputLevel {
        level,
        muted,
        dropped: jitter.dropped_blocks() + counters.source_errors + counters.source_pending,
    });
}

fn publish_stats(shared: &SharedStats, counters: &CaptureStats, jitter: &PcmJitter) {
    shared.publish(&CaptureStats {
        underruns: jitter.underruns(),
        overflow_blocks: jitter.overflow_blocks(),
        dropped_blocks: jitter.dropped_blocks(),
        late_ticks: jitter.late_ticks(),
        ..*counters
    });
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

    fn flush(&mut self) -> bool {
        false
    }

    fn failed(&self) -> bool {
        true
    }

    fn wait(&mut self, _timeout: std::time::Duration) -> WaitOutcome {
        WaitOutcome::Error
    }

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
mod tests;
