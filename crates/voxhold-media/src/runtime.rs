//! Background voice session. The UI never blocks on the socket or a media
//! engine: it writes bounded last-value requests and reads coalesced updates.
//!
//! High-frequency values (controls, stream mute, viewport, frames, levels,
//! rosters) keep only the latest value, so a burst of input cannot grow a
//! queue. User actions are bounded too; `leave` outranks everything and a
//! watch/close pair shares one last-value slot, so an old close can never
//! overtake a newer selection.

use crate::{
    Command, Controls, Event, IceServer, Media, VideoFrame,
    controller::{Action, Publication, Signal, VoiceController},
    stream::StreamViewer,
    viewer::{Outcome as ViewerOutcome, Viewer},
    voice::CaptureGate,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const LOCAL_MUTE_CAPACITY: usize = 64;
const FRAMES_PER_TICK: usize = 4;
const DEFAULT_VIEWPORT: (u32, u32) = (960, 540);
const RECONNECT_DELAY: Duration = Duration::from_secs(3);
const MAX_RECONNECTS: u32 = 5;
const LONG_SESSION: Duration = Duration::from_secs(30);
/// A blocked socket send must not keep the media lease alive for the whole
/// library-level eight-second timeout.
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
/// The leave handshake is best effort; the capture is already latched.
const LEAVE_SEND_TIMEOUT: Duration = Duration::from_millis(750);

mod updates;
use updates::Updates;
pub use updates::VoiceUpdate;

mod engine;
mod player;
use player::install_stream;
mod preflight;
mod prep;
mod recovery;
mod requests;
mod screen;
mod session;
mod test_support;
use engine::{EngineEffect, apply_engine_update};
use recovery::{PublishRecovery, RecoveredAttempt, Rotation};
use requests::{
    InputRequest, Pending, Requests, ScreenAccess, StreamCommand, set_input_request,
    set_stream_command,
};
use session::run;

pub struct StreamFrame {
    /// Selection that produced this frame. The panel ignores a frame from an
    /// older selection even if it was already in the slot during a switch.
    pub selection: u64,
    pub width: u32,
    pub height: u32,
    /// BGRA pixels ready for `RenderImage`; the UI must not swap channels.
    pub bgra: Vec<u8>,
}

pub struct VoiceHandle {
    requests: Arc<Requests>,
    updates: Arc<Updates>,
    frame: Arc<Mutex<Option<StreamFrame>>>,
    /// Capture permission is independent of the socket worker, so leave, a
    /// released PTT key or logout silences the track immediately.
    capture: Arc<CaptureGate>,
    /// Last local preview of the screen publication. The publisher engine
    /// writes it; the on-air card takes it.
    screen_preview: Arc<crate::screen_capture::PreviewStore>,
    /// Ids for the picker and its listings. `u64` so a stale answer can never
    /// replace a newer one.
    screen_picker: Arc<AtomicU64>,
    source_generation: Arc<AtomicU64>,
    attempt: u64,
    worker: Option<thread::JoinHandle<()>>,
    finished: Arc<(Mutex<bool>, Condvar)>,
}

impl VoiceHandle {
    pub fn join(
        client: voxhold_api::Client,
        server: i64,
        channel: i64,
        controls: Controls,
        self_muted: bool,
    ) -> Result<Self, String> {
        Self::join_with_rotation(client, server, channel, controls, self_muted, false)
    }

    /// Join while a token refresh is already in flight. The gate is installed
    /// on the shared requests before the worker thread starts, so its first
    /// authorization waits for the refresh answer (or the 30 s deadline)
    /// instead of using the revoked token.
    pub fn join_with_rotation(
        client: voxhold_api::Client,
        server: i64,
        channel: i64,
        controls: Controls,
        self_muted: bool,
        rotation_pending: bool,
    ) -> Result<Self, String> {
        static ATTEMPT: AtomicU64 = AtomicU64::new(1);
        let attempt = ATTEMPT.fetch_add(1, Ordering::SeqCst);
        let requests = Arc::new(Requests::default());
        if rotation_pending {
            requests.rotation_started(Instant::now());
        }
        let updates = Arc::new(Updates::new());
        let frame = Arc::new(Mutex::new(None));
        let capture = Arc::new(CaptureGate::default());
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let screen_preview = Arc::new(crate::screen_capture::PreviewStore::default());
        let screen_picker = Arc::new(AtomicU64::new(1));
        let source_generation = Arc::new(AtomicU64::new(1));
        let (worker_requests, worker_updates) = (requests.clone(), updates.clone());
        let (worker_frame, worker_finished) = (frame.clone(), finished.clone());
        let worker_capture = capture.clone();
        let worker_preview = screen_preview.clone();
        let worker = thread::Builder::new()
            .name("voxhold-voice-session".into())
            .spawn(move || {
                run(
                    client,
                    server,
                    channel,
                    controls,
                    self_muted,
                    worker_requests,
                    worker_updates,
                    worker_frame,
                    worker_capture,
                    worker_preview,
                );
                if let Ok(mut done) = worker_finished.0.lock() {
                    *done = true;
                    worker_finished.1.notify_all();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            requests,
            updates,
            frame,
            capture,
            screen_preview,
            screen_picker,
            source_generation,
            attempt,
            worker: Some(worker),
            finished,
        })
    }

    /// Identity of this join attempt; stale updates of an older handle are
    /// discarded by the panel.
    pub fn attempt(&self) -> u64 {
        self.attempt
    }

    pub fn controls(&self, controls: Controls, self_muted: bool) {
        // The panel folds join, server mute, local mute and push-to-talk into
        // `controls.muted`. Apply that decision to the live track before the
        // worker is even scheduled; a blocked socket must not hold the mic.
        self.capture
            .set_local(!controls.muted && !controls.deafened);
        if let Ok(mut slot) = self.requests.controls.lock() {
            *slot = Some((controls, self_muted));
        }
        self.requests.wake();
    }

    /// An explicit output choice from settings. Generic control updates may
    /// skip an unchanged value, but this request is always forwarded to the
    /// media engine, whose own output state decides whether it is a no-op.
    pub fn select_output(&self, output: Option<String>) {
        let output = output.filter(|id| !id.is_empty());
        if let Ok(mut slot) = self.requests.output.lock() {
            *slot = Some(output);
        }
        self.requests.wake();
    }

    /// The refresh request started. The worker must not re-authorize the old
    /// socket with the revoked token while the answer is in flight.
    pub fn token_rotation_started(&self) {
        self.requests.rotation_started(Instant::now());
        self.requests.wake();
    }

    /// The refresh result was applied to every clone of the shared client. A
    /// waiting reconnect may now open its one new socket.
    pub fn token_rotation_finished(&self) {
        self.requests.rotation_finished();
        self.requests.wake();
    }

    /// Explicit input selection. The request travels in its own slot with a
    /// monotonically increasing generation, so a later PTT/DSP `controls`
    /// update cannot displace it and a late result cannot outrank a newer
    /// choice in the UI.
    pub fn select_input(&self, input: Option<String>) -> u64 {
        static GENERATION: AtomicU64 = AtomicU64::new(1);
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst);
        let input = input.filter(|id| !id.is_empty());
        if let Ok(mut slot) = self.requests.input.lock() {
            set_input_request(&mut slot, InputRequest { input, generation });
        }
        self.requests.wake();
        generation
    }

    pub fn local_mute(&self, connection: String, muted: bool) {
        if let Ok(mut queue) = self.requests.local_mutes.lock() {
            if queue.len() >= LOCAL_MUTE_CAPACITY {
                queue.pop_front();
            }
            queue.push_back((connection, muted));
        }
        self.requests.wake();
    }

    /// System power transition. The worker reopens or releases the playout
    /// sink; a resume before the first session is a no-op for the engine.
    pub fn power(&self, resumed: bool) {
        if let Ok(mut slot) = self.requests.power.lock() {
            *slot = Some(resumed);
        }
        self.requests.wake();
    }

    pub fn take_frame(&self) -> Option<StreamFrame> {
        self.frame.lock().ok()?.take()
    }

    /// Physical pixels of the player picture; the decoder scales to this.
    /// A repeat of the current size is ignored, but a real change wakes the
    /// worker so it re-reads the latest frame without waiting for a socket
    /// event or a new RTP frame.
    pub fn set_viewport(&self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let Ok(mut slot) = self.requests.viewport.lock() else {
            return;
        };
        if *slot == Some((width, height)) {
            return;
        }
        *slot = Some((width, height));
        // Release the slot before waking: the worker reads it at the top of its
        // loop and must not contend with a writer that would not change it.
        drop(slot);
        self.requests.wake();
    }

    pub fn try_update(&self) -> Option<VoiceUpdate> {
        self.updates.poll()
    }

    /// Selecting another publication is a last-value intent: if the worker has
    /// not picked up B yet and the user chooses C, only C is applied. A close
    /// is the same slot, so it cannot erase a newer watch from another thread.
    pub fn watch_stream(&self, selection: u64, publication: Publication) {
        set_stream_command(
            &self.requests,
            StreamCommand::Watch {
                selection,
                publication,
            },
        );
        if let Ok(mut frame) = self.frame.lock() {
            frame.take();
        }
        self.requests.wake();
    }

    pub fn leave_stream(&self, selection: u64) {
        set_stream_command(&self.requests, StreamCommand::Close { selection });
        if let Ok(mut frame) = self.frame.lock() {
            frame.take();
        }
        self.requests.wake();
    }

    pub fn stream_mute(&self, selection: u64, muted: bool) {
        if let Ok(mut slot) = self.requests.stream_mute.lock() {
            *slot = Some((selection, muted));
        }
        self.requests.wake();
    }

    pub fn leave(&self) {
        // Latch first: the flag stops the worker, but the track must be silent
        // now, even while `socket.send` is still waiting.
        self.capture.latch();
        self.requests.cancel_publish_recovery();
        self.requests.leave.store(true, Ordering::SeqCst);
        if let Ok(mut stream) = self.requests.stream.lock() {
            stream.clear();
        }
        self.requests.wake();
    }
}

impl Drop for VoiceHandle {
    fn drop(&mut self) {
        self.leave();
        let Some(worker) = self.worker.take() else {
            return;
        };
        // The UI thread must never wait for the socket. The worker latches the
        // capture synchronously above; joining happens on a short-lived thread.
        let finished = self.finished.clone();
        let _ = thread::Builder::new()
            .name("voxhold-voice-close".into())
            .spawn(move || {
                let done = finished
                    .0
                    .lock()
                    .map(|done| {
                        finished
                            .1
                            .wait_timeout_while(done, Duration::from_secs(3), |done| !*done)
                            .map(|(done, _)| *done)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if done {
                    let _ = worker.join();
                }
            });
    }
}

#[derive(Debug)]
enum SessionEnd {
    Leave,
    Recover,
    Error(String),
}

struct Live {
    socket: voxhold_api::VoiceSocket,
    media: Media,
    servers: Vec<IceServer>,
    policy: String,
    output: Option<String>,
    output_working: bool,
    stream: Option<StreamViewer>,
    stream_selection: Option<u64>,
    /// Screen-share state machine of this socket session. It survives viewer
    /// switches and never survives a reconnect.
    publisher: crate::publisher::Publisher,
    /// The capture and peer of the current publishing attempt.
    screen: Option<crate::publisher::engine::PublisherEngine>,
    /// The newest live bitrate change that the worker has not accepted yet. A
    /// full command queue defers it to the next tick instead of failing.
    bitrate: screen::BitrateQueue,
    /// The one preparation that may be opening a capture or a peer. The session
    /// loop only observes and cancels it.
    prep: Option<prep::ScreenPrep<crate::publisher::engine::PublisherEngine>>,
    /// A preflight of a live quality change. The live engine above keeps
    /// running until this confirms the new capture and the publisher commits
    /// the existing stop+start.
    restart: Option<preflight::PendingRestart>,
    /// Room identity a preparation result must still match.
    server: i64,
    channel: i64,
    connection: Option<String>,
    joined: bool,
    /// The fresh streams snapshot of this socket is authoritative.
    snapshot_ready: bool,
    /// The panel confirmed the screen gate for this socket session.
    access_seen: bool,
    /// Panel-confirmed gate of the current picker/attempt.
    access: ScreenAccess,
    /// Freshest gate written by `VoiceHandle::screen_access`.
    screen_access: Arc<Mutex<Option<ScreenAccess>>>,
    /// Confirmed broadcast captured before this session ends.
    recovery: Arc<PublishRecovery>,
    /// Cancellation epoch the current live attempt was confirmed under.
    recovery_epoch: u64,
    /// The intent this session took and has not confirmed yet.
    recovered: RecoveredAttempt,
    cancel_epoch: Arc<AtomicU64>,
    /// Rotation owns the reconnect of this credential.
    rotation: Arc<Mutex<Rotation>>,
    /// Credential this socket was opened with; a different current token means
    /// an old-generation close must not end the call.
    session_token: String,
    screen_preview: Arc<crate::screen_capture::PreviewStore>,
    render: VideoFrame,
    viewport: (u32, u32),
    local_level: Option<f64>,
    frame: Arc<Mutex<Option<StreamFrame>>>,
    updates: Arc<Updates>,
}

impl Live {
    async fn send(
        &mut self,
        kind: &str,
        data: serde_json::Value,
        id: Option<&str>,
    ) -> Result<(), SessionEnd> {
        let sent = tokio::time::timeout(SEND_TIMEOUT, self.socket.send(kind, data, id)).await;
        match sent {
            Ok(Ok(())) => Ok(()),
            _ => Err(SessionEnd::Recover),
        }
    }

    /// The leave handshake has its own short budget: the capture is already
    /// closed, so a stalled socket must not delay the UI.
    async fn send_leave(&mut self) {
        let _ = tokio::time::timeout(
            LEAVE_SEND_TIMEOUT,
            self.socket.send("voice.leave", serde_json::json!({}), None),
        )
        .await;
    }

    fn clear_frame(&self) {
        if let Ok(mut frame) = self.frame.lock() {
            frame.take();
        }
    }

    async fn drain_voice_media(
        &mut self,
        controller: &mut VoiceController,
    ) -> Result<bool, SessionEnd> {
        while let Some(event) = self.media.poll(Duration::ZERO) {
            if matches!(event, Event::State { state: "connected" }) {
                self.updates.push(VoiceUpdate::Connected);
            }
            if let Event::InputApplied {
                generation,
                input,
                error,
            } = &event
            {
                self.updates.push(VoiceUpdate::InputApplied {
                    generation: *generation,
                    input: input.clone(),
                    error: error.map(str::to_owned),
                });
            }
            let speaking = controller.speaking.clone();
            let tracks = controller.connections_with_audio();
            let applied = controller.applied_audio.clone();
            let playout = controller.playout;
            let actions = controller.media_event(event);
            if !self.execute(actions).await? {
                return Ok(false);
            }
            if controller.applied_audio != applied {
                if let Some(applied) = controller.applied_audio.clone() {
                    self.updates.push(VoiceUpdate::AudioMode(applied));
                }
            }
            if controller.playout != playout {
                if let Some(stats) = controller.playout {
                    self.updates.push(VoiceUpdate::Playout(stats));
                }
            }
            if controller.speaking != speaking {
                self.updates
                    .push(VoiceUpdate::Speaking(controller.speaking.clone()));
            }
            if controller.connections_with_audio() != tracks {
                self.updates
                    .push(VoiceUpdate::Tracks(controller.connections_with_audio()));
            }
            if let Some(last) = self.local_level {
                if (last - controller.local_level).abs() > 0.005 {
                    self.local_level = Some(controller.local_level);
                    self.updates
                        .push(VoiceUpdate::LocalLevel(controller.local_level));
                }
            } else {
                self.local_level = Some(controller.local_level);
            }
        }
        Ok(true)
    }

    async fn drain_viewer(&mut self, viewer: &mut Viewer) -> Result<bool, SessionEnd> {
        let (width, height) = self.viewport;
        for _ in 0..FRAMES_PER_TICK {
            let update = {
                let Self { stream, render, .. } = self;
                let Some(engine) = stream.as_mut() else {
                    return Ok(true);
                };
                engine.poll(render, width, height)
            };
            let Some(update) = update else {
                return Ok(true);
            };
            let now = Instant::now();
            let outcomes =
                match apply_engine_update(&self.frame, &mut self.render, viewer, update, now) {
                    EngineEffect::Outcomes(outcomes) => outcomes,
                    EngineEffect::Output {
                        applied,
                        working,
                        error,
                    } => {
                        // The applied endpoint may be the previous one after a
                        // rollback. Updating both values lets a repeated request
                        // retry instead of being skipped as unchanged.
                        self.output = applied;
                        self.output_working = working;
                        if let Some(selection) =
                            viewer.watching().map(|watching| watching.selection)
                        {
                            self.updates
                                .push(VoiceUpdate::StreamAudio { selection, error });
                        }
                        Vec::new()
                    }
                };
            if !self.apply(outcomes, viewer).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn execute(&mut self, actions: Vec<Action>) -> Result<bool, SessionEnd> {
        for action in actions {
            match action {
                Action::Media(command) => self.media.command(command).map_err(|error| {
                    SessionEnd::Error(format!("РћС€РёР±РєР° РјРµРґРёР°РґРІРёР¶РєР°: {error:?}"))
                })?,
                Action::Send(signal) => self.send_signal(signal).await?,
                Action::Notice(message) => {
                    self.updates.push(VoiceUpdate::DeviceError(message.into()))
                }
                Action::Reconnect => return Ok(false),
                Action::Closed(message) => return Err(SessionEnd::Error(message.into())),
            }
        }
        Ok(true)
    }

    async fn send_signal(&mut self, signal: Signal) -> Result<(), SessionEnd> {
        // The live stand test needs the exact signaling the session sent and
        // the answer mid; the test-only probe is empty in every other build.
        #[cfg(test)]
        crate::live_probe::record_signal(signal.kind, &signal.data);
        self.send(signal.kind, signal.data, signal.request_id.as_deref())
            .await
    }

    /// Execute viewer decisions in order. A failed engine start is converted
    /// back into state-machine outcomes, so the intent is cleared the same way
    /// as any other failure.
    async fn apply(
        &mut self,
        outcomes: Vec<ViewerOutcome>,
        viewer: &mut Viewer,
    ) -> Result<bool, SessionEnd> {
        let mut work: VecDeque<ViewerOutcome> = outcomes.into();
        while let Some(outcome) = work.pop_front() {
            match outcome {
                ViewerOutcome::Start {
                    selection,
                    attempt,
                    publication,
                    muted,
                    request,
                } => {
                    // Defensive: the viewer starts only after the previous
                    // attempt closed, but an engine must never be dropped on the
                    // signaling thread if that ever changes.
                    match StreamViewer::start(
                        self.servers.clone(),
                        self.policy.clone(),
                        self.output.clone(),
                        muted,
                        attempt,
                    ) {
                        Ok(engine) => {
                            // A replacement engine has its own frame sequence.
                            // Keeping the old identity would hide its first
                            // frame when the sequence and size happen to match.
                            install_stream(
                                &mut self.stream,
                                &mut self.stream_selection,
                                &self.frame,
                                &mut self.render,
                                engine,
                                selection,
                            );
                            if let Err(end) = self.send_signal(request).await {
                                self.close_stream();
                                return Err(end);
                            }
                            let _ = publication;
                        }
                        Err(message) => {
                            for outcome in viewer
                                .on_start_failed(&message, Instant::now())
                                .into_iter()
                                .rev()
                            {
                                work.push_front(outcome);
                            }
                        }
                    }
                }
                ViewerOutcome::Close => {
                    self.close_stream();
                }
                ViewerOutcome::Send(signal) => self.send_signal(signal).await?,
                ViewerOutcome::Offer(sdp) => {
                    let failure = self
                        .stream
                        .as_ref()
                        .and_then(|stream| stream.offer(sdp).err());
                    if let Some(message) = failure {
                        for outcome in viewer
                            .on_engine_error(&message, Instant::now())
                            .into_iter()
                            .rev()
                        {
                            work.push_front(outcome);
                        }
                    }
                }
                ViewerOutcome::Ice(candidate) => {
                    let failure = self
                        .stream
                        .as_ref()
                        .and_then(|stream| stream.ice(candidate).err());
                    if let Some(message) = failure {
                        for outcome in viewer
                            .on_engine_error(&message, Instant::now())
                            .into_iter()
                            .rev()
                        {
                            work.push_front(outcome);
                        }
                    }
                }
                ViewerOutcome::Mute { selection, muted } => {
                    if self.stream_selection == Some(selection) {
                        if let Some(stream) = &mut self.stream {
                            if let Err(message) = stream.set_muted(muted) {
                                self.updates.push(VoiceUpdate::StreamStatus {
                                    selection,
                                    status: message,
                                });
                            }
                        }
                    }
                }
                ViewerOutcome::Status { selection, text } => {
                    self.updates.push(VoiceUpdate::StreamStatus {
                        selection,
                        status: text,
                    });
                }
                ViewerOutcome::Connected {
                    selection,
                    connected,
                } => {
                    self.updates.push(VoiceUpdate::StreamConnected {
                        selection,
                        connected,
                    });
                }
                ViewerOutcome::Clear { selection } => {
                    if self.stream_selection == Some(selection) {
                        self.close_stream();
                    }
                    self.updates.push(VoiceUpdate::StreamStatus {
                        selection,
                        status: String::new(),
                    });
                }
                ViewerOutcome::Failed { selection, message } => {
                    if self.stream_selection == Some(selection) {
                        self.close_stream();
                    }
                    self.updates.push(VoiceUpdate::StreamError {
                        selection,
                        error: message,
                    });
                }
                ViewerOutcome::Catalog => {
                    self.updates
                        .push(VoiceUpdate::Streams(viewer.catalog().list().to_vec()));
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
