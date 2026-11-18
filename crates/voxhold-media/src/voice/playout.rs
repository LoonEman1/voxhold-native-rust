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
//! If the sink cannot open, the caller keeps the ADM playout and must report
//! that the numeric gain is not applied. A source that stops delivering while
//! it was recently active increments the starvation counter instead of silently
//! stretching the last block.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
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
use winmm_out::{WAVE_MAPPER, WinOut, resolve_output};

/// 48 kHz mono, the same rate as the software capture and the PDF of the tone.
pub(super) const PLAYOUT_RATE: u32 = 48_000;
/// Whole 10 ms frames a source may run ahead before the queue drops the oldest.
const SOURCE_CHANNEL: usize = 8;
/// How often the playout thread publishes its counters.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub(super) struct MixState {
    pub(super) bus: f64,
    pub(super) sources: Vec<(String, Source)>,
}

enum Msg {
    State(MixState),
    Remove(String),
    Frame(String, Vec<i16>),
    Sink(Box<WinOut>),
    Stop,
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
    running: AtomicBool,
}

impl Metrics {
    fn publish(&self, mixer: &Mixer, pool: &SourcePool, running: bool) {
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
        self.running.store(running, Ordering::Relaxed);
    }

    fn snapshot(&self) -> PlayoutStats {
        PlayoutStats {
            level: f32::from_bits(self.level.load(Ordering::Relaxed)),
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
            underruns: self.underruns.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            p95_micros: self.p95_micros.load(Ordering::Relaxed),
            running: self.running.load(Ordering::Relaxed),
        }
    }
}

/// The software room playout. The sink thread is released by dropping this
/// value; the media worker never joins it.
pub(super) struct Playout {
    sender: SyncSender<Msg>,
    clip: PlayoutClip,
    metrics: Arc<Metrics>,
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
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Queue one decoded 10 ms frame for a receiver track.
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
    /// Open the sink before any source is attached. The caller only disables
    /// the ADM playout after this returns `Ok`. A stale selected endpoint falls
    /// back to the Windows default so the room stays audible; the caller
    /// reports that through its normal output event.
    pub(super) fn start(device: Option<&str>) -> Result<Self, String> {
        let index = device
            .filter(|id| !id.is_empty())
            .and_then(resolve_output)
            .unwrap_or(WAVE_MAPPER);
        Self::start_index(index)
    }

    fn start_index(index: u32) -> Result<Self, String> {
        let sink = WinOut::open(index, PLAYOUT_RATE)?;
        let (sender, receiver) = sync_channel(SOURCE_CHANNEL * 8);
        let metrics = Arc::new(Metrics::default());
        let worker_metrics = metrics.clone();
        let thread = std::thread::Builder::new()
            .name("voxhold-playout".into())
            .spawn(move || run(sink, receiver, worker_metrics))
            .map_err(|error| format!("Не удалось запустить микшер: {error}"))?;
        let clip = PlayoutClip {
            sender: sender.clone(),
            metrics: metrics.clone(),
        };
        Ok(Self {
            sender,
            clip,
            metrics,
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
        let sink = WinOut::open(index, PLAYOUT_RATE)?;
        self.sender
            .try_send(Msg::Sink(Box::new(sink)))
            .map_err(|_| "Микшер воспроизведения недоступен.".to_owned())
    }

    pub(super) fn set_state(&self, mut state: MixState) {
        state.sources.sort_by(|a, b| a.0.cmp(&b.0));
        let _ = self.sender.try_send(Msg::State(state));
    }

    pub(super) fn clip(&self) -> PlayoutClip {
        self.clip.clone()
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

fn run(initial: WinOut, receiver: Receiver<Msg>, metrics: Arc<Metrics>) {
    let mut sink = initial;
    let mut mixer = Mixer::default();
    let mut pool = SourcePool::default();
    let mut block = vec![0_i16; BLOCK_SAMPLES];
    let mut last_publish = Instant::now();
    'outer: loop {
        let mut stopped = false;
        for _ in 0..64 {
            match receiver.recv_timeout(Duration::ZERO) {
                Ok(Msg::State(state)) => {
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
                Ok(Msg::Remove(id)) => {
                    pool.remove(&id);
                    mixer.remove_source(&id);
                }
                Ok(Msg::Frame(id, frame)) => pool.push(&id, frame),
                Ok(Msg::Sink(next)) => {
                    let mut old = std::mem::replace(&mut sink, *next);
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
            // W07 recording subscribes here: `block` is the limited room mix
            // after every per-user gain and mute, before it reaches the device.
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
            metrics.publish(&mixer, &pool, !sink.failed());
            last_publish = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    metrics.publish(&mixer, &pool, false);
    sink.close();
}

impl Session {
    /// Feed the current bus gain and per-source gains/mutes to the mixer. A
    /// full snapshot is sent so a source that disappeared can be dropped and
    /// the local mute stays a separate flag.
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

    /// Switch the software sink. The new WinMM device is opened before the old
    /// sink is replaced; a failure leaves the previous audible path in place.
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

    /// Release the sink on suspend. The ADM playout is already disabled for a
    /// room session, so the room is silent until `resume_playout`.
    pub(super) fn suspend_playout(&mut self) {
        if self.playout.take().is_some() {
            self.abort_playout_tasks();
        }
    }

    /// Reopen the room sink after a resume or a sink failure and re-attach the
    /// receivers that survived. The ADM path (a viewer, or the fallback where
    /// the mixer never opened) is re-checked through the regular device poll.
    pub(super) fn resume_playout(&mut self, events: &Events) {
        if !self.software_playout {
            // The platform path owns playout on this session; force the normal
            // device poll instead of opening a second sink.
            self.device_check = std::time::Instant::now() - Duration::from_secs(2);
            self.check_devices(events);
            return;
        }
        if self.playout.as_ref().is_some_and(Playout::running) {
            self.push_playout_state();
            return;
        }
        self.playout.take();
        self.abort_playout_tasks();
        let device = self
            .output
            .applied
            .clone()
            .or_else(|| self.controls.output.clone());
        match Playout::start(device.as_deref()) {
            Ok(playout) => {
                self.playout = Some(playout);
                self.attach_existing_tracks();
                self.push_playout_state();
                events.push(Event::DeviceError {
                    message: "Звук комнаты возобновлён после сна.",
                });
            }
            Err(_) => events.push(Event::DeviceError {
                message: "Не удалось возобновить звук комнаты после сна.",
            }),
        }
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
}
