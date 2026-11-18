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
use crate::mixer::{BLOCK_SAMPLES, Bus, Mixer, PlayoutStats, Source, SourcePool};
use winmm_out::{OpenSink, Sink, WAVE_MAPPER, resolve_output};

/// 48 kHz mono, the same rate as the software capture and the tone.
pub(super) const PLAYOUT_RATE: u32 = 48_000;
/// Whole 10 ms frames a source may run ahead before the queue drops the oldest.
const SOURCE_CHANNEL: usize = 8;
/// How often the playout thread publishes its counters.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// Software-sink reopen budget. Each failure schedules the next attempt; the
/// fifth failure switches the session to the ADM fallback instead of retrying
/// forever.
pub(super) const MAX_OPEN_ATTEMPTS: u8 = 5;
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
    Stop,
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

/// One post-mix block tap. W07's recorder subscribes here instead of decoding
/// the same tracks a second time; each subscriber gets its own bounded queue.
#[derive(Clone, Default)]
pub(super) struct Tap {
    sender: Arc<Mutex<Vec<SyncSender<Vec<i16>>>>>,
}

pub(super) struct TapReceiver {
    receiver: Receiver<Vec<i16>>,
}

impl TapReceiver {
    #[allow(dead_code)]
    pub(super) fn try_recv(&self) -> Option<Vec<i16>> {
        self.receiver.try_recv().ok()
    }
}

impl Tap {
    fn subscribe(&self) -> TapReceiver {
        let (sender, receiver) = sync_channel(16);
        if let Ok(mut senders) = self.sender.lock() {
            senders.push(sender);
        }
        TapReceiver { receiver }
    }

    #[cfg(test)]
    fn subscribers(&self) -> usize {
        self.sender.lock().map(|senders| senders.len()).unwrap_or(0)
    }

    fn emit(&self, block: &[i16]) {
        let Ok(mut senders) = self.sender.lock() else {
            return;
        };
        senders.retain(|sender| sender.try_send(block.to_vec()).is_ok());
    }
}

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
        }
    }
}

/// Reopen scheduling shared by resume and sink failure. The unit test drives
/// exactly this type; the attempt count is bounded and the delay grows.
#[derive(Clone, Copy, Debug)]
pub(super) struct OpenAttempts {
    attempts: u8,
    next_at: Instant,
}

