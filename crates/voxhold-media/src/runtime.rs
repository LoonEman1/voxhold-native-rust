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
    controller::{Action, Participant, Publication, Signal, VoiceController},
    stream::{EngineUpdate, StreamViewer},
    viewer::{Outcome as ViewerOutcome, Viewer},
    voice::CaptureGate,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const UPDATE_CAPACITY: usize = 64;
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

#[derive(Clone, Debug)]
pub enum VoiceUpdate {
    Connecting,
    Connected,
    Reconnecting,
    Participants(Vec<Participant>),
    Speaking(HashMap<String, bool>),
    LocalLevel(f64),
    Tracks(HashSet<String>),
    Streams(Vec<Publication>),
    StreamStatus {
        selection: u64,
        status: String,
    },
    StreamError {
        selection: u64,
        error: String,
    },
    StreamConnected {
        selection: u64,
        connected: bool,
    },
    DeviceError(String),
    /// The attempt finished. An error reason is carried together with the end
    /// so the UI can never lose it to a coalesced terminal `Left`.
    Ended(Option<String>),
}

impl VoiceUpdate {
    fn same_slot(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::StreamStatus { selection: a, .. }, Self::StreamStatus { selection: b, .. })
            | (Self::StreamError { selection: a, .. }, Self::StreamError { selection: b, .. })
            | (
                Self::StreamConnected { selection: a, .. },
                Self::StreamConnected { selection: b, .. },
            ) => a == b,
            _ => matches!(
                (self, other),
                (Self::Connecting, Self::Connecting)
                    | (Self::Connected, Self::Connected)
                    | (Self::Reconnecting, Self::Reconnecting)
                    | (Self::Participants(_), Self::Participants(_))
                    | (Self::Speaking(_), Self::Speaking(_))
                    | (Self::LocalLevel(_), Self::LocalLevel(_))
                    | (Self::Tracks(_), Self::Tracks(_))
                    | (Self::Streams(_), Self::Streams(_))
            ),
        }
    }

    fn terminal(&self) -> bool {
        matches!(self, Self::Ended(_))
    }
}

/// Coalescing update queue. Terminal updates are never merged into a snapshot.
struct Updates {
    queue: Mutex<VecDeque<VoiceUpdate>>,
}

impl Updates {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
        }
    }

    fn push(&self, update: VoiceUpdate) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        if update.terminal() {
            queue.retain(|old| !old.terminal());
            queue.push_back(update);
            return;
        }
        if let Some(slot) = queue.iter_mut().find(|old| old.same_slot(&update)) {
            *slot = update;
            return;
        }
        if queue.len() >= UPDATE_CAPACITY {
            queue.pop_front();
        }
        queue.push_back(update);
    }

    fn poll(&self) -> Option<VoiceUpdate> {
        self.queue.lock().ok()?.pop_front()
    }
}

/// Selecting and closing a stream share one last-value slot. A stale close can
/// therefore never overtake a newer watch intent.
#[derive(Clone, Debug)]
enum StreamCommand {
    Watch {
        selection: u64,
        publication: Publication,
    },
    Close {
        selection: u64,
    },
}

impl StreamCommand {
    fn selection(&self) -> u64 {
        match self {
            Self::Watch { selection, .. } | Self::Close { selection } => *selection,
        }
    }
}

/// Install a stream intent only if it is not older than the queued one.
/// Selection ids are monotonic, so a late close for A cannot erase a newer
/// watch for B (and vice versa).
fn set_stream_command(requests: &Requests, command: StreamCommand) -> bool {
    let selection = command.selection();
    let Ok(mut slot) = requests.stream.lock() else {
        return false;
    };
    if slot
        .as_ref()
        .is_some_and(|current| current.selection() > selection)
    {
        return false;
    }
    *slot = Some(command);
    true
}

