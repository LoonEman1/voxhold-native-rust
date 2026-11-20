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
    media_stream_track::RtcTrackState,
};
use tokio_stream::StreamExt;

use super::*;
use crate::mixer::{BLOCK_SAMPLES, Bus, Mixer, PlayoutPhase, PlayoutStats, Source, SourcePool};
use winmm_out::{OpenSink, Sink, WAVE_MAPPER, resolve_output};

/// 48 kHz mono, the same rate as the software capture and the tone.
pub(crate) const PLAYOUT_RATE: u32 = 48_000;
/// Whole 10 ms frames a source may run ahead before the queue drops the oldest.
const SOURCE_CHANNEL: usize = 8;
/// How often the playout thread publishes its counters.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// Software-sink reopen budget. Each failed open *and* each sink that opened
/// but died before proving stability spends one attempt; the fifth failure
/// switches the session to the ADM fallback instead of retrying forever.
pub(crate) const MAX_OPEN_ATTEMPTS: u8 = 5;
/// Stability proof: the sink must live this long and write this many blocks.
/// A driver can open and fail after the first buffer; two seconds covers a
/// device reset/unplug race, and 50 blocks (0.5 s) prove that PCM was accepted.
pub(crate) const STABLE_AFTER: Duration = Duration::from_secs(2);
pub(crate) const STABLE_BLOCKS: u64 = 50;
/// Slow retry of the ADM path when it could not be started.
const ADM_RETRY: Duration = Duration::from_secs(5);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);
/// How often an attachment task checks whether its remote track ended. The
/// native audio sink has no end callback, so an ended track would otherwise
/// park the task and its mixer source until the session drops.
const TRACK_STATE_POLL: Duration = Duration::from_secs(1);

/// Which playout path owns the room right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlayoutMode {
    Software,
    /// The software mixer could not be kept alive. The ADM plays the room and
    /// only mute is applied; the UI is told through `AppliedAudio`.
    AdmFallback,
}

/// Why the session is reopening the sink. Only the message differs, never the
/// retry policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryCause {
    Resume,
    Sink,
}

#[derive(Clone, Debug)]
pub(crate) struct MixState {
    pub(crate) bus: f64,
    pub(crate) sources: Vec<(String, Source)>,
}

enum Msg {
    Remove(String, u64),
    Frame(String, u64, Vec<i16>),
    Sink(Box<dyn Sink>),
}

/// Current attachment generation per track id. A re-offered track gets a new
/// generation; frames and the end marker of the older task are then dropped, so
/// the two receivers can never overlap in the mixer even before the aborted
/// task has stopped.
#[derive(Default)]
struct Epochs {
    current: std::collections::HashMap<String, u64>,
}

enum Epoch {
    /// The task was replaced; its frame or end marker is stale.
    Stale,
    /// The current task keeps pushing.
    Current,
    /// A newer attachment takes the id; the old queued PCM must be discarded.
    New,
}

impl Epochs {
    fn frame(&mut self, id: &str, epoch: u64) -> Epoch {
        match self.current.get(id) {
            Some(&current) if epoch < current => Epoch::Stale,
            Some(&current) if epoch == current => Epoch::Current,
            _ => {
                self.current.insert(id.to_owned(), epoch);
                Epoch::New
            }
        }
    }

    /// An end marker only clears the source of the attachment that owns it.
    fn ended(&mut self, id: &str, epoch: u64) -> bool {
        if self.current.get(id) == Some(&epoch) {
            self.current.remove(id);
            true
        } else {
            false
        }
    }
}

/// Apply one decoded frame for its attachment generation. A new generation may
/// discard the previous task's queued PCM, but never the participant's mixer
/// source: gain and mute belong to the participant, not to the decoded stream.
/// Returns whether the frame reached the source queue.
fn apply_frame(
    epochs: &mut Epochs,
    pool: &mut SourcePool,
    id: &str,
    epoch: u64,
    frame: Vec<i16>,
) -> bool {
    match epochs.frame(id, epoch) {
        Epoch::Stale => false,
        Epoch::Current => {
            pool.push(id, frame);
            true
        }
        Epoch::New => {
            pool.remove(id);
            pool.push(id, frame);
            true
        }
    }
}