impl Default for OpenAttempts {
    fn default() -> Self {
        Self {
            attempts: 0,
            next_at: Instant::now(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailureOutcome {
    Retry(Duration),
    AdmFallback,
}

impl OpenAttempts {
    pub(super) fn reset(&mut self, now: Instant) {
        self.attempts = 0;
        self.next_at = now;
    }
    pub(super) fn due(&self, now: Instant) -> bool {
        now >= self.next_at
    }

    #[cfg(test)]
    pub(super) fn attempts(&self) -> u8 {
        self.attempts
    }

    /// Record one failed open. The first four failures schedule 0.5/1/2/4 s;
    /// the fifth spends the budget and asks for the ADM fallback.
    pub(super) fn record_failure(&mut self, now: Instant) -> FailureOutcome {
        self.attempts = self.attempts.saturating_add(1);
        if self.attempts >= MAX_OPEN_ATTEMPTS {
            return FailureOutcome::AdmFallback;
        }
        let delay = match self.attempts {
            1 => Duration::from_millis(500),
            2 => Duration::from_secs(1),
            3 => Duration::from_secs(2),
            _ => Duration::from_secs(4),
        };
        self.next_at = now + delay;
        FailureOutcome::Retry(delay)
    }
}

/// One bounded open attempt shared by resume and the sink-health tick. The
/// test drives this exact function with a fake factory.
pub(super) fn open_sink(
    open: &Arc<OpenSink>,
    device: Option<&str>,
    attempts: &mut OpenAttempts,
    now: Instant,
) -> Result<Playout, FailureOutcome> {
    match Playout::start_with(device, open.clone()) {
        Ok(playout) => {
            attempts.reset(now);
            Ok(playout)
        }
        Err(_) => Err(attempts.record_failure(now)),
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
        let worker_metrics = metrics.clone();
        let state = StateSlot::default();
        let worker_state = state.clone();
        let tap = Tap::default();
        let worker_tap = tap.clone();
        let thread = std::thread::Builder::new()
            .name("voxhold-playout".into())
            .spawn(move || run(sink, receiver, worker_metrics, worker_state, worker_tap))
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
        let mut stats = self.metrics.snapshot();
        // The thread publishes on its own cadence; right after open the
        // counters are still zero but the sink is live.
        if self.started.elapsed() < PUBLISH_INTERVAL {
            stats.running = true;
        }
        stats
    }

    pub(super) fn running(&self) -> bool {
        self.stats().running
    }
}

impl Drop for Playout {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Stop);
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
) {
    let mut mixer = Mixer::default();
    let mut pool = SourcePool::default();
    let mut block = vec![0_i16; BLOCK_SAMPLES];
    let mut last_publish = Instant::now();
    'outer: loop {
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
                Ok(Msg::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
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
        if !self.software_playout {
            // The platform path owns playout on this session; force the normal
            // device poll instead of opening a second sink.
            self.device_check = std::time::Instant::now() - Duration::from_secs(2);
            self.check_devices(events);
            return;
        }
        if self.playout_mode == PlayoutMode::AdmFallback {
            // The mixer budget was already spent. Reopening it while the ADM
            // plays would duplicate the room; only the ADM path is restored.
            self.audio.set_playout(true);
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
        self.playout_attempts.reset(std::time::Instant::now());
        if !self.attempt_software_playout(events, RecoveryCause::Resume) {
            // The retry ladder owns the next attempt; the UI must stop saying
            // that the mixer applies gain in the meantime.
            self.report_audio(events);
        }
    }

    /// Periodic sink health and retry driver. Called from the media tick, not
    /// from the UI, and never busy-loops: a failed open schedules a delay.
    pub(super) fn tick_playout(&mut self, events: &Events) {
        let now = std::time::Instant::now();
        match self.playout_mode {
            PlayoutMode::Software => {
                if self.playout_suspended {
                    return;
                }
                if let Some(playout) = &self.playout {
                    if playout.running() {
                        if self.last_playout_report.elapsed() >= REPORT_INTERVAL {
                            self.last_playout_report = now;
                            let mut stats = playout.stats();
                            stats.reconnects = self.playout_reconnects;
                            events.push(Event::Playout(stats));
                        }
                        return;
                    }
                    // The thread ended (device removed or a driver reset).
                    self.playout = None;
                    self.abort_playout_tasks();
                    self.playout_attempts.reset(now);
                    events.push(Event::DeviceError {
                        message: "Воспроизведение комнаты остановилось; восстанавливаем.",
                    });
                    self.report_audio(events);
                }
                if !self.playout_attempts.due(now) {
                    return;
                }
                self.attempt_software_playout(events, RecoveryCause::Sink);
            }
            PlayoutMode::AdmFallback => {
                if !self.adm_attempts.due(now) {
                    return;
                }
                self.adm_attempts.reset(now + ADM_RETRY);
                if self.audio.running(false) {
                    return;
                }
                let started = self.audio.start(false);
                self.report_audio(events);
                if !started {
                    events.push(Event::DeviceError {
                        message: "Системное воспроизведение комнаты не восстановлено; повторяем.",
                    });
                }
            }
        }
    }

    /// One bounded open attempt. `Ok` attaches the surviving receivers and
    /// reports the restored path; failures schedule the next attempt or move
    /// to the ADM fallback.
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
                    events.push(Event::Playout(stats));
                }
                events.push(Event::DeviceError {
                    message: match cause {
                        RecoveryCause::Resume => "Звук комнаты возобновлён после сна.",
                        RecoveryCause::Sink => "Звук комнаты восстановлен после остановки микшера.",
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
        self.playout = None;
        self.abort_playout_tasks();
        self.audio.set_playout(true);
        let started = self.audio.start(false);
        self.adm_attempts
            .reset(std::time::Instant::now() + ADM_RETRY);
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

    /// Every receiver that is still present gets a fresh engine sink on the new
    /// mixer. `on_track` is not repeated for those tracks after a system sleep.
    fn attach_existing_tracks(&mut self) {
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
