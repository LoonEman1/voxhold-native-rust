//! Software playout for the room voice bus.
//!
//! The room session disables the platform ADM playout and feeds decoded remote
//! audio through `mixer` into a WinMM sink. That is the only path in the pinned
//! WebRTC wrapper where a numeric 0–200 % per-participant gain, a local mute
//! that keeps the stored percentage, a limiter and a post-mix meter are real.
//!
//! A stream player keeps its own ADM engine (`stream.rs`): its audio is never
//! part of this mix.
//!
//! Recovery is explicit. A sink that dies or cannot be reopened is retried with
//! a bounded backoff; when the budget is spent the session re-enables the ADM
//! playout and applies only mute there. The UI receives `Event::Audio` after
//! every transition, so it never keeps claiming a mixer that is not running.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    time::{Duration, Instant},
};

use libwebrtc::{
    audio_stream::native::{NativeAudioStream, NativeAudioStreamOptions},
    audio_track::RtcAudioTrack,
};
use tokio_stream::StreamExt;

use super::*;
use crate::mixer::{BLOCK_SAMPLES, Bus, Mixer, PlayoutPhase, PlayoutStats, Source, SourcePool};
use winmm_out::{OpenSink, Sink, WAVE_MAPPER, resolve_output};

/// 48 kHz mono, the same rate as the software capture and the tone.
pub(super) const PLAYOUT_RATE: u32 = 48_000;
/// Whole 10 ms frames a source may run ahead before the queue drops the oldest.
const SOURCE_CHANNEL: usize = 8;
/// How often the playout thread publishes its counters.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// Software-sink reopen budget. Each failed open *and* each sink that opened
/// but died before proving stability spends one attempt; the fifth failure
/// switches the session to the ADM fallback instead of retrying forever.
pub(super) const MAX_OPEN_ATTEMPTS: u8 = 5;
/// Stability proof: the sink must live this long and write this many blocks.
/// A driver can open and fail after the first buffer; two seconds covers a
/// device reset/unplug race, and 50 blocks (0.5 s) prove that PCM was accepted.
pub(super) const STABLE_AFTER: Duration = Duration::from_secs(2);
pub(super) const STABLE_BLOCKS: u64 = 50;
/// Slow retry of the ADM path when it could not be started.
const ADM_RETRY: Duration = Duration::from_secs(5);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// Which playout path owns the room right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlayoutMode {
    Software,
    /// The software mixer could not be kept alive. The ADM plays the room and
    /// only mute is applied; the UI is told through `AppliedAudio`.
    AdmFallback,
}

/// Why the session is reopening the sink. Only the message differs, never the
/// retry policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryCause {
    Resume,
    Sink,
}

#[derive(Clone, Debug)]
pub(super) struct MixState {
    pub(super) bus: f64,
    pub(super) sources: Vec<(String, Source)>,
}

enum Msg {
    Remove(String),
    Frame(String, Vec<i16>),
    Sink(Box<dyn Sink>),
}

/// Bounded, coalesced state slot. A busy frame channel must never lose a gain
/// snapshot: the newest snapshot waits here until the mixer thread takes it.
#[derive(Clone, Default)]
struct StateSlot(Arc<Mutex<Option<MixState>>>);

impl StateSlot {
    fn store(&self, state: MixState) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(state);
        }
    }

    fn take(&self) -> Option<MixState> {
        self.0.lock().ok()?.take()
    }
}

mod tap;
use tap::{Tap, TapRead, TapReceiver};

#[derive(Default)]
struct Metrics {
    level: AtomicU32,
    peak: AtomicU32,
    underruns: AtomicU64,
    dropped: AtomicU64,
    /// Frames dropped by the bounded source channel, before the mixer queue.
    queue_dropped: AtomicU64,
    p95_micros: AtomicU32,
    latency_ms: AtomicU32,
    limited_blocks: AtomicU64,
    limiter_releases: AtomicU64,
    blocks: AtomicU64,
    /// Whole blocks the sink accepted. Stability requires real writes, not just
    /// a successful `waveOutOpen`.
    blocks_written: AtomicU64,
    running: AtomicBool,
}