/// Latest-value request slots shared by the handle and the worker.
#[derive(Default)]
struct Requests {
    controls: Mutex<Option<(Controls, bool)>>,
    local_mutes: Mutex<VecDeque<(String, bool)>>,
    stream: Mutex<Option<StreamCommand>>,
    stream_mute: Mutex<Option<(u64, bool)>>,
    viewport: Mutex<Option<(u32, u32)>>,
    leave: AtomicBool,
    notify: Notify,
}

enum Pending {
    Leave,
    Stream(StreamCommand),
    Controls(Controls, bool),
    StreamMute(u64, bool),
    LocalMute(String, bool),
}

impl Requests {
    fn wake(&self) {
        self.notify.notify_waiters();
    }

    /// Drain one action by priority. Called in a loop until it returns `None`.
    fn take(&self) -> Option<Pending> {
        if self.leave.swap(false, Ordering::SeqCst) {
            return Some(Pending::Leave);
        }
        if let Ok(mut stream) = self.stream.lock() {
            if let Some(command) = stream.take() {
                return Some(Pending::Stream(command));
            }
        }
        if let Ok(mut controls) = self.controls.lock() {
            if let Some((controls, self_muted)) = controls.take() {
                return Some(Pending::Controls(controls, self_muted));
            }
        }
        if let Ok(mut mute) = self.stream_mute.lock() {
            if let Some((selection, muted)) = mute.take() {
                return Some(Pending::StreamMute(selection, muted));
            }
        }
        let item = self.local_mutes.lock().ok()?.pop_front();
        item.map(|(connection, muted)| Pending::LocalMute(connection, muted))
    }