/// One attachment generation for the whole process. The mixer worker compares
/// it per track id, so a late frame from a closed task is ignored.
static ATTACH_SERIAL: AtomicU64 = AtomicU64::new(0);

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
use tap::Tap;
pub(crate) use tap::TapReceiver;
mod tracks;
pub(crate) use tracks::TrackSet;

/// Identifies one mixer generation. A task recorded against an older serial
/// belongs to a replaced playout and must not be trusted again.
static MIXER_SERIAL: AtomicU64 = AtomicU64::new(0);

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
pub(crate) struct SinkRecovery {
    failures: u8,
    next_at: Instant,
    phase: PlayoutPhase,
}

impl Default for SinkRecovery {
    fn default() -> Self {
        Self {
            failures: 0,
            next_at: Instant::now(),
            phase: PlayoutPhase::Opening,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureOutcome {
    Retry(Duration),
    AdmFallback,
}

impl SinkRecovery {
    pub(crate) fn phase(&self) -> PlayoutPhase {
        self.phase
    }

    #[cfg(test)]
    pub(crate) fn failures(&self) -> u8 {
        self.failures
    }

    pub(crate) fn due(&self, now: Instant) -> bool {
        now >= self.next_at
    }

    /// A successful open is only the start of the proof window.
    pub(crate) fn opened(&mut self) {
        self.phase = PlayoutPhase::Unstable;
    }

    /// The sink lived long enough and wrote real blocks: the budget is clear.
    pub(crate) fn mark_stable(&mut self) {
        if self.phase != PlayoutPhase::Unstable {
            return;
        }
        self.failures = 0;
        self.phase = PlayoutPhase::Stable;
    }

    /// Resume keeps the spent budget: a resumed system must not get five fresh
    /// attempts for every suspend.
    pub(crate) fn reset_for_resume(&mut self, now: Instant) {
        self.phase = PlayoutPhase::Opening;
        self.next_at = now;
    }

    /// Record one failed open or one sink that died before proving stability.
    /// Both spend the same budget; the fifth failure asks for the ADM fallback.
    pub(crate) fn record_failure(&mut self, now: Instant) -> FailureOutcome {
        self.failures = self.failures.saturating_add(1);
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

    pub(crate) fn to_adm(&mut self) {
        self.phase = PlayoutPhase::AdmFallback;
    }

    /// A sink that opened and died before proving stability spends the same
    /// budget as an open that never succeeded. This is the only accounting for
    /// the "opened but died" path; removing it would make the ladder endless.
    pub(crate) fn sink_died(&mut self, now: Instant) -> FailureOutcome {
        self.record_failure(now)
    }
}

/// Slow retry slot for the platform path. It has no budget: the ADM is the
/// last audible path, so it keeps being restarted every few seconds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryAt {
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
    pub(crate) fn due(&self, now: Instant) -> bool {
        now >= self.next_at
    }

    pub(crate) fn schedule(&mut self, now: Instant, delay: Duration) {
        self.next_at = now + delay;
    }
}

/// ADM health: initialized is not enough when audio is actually expected. The
/// cumulative played duration must move; a startup grace covers the first
/// stats report, as for capture. A room with no inbound audio tracks is
/// *unknown*, not broken: restarting the ADM every few seconds in an empty
/// room would only add noise and messages.
pub(crate) fn adm_playout_ok(
    running: bool,
    inbound_audio: bool,
    since_start: Duration,
    duration_seen: bool,
    since_progress: Duration,
) -> bool {
    if !running {
        return false;
    }
    if !inbound_audio {
        return true;
    }
    if since_start < INPUT_CAPTURE_GRACE {
        return true;
    }
    // A missing stats report is also unknown; only stale, previously observed
    // progress proves that the platform path stopped delivering PCM.
    !duration_seen || since_progress < Duration::from_secs(2)
}

/// One bounded open attempt shared by resume and the sink-health tick. The
/// test drives this exact function with a fake factory.
pub(crate) fn open_sink(
    open: &Arc<OpenSink>,
    device: Option<&str>,
    recovery: &mut SinkRecovery,
    now: Instant,
) -> Result<Playout, FailureOutcome> {
    match Playout::start_with(device, open.clone()) {
        Ok(playout) => {
            recovery.opened();
            Ok(playout)
        }
        Err(_) => Err(recovery.record_failure(now)),
    }
}

/// The software room playout. The sink thread is released by dropping this
/// value; the media worker never joins it.
pub(crate) struct Playout {
    sender: SyncSender<Msg>,
    clip: PlayoutClip,
    metrics: Arc<Metrics>,
    state: StateSlot,
    tap: Tap,
    open: Arc<OpenSink>,
    /// Generation of this mixer, used to tell a current attachment task from
    /// one bound to a replaced playout.
    serial: u64,
    /// Non-blocking shutdown signal; `Drop` sets it instead of sending on a
    /// possibly full channel.
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Proof window of the current sink: `since` and the cumulative written
    /// counter at that moment. Replacing the sink restarts the window, so a
    /// new (possibly dead) device cannot inherit the old proof.
    stability: Arc<Mutex<Stability>>,
}

#[derive(Clone, Copy, Debug)]
struct Stability {
    since: Instant,
    blocks: u64,
}

/// A cheap handle a source task uses to push decoded frames.
#[derive(Clone)]
pub(crate) struct PlayoutClip {
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
    pub(crate) fn push_frame(&self, track_id: &str, epoch: u64, block: Vec<i16>) -> bool {
        self.send(Msg::Frame(track_id.to_owned(), epoch, block))
    }

    pub(crate) fn remove_source(&self, track_id: &str, epoch: u64) {
        // The task is ending, so the marker must not be treated as another
        // dropped frame: a full channel would otherwise keep the mixer source
        // (and the finished attachment) forever. A closed playout has nothing
        // to remove.
        let _ = self.sender.send(Msg::Remove(track_id.to_owned(), epoch));
    }

    /// Attach a decoded remote track. The task drains the engine sink into the
    /// mixer until the track ends or the media session drops the handle.
    pub(crate) fn attach(
        &self,
        track_id: String,
        track: RtcAudioTrack,
        runtime: tokio::runtime::Handle,
    ) -> tokio::task::JoinHandle<()> {
        let clip = self.clone();
        let epoch = ATTACH_SERIAL.fetch_add(1, Ordering::Relaxed) + 1;
        runtime.spawn(async move {
            // The sink produces no end callback; a clone of the track lets the
            // task notice `Ended` instead of waiting for a closed queue forever.
            let state = track.clone();
            let mut stream = NativeAudioStream::with_options(
                track,
                PLAYOUT_RATE as i32,
                1,
                NativeAudioStreamOptions {
                    queue_size_frames: Some(SOURCE_CHANNEL),
                },
            );
            let mut poll = tokio::time::interval(TRACK_STATE_POLL);
            poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    // `next` only polls the frame queue, so dropping its future
                    // on the state tick loses no frame.
                    frame = stream.next() => match frame {
                        Some(frame) => {
                            if !clip.push_frame(&track_id, epoch, frame.data.to_vec()) {
                                break;
                            }
                        }
                        None => break,
                    },
                    _ = poll.tick() => {
                        if state.state() == RtcTrackState::Ended {
                            break;
                        }
                    }
                }
            }
            stream.close();
            clip.remove_source(&track_id, epoch);
        })
    }
}

impl Playout {
    /// Injectable constructor used by the recovery tests. A stale selected
    /// endpoint falls back to the Windows default so the room stays audible;
    /// the caller reports the applied choice through its normal output event.
    pub(crate) fn start_with(device: Option<&str>, open: Arc<OpenSink>) -> Result<Self, String> {
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
            serial: MIXER_SERIAL.fetch_add(1, Ordering::Relaxed) + 1,
            stop,
            thread: Some(thread),
            stability: Arc::new(Mutex::new(Stability {
                since: Instant::now(),
                blocks: 0,
            })),
        })
    }

    /// Switch the sink. The new device is opened on the media worker first, so
    /// a failure can be rolled back by the caller without touching the old
    /// sink. Returns whether the request was queued.
    pub(crate) fn switch_sink(&self, device: Option<&str>) -> Result<(), String> {
        let index = match device.filter(|id| !id.is_empty()) {
            Some(id) => {
                resolve_output(id).ok_or("Выбранное устройство воспроизведения недоступно.")?
            }
            None => WAVE_MAPPER,
        };
        let sink = (self.open)(index, PLAYOUT_RATE)?;
        self.sender
            .try_send(Msg::Sink(sink))
            .map_err(|_| "Микшер воспроизведения недоступен.".to_owned())?;
        // The new device has to prove itself from scratch; the old sink's age
        // and written blocks must not carry over.
        if let Ok(mut stability) = self.stability.lock() {
            *stability = Stability {
                since: Instant::now(),
                blocks: self.metrics.blocks_written.load(Ordering::Relaxed),
            };
        }
        Ok(())
    }

    pub(crate) fn set_state(&self, mut state: MixState) {
        state.sources.sort_by(|a, b| a.0.cmp(&b.0));
        self.state.store(state);
    }

    pub(crate) fn clip(&self) -> PlayoutClip {
        self.clip.clone()
    }

    pub(crate) fn serial(&self) -> u64 {
        self.serial
    }

    /// Post-mix subscription for the W07 recorder. No consumer is wired in this
    /// stage; the seam is tested with the mixer's synthetic blocks.
    #[allow(dead_code)]
    pub(crate) fn tap(&self) -> TapReceiver {
        self.tap.subscribe()
    }

    pub(crate) fn stats(&self) -> PlayoutStats {
        self.metrics.snapshot()
    }

    pub(crate) fn running(&self) -> bool {
        self.stats().running
    }

    /// A sink proves stability by living `STABLE_AFTER` and accepting
    /// `STABLE_BLOCKS` whole blocks since the current sink was installed.
    pub(crate) fn stable(&self, now: Instant) -> bool {
        let Ok(stability) = self.stability.lock() else {
            return false;
        };
        now.duration_since(stability.since) >= STABLE_AFTER
            && self
                .metrics
                .blocks_written
                .load(Ordering::Relaxed)
                .saturating_sub(stability.blocks)
                >= STABLE_BLOCKS
    }

    #[cfg(test)]
    pub(crate) fn written_blocks(&self) -> u64 {
        self.metrics.blocks_written.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn test_rewind_start(&self, by: Duration) {
        if let Ok(mut stability) = self.stability.lock() {
            stability.since = stability.since.checked_sub(by).unwrap_or(stability.since);
        }
    }

    #[cfg(test)]
    pub(crate) fn test_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
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
    let mut epochs = Epochs::default();
    let mut block = vec![0_i16; BLOCK_SAMPLES];
    let mut last_publish = Instant::now();
    'outer: loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let mut stopped = false;
        for _ in 0..64 {
            match receiver.recv_timeout(Duration::ZERO) {
                Ok(Msg::Remove(id, epoch)) => {
                    if epochs.ended(&id, epoch) {
                        pool.remove(&id);
                        mixer.remove_source(&id);
                    }
                }
                Ok(Msg::Frame(id, epoch, frame)) => {
                    apply_frame(&mut epochs, &mut pool, &id, epoch, frame);
                }
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

mod session;

#[cfg(test)]
mod tests;