impl Metrics {
    fn publish(&self, mixer: &Mixer, pool: &SourcePool, latency_ms: u16, running: bool) {
        self.level.store(mixer.level().to_bits(), Ordering::Relaxed);
        self.peak.store(mixer.peak().to_bits(), Ordering::Relaxed);
        self.underruns.store(pool.starved(), Ordering::Relaxed);
        self.dropped.store(
            pool.dropped()
                .saturating_add(self.queue_dropped.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );
        self.p95_micros
            .store(mixer.stats().p95_micros(), Ordering::Relaxed);
        self.latency_ms
            .store(u32::from(latency_ms), Ordering::Relaxed);
        self.limited_blocks
            .store(mixer.limited_blocks(), Ordering::Relaxed);
        self.limiter_releases
            .store(mixer.limiter_releases(), Ordering::Relaxed);
        self.blocks.store(mixer.stats().blocks(), Ordering::Relaxed);
        self.running.store(running, Ordering::Relaxed);
    }

    fn snapshot(&self) -> PlayoutStats {
        PlayoutStats {
            level: f32::from_bits(self.level.load(Ordering::Relaxed)),
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
            underruns: self.underruns.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            p95_micros: self.p95_micros.load(Ordering::Relaxed),
            latency_ms: self.latency_ms.load(Ordering::Relaxed) as u16,
            limited_blocks: self.limited_blocks.load(Ordering::Relaxed),
            limiter_releases: self.limiter_releases.load(Ordering::Relaxed),
            blocks: self.blocks.load(Ordering::Relaxed),
            reconnects: 0,
            running: self.running.load(Ordering::Relaxed),
            phase: PlayoutPhase::Opening,
        }
    }
}

/// Reopen scheduling shared by resume and sink failure. An open is not a
/// recovery: it moves to `Unstable` and only a proven-stable sink clears the
/// failure budget. The unit tests drive this exact type.
#[derive(Clone, Copy, Debug)]
pub(super) struct SinkRecovery {
    failures: u8,
    next_at: Instant,
    phase: PlayoutPhase,
    opened_at: Option<Instant>,
}

impl Default for SinkRecovery {
    fn default() -> Self {
        Self {
            failures: 0,
            next_at: Instant::now(),
            phase: PlayoutPhase::Opening,
            opened_at: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailureOutcome {
    Retry(Duration),
    AdmFallback,
}

impl SinkRecovery {
    pub(super) fn phase(&self) -> PlayoutPhase {
        self.phase
    }

    #[cfg(test)]
    pub(super) fn failures(&self) -> u8 {
        self.failures
    }

    pub(super) fn due(&self, now: Instant) -> bool {
        now >= self.next_at
    }

    /// A successful open is only the start of the proof window.
    pub(super) fn opened(&mut self, now: Instant) {
        self.phase = PlayoutPhase::Unstable;
        self.opened_at = Some(now);
    }

    /// The sink lived long enough and wrote real blocks: the budget is clear.
    pub(super) fn mark_stable(&mut self) {
        if self.phase != PlayoutPhase::Unstable {
            return;
        }
        self.failures = 0;
        self.phase = PlayoutPhase::Stable;
        self.opened_at = None;
    }

    /// Resume keeps the spent budget: a resumed system must not get five fresh
    /// attempts for every suspend.
    pub(super) fn reset_for_resume(&mut self, now: Instant) {
        self.phase = PlayoutPhase::Opening;
        self.next_at = now;
        self.opened_at = None;
    }

    /// Record one failed open or one sink that died before proving stability.
    /// Both spend the same budget; the fifth failure asks for the ADM fallback.
    pub(super) fn record_failure(&mut self, now: Instant) -> FailureOutcome {
        self.failures = self.failures.saturating_add(1);
        self.opened_at = None;
        if self.failures >= MAX_OPEN_ATTEMPTS {
            self.phase = PlayoutPhase::AdmFallback;
            return FailureOutcome::AdmFallback;
        }
        let delay = match self.failures {
            1 => Duration::from_millis(500),
            2 => Duration::from_secs(1),
            3 => Duration::from_secs(2),
            _ => Duration::from_secs(4),
        };
        self.phase = PlayoutPhase::Opening;
        self.next_at = now + delay;
        FailureOutcome::Retry(delay)
    }

    pub(super) fn to_adm(&mut self) {
        self.phase = PlayoutPhase::AdmFallback;
        self.opened_at = None;
    }

    /// A sink that opened and died before proving stability spends the same
    /// budget as an open that never succeeded. This is the only accounting for
    /// the "opened but died" path; removing it would make the ladder endless.
    pub(super) fn sink_died(&mut self, now: Instant) -> FailureOutcome {
        self.record_failure(now)
    }

    #[cfg(test)]
    pub(super) fn opened_at(&self) -> Option<Instant> {
        self.opened_at
    }
}

/// Slow retry slot for the platform path. It has no budget: the ADM is the
/// last audible path, so it keeps being restarted every few seconds.
#[derive(Clone, Copy, Debug)]
pub(super) struct RetryAt {
    next_at: Instant,
}

impl Default for RetryAt {
    fn default() -> Self {
        Self {
            next_at: Instant::now(),
        }
    }
}

impl RetryAt {
    pub(super) fn due(&self, now: Instant) -> bool {
        now >= self.next_at
    }

    pub(super) fn schedule(&mut self, now: Instant, delay: Duration) {
        self.next_at = now + delay;
    }
}

/// ADM health: initialized is not enough. The cumulative played duration must
/// move; a startup grace covers the first stats report, as for capture.
pub(super) fn adm_playout_ok(
    running: bool,
    since_start: Duration,
    duration_seen: bool,
    since_progress: Duration,
) -> bool {
    if !running {
        return false;
    }
    if since_start < INPUT_CAPTURE_GRACE {
        return true;
    }
    duration_seen && since_progress < Duration::from_secs(2)
}

/// One bounded open attempt shared by resume and the sink-health tick. The
/// test drives this exact function with a fake factory.
pub(super) fn open_sink(
    open: &Arc<OpenSink>,
    device: Option<&str>,
    recovery: &mut SinkRecovery,
    now: Instant,
) -> Result<Playout, FailureOutcome> {
    match Playout::start_with(device, open.clone()) {
        Ok(playout) => {
            recovery.opened(now);
            Ok(playout)
        }
        Err(_) => Err(recovery.record_failure(now)),
    }
}

/// The software room playout. The sink thread is released by dropping this
/// value; the media worker never joins it.
pub(super) struct Playout {
    sender: SyncSender<Msg>,
    clip: PlayoutClip,
    metrics: Arc<Metrics>,
    state: StateSlot,
    tap: Tap,
    open: Arc<OpenSink>,
    /// Non-blocking shutdown signal; `Drop` sets it instead of sending on a
    /// possibly full channel.
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    started: Instant,
}

/// A cheap handle a source task uses to push decoded frames.
#[derive(Clone)]
pub(super) struct PlayoutClip {
    sender: SyncSender<Msg>,
    metrics: Arc<Metrics>,
}

impl PlayoutClip {
    fn send(&self, message: Msg) -> bool {
        match self.sender.try_send(message) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.metrics.queue_dropped.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Queue one decoded 10 ms frame. A full channel drops this frame and keeps
    /// the source alive; only a closed playout ends the task.
    pub(super) fn push_frame(&self, track_id: &str, block: Vec<i16>) -> bool {
        self.send(Msg::Frame(track_id.to_owned(), block))
    }

    pub(super) fn remove_source(&self, track_id: &str) {
        let _ = self.send(Msg::Remove(track_id.to_owned()));
    }

    /// Attach a decoded remote track. The task drains the engine sink into the
    /// mixer until the track ends or the media session drops the handle.
    pub(super) fn attach(
        &self,
        track_id: String,
        track: RtcAudioTrack,
        runtime: tokio::runtime::Handle,
    ) -> tokio::task::JoinHandle<()> {
        let clip = self.clone();
        runtime.spawn(async move {
            let mut stream = NativeAudioStream::with_options(
                track,
                PLAYOUT_RATE as i32,
                1,
                NativeAudioStreamOptions {
                    queue_size_frames: Some(SOURCE_CHANNEL),
                },
            );
            while let Some(frame) = stream.next().await {
                if !clip.push_frame(&track_id, frame.data.to_vec()) {
                    break;
                }
            }
            stream.close();
            clip.remove_source(&track_id);
        })
    }
}

impl Playout {
    /// Injectable constructor used by the recovery tests. A stale selected
    /// endpoint falls back to the Windows default so the room stays audible;
    /// the caller reports the applied choice through its normal output event.
    pub(super) fn start_with(device: Option<&str>, open: Arc<OpenSink>) -> Result<Self, String> {
        let index = device
            .filter(|id| !id.is_empty())
            .and_then(resolve_output)
            .unwrap_or(WAVE_MAPPER);
        let sink = open(index, PLAYOUT_RATE)?;
        let (sender, receiver) = sync_channel(SOURCE_CHANNEL * 8);
        let metrics = Arc::new(Metrics::default());
        // A freshly opened sink is running until the thread proves otherwise;
        // the old `started < 250 ms` shortcut hid an immediate failure.
        metrics.running.store(true, Ordering::Relaxed);
        let worker_metrics = metrics.clone();
        let state = StateSlot::default();
        let worker_state = state.clone();
        let tap = Tap::default();
        let worker_tap = tap.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("voxhold-playout".into())
            .spawn(move || {
                run(
                    sink,
                    receiver,
                    worker_metrics,
                    worker_state,
                    worker_tap,
                    worker_stop,
                )
            })
            .map_err(|error| format!("Не удалось запустить микшер: {error}"))?;
        let clip = PlayoutClip {
            sender: sender.clone(),
            metrics: metrics.clone(),
        };
        Ok(Self {
            sender,
            clip,
            metrics,
            state,
            tap,
            open,
            stop,
            thread: Some(thread),
            started: Instant::now(),
        })
    }

    /// Switch the sink. The new device is opened on the media worker first, so
    /// a failure can be rolled back by the caller without touching the old
    /// sink. Returns whether the request was queued.
    pub(super) fn switch_sink(&self, device: Option<&str>) -> Result<(), String> {
        let index = match device.filter(|id| !id.is_empty()) {
            Some(id) => {
                resolve_output(id).ok_or("Выбранное устройство воспроизведения недоступно.")?
            }
            None => WAVE_MAPPER,
        };
        let sink = (self.open)(index, PLAYOUT_RATE)?;
        self.sender
            .try_send(Msg::Sink(sink))
            .map_err(|_| "Микшер воспроизведения недоступен.".to_owned())
    }

    pub(super) fn set_state(&self, mut state: MixState) {
        state.sources.sort_by(|a, b| a.0.cmp(&b.0));
        self.state.store(state);
    }

    pub(super) fn clip(&self) -> PlayoutClip {
        self.clip.clone()
    }

    /// Post-mix subscription for the W07 recorder. No consumer is wired in this
    /// stage; the seam is tested with the mixer's synthetic blocks.
    #[allow(dead_code)]
    pub(super) fn tap(&self) -> TapReceiver {
        self.tap.subscribe()
    }

    pub(super) fn stats(&self) -> PlayoutStats {
        self.metrics.snapshot()
    }

    pub(super) fn running(&self) -> bool {
        self.stats().running
    }

    /// A sink proves stability by living `STABLE_AFTER` and accepting
    /// `STABLE_BLOCKS` whole blocks since it was opened.
    pub(super) fn stable(&self, now: Instant) -> bool {
        now.duration_since(self.started) >= STABLE_AFTER
            && self.metrics.blocks_written.load(Ordering::Relaxed) >= STABLE_BLOCKS
    }

    #[cfg(test)]
    pub(super) fn written_blocks(&self) -> u64 {
        self.metrics.blocks_written.load(Ordering::Relaxed)
    }
}

impl Drop for Playout {
    fn drop(&mut self) {
        // Non-blocking: a full frame channel must not delay a leave. The thread
        // observes the flag within one 2 ms tick and closes the sink.
        self.stop.store(true, Ordering::SeqCst);
        // Detach: closing the WinMM sink may wait a few milliseconds for the
        // driver and must not delay a UI-driven leave.
        if let Some(thread) = self.thread.take() {
            drop(thread);
        }
    }
}

fn run(
    mut sink: Box<dyn Sink>,
    receiver: Receiver<Msg>,
    metrics: Arc<Metrics>,
    state: StateSlot,
    tap: Tap,
    stop: Arc<AtomicBool>,
) {
    let mut mixer = Mixer::default();
    let mut pool = SourcePool::default();
    let mut block = vec![0_i16; BLOCK_SAMPLES];
    let mut last_publish = Instant::now();
    'outer: loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let mut stopped = false;
        for _ in 0..64 {
            match receiver.recv_timeout(Duration::ZERO) {
                Ok(Msg::Remove(id)) => {
                    pool.remove(&id);
                    mixer.remove_source(&id);
                }
                Ok(Msg::Frame(id, frame)) => pool.push(&id, frame),
                Ok(Msg::Sink(next)) => {
                    let mut old = std::mem::replace(&mut sink, next);
                    old.close();
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    stopped = true;
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        if stopped {
            break 'outer;
        }
        if let Some(state) = state.take() {
            mixer.set_bus_gain(Bus::Voice, state.bus);
            let wanted: BTreeMap<&str, Source> = state
                .sources
                .iter()
                .map(|(id, source)| (id.as_str(), *source))
                .collect();
            mixer.clear_sources();
            for (id, source) in &wanted {
                mixer.set_source(id, *source);
                pool.ensure(id);
            }
            let stale: Vec<String> = pool
                .ids()
                .filter(|id| !wanted.contains_key(id.as_str()))
                .cloned()
                .collect();
            for id in stale {
                pool.remove(&id);
                mixer.remove_source(&id);
            }
        }
        if sink.failed() {
            break;
        }
        // The device clock paces the mixer: one 10 ms block per free buffer.
        let mut filled = 0;
        while sink.writable() && filled < 2 {
            let started = Instant::now();
            mixer.render(Bus::Voice, &mut pool, &mut block);
            mixer
                .stats_mut()
                .record_latency(started.elapsed().as_micros() as u32);
            // W07's recorder receives the limited room mix here.
            tap.emit(&block);
            if !sink.write(&block) {
                break;
            }
            metrics.blocks_written.fetch_add(1, Ordering::Relaxed);
            filled += 1;
            // Without attached sources, keep the device alive with silence but
            // do not spin through the buffers faster than real time.
            if pool.len() == 0 {
                break;
            }
        }
        if last_publish.elapsed() >= PUBLISH_INTERVAL {
            let latency = sink.buffered_blocks().saturating_mul(10);
            metrics.publish(&mixer, &pool, latency, !sink.failed());
            last_publish = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    metrics.publish(&mixer, &pool, 0, false);
    sink.close();
}

fn prune_finished(tasks: &mut Vec<tokio::task::JoinHandle<()>>) {
    tasks.retain(|task| !task.is_finished());
}

impl Session {
    /// Feed the current bus gain and per-source gains/mutes to the mixer. A
    /// full snapshot is stored as one last-value slot, so a busy frame channel
    /// cannot lose it.
    pub(super) fn push_playout_state(&self) {
        let Some(playout) = &self.playout else {
            return;
        };
        let bus = if self.controls.deafened {
            0.0
        } else {
            self.controls.output_gain.unwrap_or(1.0).clamp(0.0, 2.0)
        };
        let sources = self
            .sources
            .iter()
            .map(|(id, source)| (id.clone(), *source))
            .collect::<Vec<_>>();
        playout.set_state(MixState { bus, sources });
    }

    /// Switch the software sink. The new device is opened before the old sink
    /// is replaced; a failure leaves the previous audible path in place.
    pub(super) fn switch_playout(&mut self, id: &str) -> SwitchResult {
        let Some(playout) = &self.playout else {
            return SwitchResult {
                switched: false,
                audible: false,
                applied: self.output.applied.clone(),
            };
        };
        let target = (!id.is_empty()).then(|| id.to_owned());
        let opened = self.output.applied.clone();
        match playout.switch_sink(target.as_deref()) {
            Ok(()) => SwitchResult {
                switched: true,
                audible: true,
                applied: target,
            },
            Err(_) => SwitchResult {
                switched: false,
                audible: playout.running(),
                applied: opened,
            },
        }
    }

    /// Release the sink on suspend. The room is silent until a resume; the UI
    /// is told immediately that the mixer no longer runs.
    pub(super) fn suspend_playout(&mut self, events: &Events) {
        self.playout_suspended = true;
        if self.playout.take().is_some() {
            self.abort_playout_tasks();
        }
        self.report_audio(events);
    }

    /// Reopen the room sink after a resume. A failure is not final: the retry
    /// ladder is driven by `tick_playout`, and after the budget the session
    /// moves to the ADM fallback instead of staying silent.
    pub(super) fn resume_playout(&mut self, events: &Events) {
        self.playout_suspended = false;
        let now = std::time::Instant::now();
        if !self.software_playout {
            // The platform path owns playout on this session; force the normal
            // device poll instead of opening a second sink.
            self.device_check = now - Duration::from_secs(2);
            self.check_devices(events);
            return;
        }
        if self.playout_mode == PlayoutMode::AdmFallback {
            // The mixer budget was already spent. Reopening it while the ADM
            // plays would duplicate the room; only the ADM path is restored,
            // and its real PCM progress is checked by the watchdog tick.
            self.audio.set_playout(true);
            self.adm_playout_started_at = now;
            self.adm_playout_duration = None;
            self.adm_playout_progress_at = now;
            if !self.audio.running(false) {
                let started = self.audio.start(false);
                self.report_audio(events);
                if !started {
                    events.push(Event::DeviceError {
                        message: "Системное воспроизведение комнаты не восстановлено после сна.",
                    });
                }
            }
            return;
        }
        if self.playout.as_ref().is_some_and(Playout::running) {
            self.push_playout_state();
            return;
        }
        self.playout.take();
        self.abort_playout_tasks();
        self.playout_attempts.reset_for_resume(now);
        if !self.attempt_software_playout(events, RecoveryCause::Resume) {
            // The retry ladder owns the next attempt; the UI must stop saying
            // that the mixer applies gain in the meantime.
            self.report_audio(events);
        }
    }

    /// Periodic sink health and retry driver. Called from the media tick, not
    /// from the UI, and never busy-loops: every transition is counted once.
    pub(super) fn tick_playout(&mut self, events: &Events) {
        let now = std::time::Instant::now();
        self.prune_playout_tasks();
        match self.playout_mode {
            PlayoutMode::Software => {
                if self.playout_suspended {
                    return;
                }
                if let Some(playout) = &self.playout {
                    if playout.running() {
                        if self.playout_attempts.phase() == PlayoutPhase::Unstable
                            && playout.stable(now)
                        {
                            // Only proof of stability clears the shared budget.
                            self.playout_attempts.mark_stable();
                        }
                        if self.last_playout_report.elapsed() >= REPORT_INTERVAL {
                            self.last_playout_report = now;
                            let mut stats = playout.stats();
                            stats.reconnects = self.playout_reconnects;
                            stats.phase = self.playout_attempts.phase();
                            events.push(Event::Playout(stats));
                        }
                        return;
                    }
                    // The sink opened but died before proving stability. This
                    // is one state transition: one failure, one message, no
                    // reattach and no immediate reopen.
                    self.playout = None;
                    self.abort_playout_tasks();
                    let outcome = self.playout_attempts.sink_died(now);
                    events.push(Event::DeviceError {
                        message: "Воспроизведение комнаты остановилось; восстанавливаем.",
                    });
                    self.report_audio(events);
                    if outcome == FailureOutcome::AdmFallback {
                        self.engage_adm_fallback(events, RecoveryCause::Sink);
                        return;
                    }
                }
                if !self.playout_attempts.due(now) {
                    return;
                }
                self.attempt_software_playout(events, RecoveryCause::Sink);
            }
            PlayoutMode::AdmFallback => {
                if self.playout_suspended {
                    return;
                }
                if !self.adm_retry.due(now) {
                    return;
                }
                self.adm_retry.schedule(now, ADM_RETRY);
                if self.adm_playout_healthy(now) {
                    return;
                }
                // Initialized is not proof of PCM: restart a platform path that
                // stopped delivering played samples.
                if self.audio.running(false) {
                    let _ = self.audio.stop(false);
                }
                self.audio.set_playout(true);
                let started = self.audio.start(false);
                self.adm_playout_started_at = now;
                self.adm_playout_duration = None;
                self.adm_playout_progress_at = now;
                self.report_audio(events);
                events.push(Event::DeviceError {
                    message: if started {
                        "Системный ADM не отдаёт PCM; перезапускаем воспроизведение."
                    } else {
                        "Системное воспроизведение комнаты не восстановлено; повторяем."
                    },
                });
            }
        }
    }

    /// The ADM path is healthy only while its cumulative played duration moves.
    /// A startup grace covers the first stats report, exactly as the capture
    /// watchdog does.
    fn adm_playout_healthy(&self, now: std::time::Instant) -> bool {
        adm_playout_ok(
            self.audio.running(false),
            now.duration_since(self.adm_playout_started_at),
            self.adm_playout_duration.is_some(),
            self.adm_playout_progress_at.elapsed(),
        )
    }

    /// One bounded open attempt. A successful open is not yet a recovery: it
    /// moves to `Unstable` and the health tick proves it before the budget is
    /// cleared. Failures schedule the next attempt or move to the ADM fallback.
    pub(super) fn attempt_software_playout(
        &mut self,
        events: &Events,
        cause: RecoveryCause,
    ) -> bool {
        let now = std::time::Instant::now();
        let device = self
            .output
            .selected
            .clone()
            .or_else(|| self.output.applied.clone())
            .or_else(|| self.controls.output.clone());
        match open_sink(
            &self.playout_open,
            device.as_deref(),
            &mut self.playout_attempts,
            now,
        ) {
            Ok(playout) => {
                self.playout = Some(playout);
                self.playout_mode = PlayoutMode::Software;
                self.playout_reconnects = self.playout_reconnects.saturating_add(1);
                self.attach_existing_tracks();
                self.push_playout_state();
                self.report_audio(events);
                if let Some(playout) = &self.playout {
                    let mut stats = playout.stats();
                    stats.reconnects = self.playout_reconnects;
                    stats.phase = PlayoutPhase::Unstable;
                    events.push(Event::Playout(stats));
                }
                events.push(Event::DeviceError {
                    message: match cause {
                        RecoveryCause::Resume => {
                            "Звук комнаты открыт после сна; проверяем стабильность."
                        }
                        RecoveryCause::Sink => "Микшер открыт заново; проверяем стабильность.",
                    },
                });
                true
            }
            Err(FailureOutcome::Retry(_)) => {
                events.push(Event::DeviceError {
                    message: match cause {
                        RecoveryCause::Resume => "Не удалось возобновить звук комнаты; повторяем.",
                        RecoveryCause::Sink => "Не удалось восстановить микшер комнаты; повторяем.",
                    },
                });
                false
            }
            Err(FailureOutcome::AdmFallback) => {
                self.engage_adm_fallback(events, cause);
                false
            }
        }
    }

    /// The software mixer exhausted its budget. Re-enable the platform playout
    /// and tell the UI that only mute is applied from now on.
    fn engage_adm_fallback(&mut self, events: &Events, cause: RecoveryCause) {
        self.playout_mode = PlayoutMode::AdmFallback;
        self.playout_attempts.to_adm();
        self.playout = None;
        self.abort_playout_tasks();
        self.audio.set_playout(true);
        let started = self.audio.start(false);
        let now = std::time::Instant::now();
        self.adm_retry.schedule(now, ADM_RETRY);
        self.adm_playout_started_at = now;
        self.adm_playout_duration = None;
        self.adm_playout_progress_at = now;
        self.apply_audio_controls();
        self.report_audio(events);
        events.push(Event::DeviceError {
            message: match (cause, started) {
                (_, false) => {
                    "Микшер и системное воспроизведение не восстановились; звук комнаты недоступен."
                }
                (RecoveryCause::Resume, true) => {
                    "Микшер не восстановился после сна; звук комнаты идёт через системный ADM без усиления участников."
                }
                (RecoveryCause::Sink, true) => {
                    "Микшер не восстановился; звук комнаты идёт через системный ADM без усиления участников."
                }
            },
        });
    }

    fn abort_playout_tasks(&mut self) {
        if let Ok(mut tasks) = self.playout_tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }

    /// A finished track task is kept in the shared Vec until the call ends
    /// unless it is pruned; a call can add a task per renegotiation.
    fn prune_playout_tasks(&mut self) {
        if let Ok(mut tasks) = self.playout_tasks.lock() {
            prune_finished(&mut tasks);
        }
    }

    /// Every receiver that is still present gets a fresh engine sink on the new
    /// mixer. `on_track` is not repeated for those tracks after a system sleep.
    fn attach_existing_tracks(&mut self) {
        self.prune_playout_tasks();
        let Some(playout) = self.playout.as_ref().map(Playout::clip) else {
            return;
        };
        for receiver in self.peer.receivers() {
            if let Some(MediaStreamTrack::Audio(track)) = receiver.track() {
                let task = playout.attach(track.id(), track, self.runtime.clone());
                if let Ok(mut tasks) = self.playout_tasks.lock() {
                    tasks.push(task);
                }
            }
        }
    }
}

impl Session {
    pub(super) fn apply_audio_controls(&mut self) {
        if self.playout.is_some() {
            // The software mixer owns the gain and the local mute; the remote
            // track stays enabled so its sink keeps receiving decoded PCM.
            self.push_playout_state();
            return;
        }
        let master = if self.controls.deafened { 0.0 } else { 1.0 };
        // Without the software mixer the pinned wrapper exposes no numeric
        // gain, so only zero silences the track. The stored percentage is kept
        // for the mixer path and for the UI.
        let room_audible = self.controls.output_gain.unwrap_or(1.0) > 0.0;
        for receiver in self.peer.receivers() {
            if let Some(MediaStreamTrack::Audio(track)) = receiver.track() {
                let id = track.id();
                let source = self.sources.get(&id).copied().unwrap_or_default();
                let gain = (source.gain * master).clamp(0.0, 2.0);
                track.set_enabled(room_audible && !source.muted && gain > 0.0);
            }
        }
    }

    /// Effective gain of one receiver as the mixer sees it.
    #[allow(dead_code)]
    pub fn gain_for_track(&self, track_id: &str) -> f64 {
        let source = self.sources.get(track_id).copied().unwrap_or_default();
        if source.muted { 0.0 } else { source.gain }
    }

    /// True only while the software sink is actually open. The UI must use this
    /// rather than the start-of-session `AppliedAudio` flag.
    pub(super) fn software_mixer_active(&self) -> bool {
        self.playout_mode == PlayoutMode::Software
            && self.playout.as_ref().is_some_and(Playout::running)
    }

    /// True when the session owns the software path (avoids the ADM device
    /// check racing the retry ladder).
    pub(super) fn software_playout_mode(&self) -> bool {
        self.playout_mode == PlayoutMode::Software && self.software_playout
    }
}

#[cfg(test)]
mod tests;