    fn viewport(&self) -> (u32, u32) {
        self.viewport
            .lock()
            .ok()
            .and_then(|viewport| *viewport)
            .filter(|(width, height)| *width > 0 && *height > 0)
            .unwrap_or(DEFAULT_VIEWPORT)
    }
}

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
        static ATTEMPT: AtomicU64 = AtomicU64::new(1);
        let attempt = ATTEMPT.fetch_add(1, Ordering::SeqCst);
        let requests = Arc::new(Requests::default());
        let updates = Arc::new(Updates::new());
        let frame = Arc::new(Mutex::new(None));
        let capture = Arc::new(CaptureGate::default());
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let (worker_requests, worker_updates) = (requests.clone(), updates.clone());
        let (worker_frame, worker_finished) = (frame.clone(), finished.clone());
        let worker_capture = capture.clone();
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

    pub fn local_mute(&self, connection: String, muted: bool) {
        if let Ok(mut queue) = self.requests.local_mutes.lock() {
            if queue.len() >= LOCAL_MUTE_CAPACITY {
                queue.pop_front();
            }
            queue.push_back((connection, muted));
        }
        self.requests.wake();
    }

    pub fn take_frame(&self) -> Option<StreamFrame> {
        self.frame.lock().ok()?.take()
    }

    /// Physical pixels of the player picture; the decoder scales to this.
    pub fn set_viewport(&self, width: u32, height: u32) {
        if let Ok(mut slot) = self.requests.viewport.lock() {
            *slot = Some((width, height));
        }
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
        self.requests.leave.store(true, Ordering::SeqCst);
        if let Ok(mut stream) = self.requests.stream.lock() {
            stream.take();
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
    stream: Option<StreamViewer>,
    stream_selection: Option<u64>,
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

    /// Move the freshly decoded frame into the last-value slot, reclaiming the
    /// buffer the UI has not taken yet so steady state does not allocate.
    fn publish_frame(&mut self, selection: u64) {
        let mut next = self
            .frame
            .lock()
            .ok()
            .and_then(|mut slot| {
                // A frame from another selection is not a buffer to reclaim.
                slot.take()
                    .filter(|frame| frame.selection == selection)
                    .map(|frame| frame.bgra)
            })
            .unwrap_or_default();
        if next.len() < self.render.bgra.len() {
            next.resize(self.render.bgra.len(), 0);
        }
        let (width, height) = (self.render.width(), self.render.height());
        std::mem::swap(&mut next, &mut self.render.bgra);
        if let Ok(mut slot) = self.frame.lock() {
            *slot = Some(StreamFrame {
                selection,
                width,
                height,
                bgra: next,
            });
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
            let speaking = controller.speaking.clone();
            let tracks = controller.connections_with_audio();
            let actions = controller.media_event(event);
            if !self.execute(actions).await? {
                return Ok(false);
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
            let outcomes = match update {
                EngineUpdate::Frame => {
                    if let Some(selection) = viewer.watching().map(|watching| watching.selection) {
                        self.publish_frame(selection);
                    }
                    viewer.on_frame()
                }
                EngineUpdate::Answer(sdp) => viewer.on_answer(sdp),
                EngineUpdate::Ice(candidate) => viewer.on_ice(candidate),
                EngineUpdate::State(state) => viewer.on_engine_state(state, now),
                EngineUpdate::Error(message) => viewer.on_engine_error(&message, now),
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
                Action::Media(command) => self
                    .media
                    .command(command)
                    .map_err(|error| SessionEnd::Error(format!("Ошибка медиадвижка: {error:?}")))?,
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
                    publication,
                    muted,
                    request,
                } => {
                    match StreamViewer::start(
                        self.servers.clone(),
                        self.policy.clone(),
                        self.output.clone(),
                        muted,
                    ) {
                        Ok(engine) => {
                            self.stream = Some(engine);
                            self.stream_selection = Some(selection);
                            if let Err(end) = self.send_signal(request).await {
                                self.stream = None;
                                self.stream_selection = None;
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
                    self.stream = None;
                    self.stream_selection = None;
                    self.clear_frame();
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
                        self.stream = None;
                        self.stream_selection = None;
                        self.clear_frame();
                    }
                    self.updates.push(VoiceUpdate::StreamStatus {
                        selection,
                        status: String::new(),
                    });
                }
                ViewerOutcome::Failed { selection, message } => {
                    if self.stream_selection == Some(selection) {
                        self.stream = None;
                        self.stream_selection = None;
                        self.clear_frame();
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

async fn run_session(
    client: &voxhold_api::Client,
    requests: &Requests,
    controller: &mut VoiceController,
    viewer: &mut Viewer,
    updates: Arc<Updates>,
    frame: Arc<Mutex<Option<StreamFrame>>>,
    capture: Arc<CaptureGate>,
) -> SessionEnd {
    let config = match client.webrtc_config().await {
        Ok(config) => config,
        Err(error) => {
            return if controller.resume_connection_id.is_some() {
                SessionEnd::Recover
            } else {
                SessionEnd::Error(format!(
                    "Не удалось загрузить настройки соединения: {error}"
                ))
            };
        }
    };
    let media = match Media::new_with_capture(capture) {
        Ok(media) => media,
        Err(error) => return SessionEnd::Error(format!("{error:?}")),
    };
    let servers: Vec<IceServer> = config
        .ice_servers
        .into_iter()
        .map(|server| IceServer {
            urls: server.urls,
            username: server.username,
            credential: server.credential,
        })
        .collect();
    if let Err(error) = media.command(Command::Start {
        ice_servers: servers.clone(),
        ice_transport_policy: config.ice_transport_policy.clone(),
        controls: controller.effective(),
    }) {
        return SessionEnd::Error(format!("{error:?}"));
    }
    let socket = match client.open_voice_socket().await {
        Ok(socket) => socket,
        Err(error) => {
            return if controller.resume_connection_id.is_some() {
                SessionEnd::Recover
            } else {
                SessionEnd::Error(error.to_string())
            };
        }
    };
    let mut live = Live {
        socket,
        media,
        servers,
        policy: config.ice_transport_policy,
        output: controller.controls.output.clone(),
        stream: None,
        stream_selection: None,
        render: VideoFrame::default(),
        viewport: requests.viewport(),
        local_level: None,
        frame,
        updates,
    };
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let notified = requests.notify.notified();
        while let Some(pending) = requests.take() {
            match pending {
                Pending::Leave => {
                    // The handle already latched the capture synchronously.
                    // Stopping the media worker applies the same latch again
                    // and releases the device before the leave handshake.
                    let _ = live.media.command(Command::Stop);
                    let outcomes = viewer.leave(Instant::now());
                    let _ = live.apply(outcomes, viewer).await;
                    live.send_leave().await;
                    // A normal leave waits briefly for the acknowledgement so
                    // the server removes the participant before we drop the
                    // peer; the socket is closed regardless.
                    let deadline = Instant::now() + Duration::from_secs(1);
                    while Instant::now() < deadline {
                        let left =
                            tokio::time::timeout(Duration::from_millis(200), live.socket.read());
                        match left.await {
                            Ok(Ok(event)) if event.kind == "voice.left" => break,
                            Ok(Ok(_)) => continue,
                            _ => break,
                        }
                    }
                    live.stream = None;
                    live.stream_selection = None;
                    live.clear_frame();
                    return SessionEnd::Leave;
                }
                Pending::Stream(StreamCommand::Close { selection }) => {
                    let outcomes = viewer.leave_selection(selection, Instant::now());
                    if live.stream_selection == Some(selection) {
                        live.stream = None;
                        live.stream_selection = None;
                        live.clear_frame();
                    }
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::Stream(StreamCommand::Watch {
                    selection,
                    publication,
                }) => {
                    let outcomes = viewer.select(&publication, selection, Instant::now());
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::StreamMute(selection, muted) => {
                    let outcomes = viewer.set_muted(selection, muted);
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::Controls(controls, self_muted) => {
                    let output = controls.output.clone();
                    let actions = controller.update_controls(controls, self_muted);
                    if output != live.output {
                        live.output.clone_from(&output);
                        if let Some(stream) = &mut live.stream {
                            if let Err(message) = stream.set_output(output.clone()) {
                                live.updates.push(VoiceUpdate::StreamStatus {
                                    selection: live.stream_selection.unwrap_or_default(),
                                    status: message,
                                });
                            }
                        }
                    }
                    match live.execute(actions).await {
                        Ok(true) => {}
                        Ok(false) => return SessionEnd::Recover,
                        Err(end) => return end,
                    }
                }
                Pending::LocalMute(connection, muted) => {
                    let actions = controller.set_local_mute(&connection, muted);
                    match live.execute(actions).await {
                        Ok(true) => {}
                        Ok(false) => return SessionEnd::Recover,
                        Err(end) => return end,
                    }
                    live.updates
                        .push(VoiceUpdate::Tracks(controller.connections_with_audio()));
                    live.updates
                        .push(VoiceUpdate::Speaking(controller.speaking.clone()));
                }
            }
            live.updates
                .push(VoiceUpdate::Participants(controller.participants.clone()));
        }
        tokio::select! {
            _ = notified => {}
            event = live.socket.read() => {
                let Ok(event) = event else { return SessionEnd::Recover };
                if event.kind.starts_with("stream.")
                    || (event.kind == "error"
                        && event.request_id.starts_with("native-stream-"))
                {
                    let outcomes = viewer.on_server_event(
                        &event.kind,
                        &event.request_id,
                        &event.data,
                        Instant::now(),
                    );
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                    // `stream.watching` refreshes the publication in the list.
                    live.updates
                        .push(VoiceUpdate::Streams(viewer.catalog().list().to_vec()));
                    continue;
                }
                let actions = controller.on_signal(&event.kind, &event.data);
                match live.execute(actions).await {
                    Ok(true) => {}
                    Ok(false) => return SessionEnd::Recover,
                    Err(end) => return end,
                }
                live.media.set_joined(controller.joined);
                live.media
                    .set_server_allowed(!controller.server_muted && !controller.controls.deafened);
                // The viewer follows the confirmed voice state, not the socket:
                // a fresh join re-watches the selected publication.
                let outcomes = viewer.set_joined(controller.joined, Instant::now());
                if let Err(end) = live.apply(outcomes, viewer).await {
                    return end;
                }
                live.updates
                    .push(VoiceUpdate::Participants(controller.participants.clone()));
                live.updates
                    .push(VoiceUpdate::Speaking(controller.speaking.clone()));
                live.updates
                    .push(VoiceUpdate::Tracks(controller.connections_with_audio()));
            }
            _ = tick.tick() => {
                live.viewport = requests.viewport();
                let outcomes = viewer.tick(Instant::now());
                if let Err(end) = live.apply(outcomes, viewer).await {
                    return end;
                }
                match live.drain_voice_media(controller).await {
                    Ok(true) => {}
                    Ok(false) => return SessionEnd::Recover,
                    Err(end) => return end,
                }
                match live.drain_viewer(viewer).await {
                    Ok(true) => {}
                    Ok(false) => return SessionEnd::Recover,
                    Err(end) => return end,
                }
            }
        }
    }
}

fn run(
    client: voxhold_api::Client,
    server: i64,
    channel: i64,
    controls: Controls,
    self_muted: bool,
    requests: Arc<Requests>,
    updates: Arc<Updates>,
    frame: Arc<Mutex<Option<StreamFrame>>>,
    capture: Arc<CaptureGate>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        updates.push(VoiceUpdate::Ended(Some(
            "Не удалось запустить голосовое соединение".into(),
        )));
        return;
    };
    let mut controller = VoiceController::new(server, channel, controls, self_muted);
    let mut viewer = Viewer::new(server, channel);
    let mut retries = 0;
    loop {
        if requests.leave.swap(false, Ordering::SeqCst) {
            break;
        }
        updates.push(VoiceUpdate::Connecting);
        let started = Instant::now();
        let end = runtime.block_on(run_session(
            &client,
            &requests,
            &mut controller,
            &mut viewer,
            updates.clone(),
            frame.clone(),
            capture.clone(),
        ));
        controller.joined = false;
        let _ = viewer.set_joined(false, Instant::now());
        updates.push(VoiceUpdate::Streams(Vec::new()));
        if let Ok(mut frame) = frame.lock() {
            frame.take();
        }
        match end {
            SessionEnd::Leave => break,
            SessionEnd::Error(message) => {
                updates.push(VoiceUpdate::Ended(Some(message)));
                return;
            }
            SessionEnd::Recover => {}
        }
        if started.elapsed() > LONG_SESSION {
            retries = 0;
        }
        retries += 1;
        if retries > MAX_RECONNECTS {
            updates.push(VoiceUpdate::Ended(Some(
                "Не удалось восстановить голосовое соединение".into(),
            )));
            return;
        }
        updates.push(VoiceUpdate::Reconnecting);
        let deadline = Instant::now() + RECONNECT_DELAY;
        let mut aborted = false;
        while Instant::now() < deadline {
            while let Some(pending) = requests.take() {
                match pending {
                    Pending::Leave => {
                        aborted = true;
                        break;
                    }
                    Pending::Controls(controls, self_muted) => {
                        controller.controls = controls;
                        controller.self_muted = self_muted;
                    }
                    Pending::LocalMute(connection, muted) => {
                        controller.set_local_mute(&connection, muted);
                    }
                    Pending::Stream(StreamCommand::Watch {
                        selection,
                        publication,
                    }) => {
                        viewer.select_intent(publication, selection);
                    }
                    Pending::Stream(StreamCommand::Close { selection }) => {
                        let _ = viewer.leave_selection(selection, Instant::now());
                    }
                    Pending::StreamMute(selection, muted) => {
                        let _ = viewer.set_muted(selection, muted);
                    }
                }
            }
            if aborted {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if aborted {
            break;
        }
    }
    let _ = frame.lock().map(|mut frame| frame.take());
    updates.push(VoiceUpdate::Ended(None));
}

#[cfg(test)]
mod tests;
