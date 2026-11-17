//! Background voice session. The UI never blocks on the socket or a media
//! engine: it writes bounded last-value requests and reads coalesced updates.
//!
//! High-frequency values (controls, stream mute, viewport, frames, levels,
//! rosters) keep only the latest value, so a burst of input cannot grow a
//! queue. User actions are bounded too; `leave` outranks everything and
//! `leave_stream` clears a pending watch so a close can never be overtaken.

use crate::{
    Command, Controls, Event, IceServer, Media, VideoFrame,
    controller::{Action, Participant, Publication, Signal, VoiceController},
    stream::{EngineUpdate, StreamViewer},
    viewer::{Outcome as ViewerOutcome, Viewer},
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
    StreamStatus(String),
    StreamError(String),
    DeviceError(String),
    Error(String),
    Left,
}

impl VoiceUpdate {
    fn same_slot(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Connecting, Self::Connecting)
                | (Self::Connected, Self::Connected)
                | (Self::Reconnecting, Self::Reconnecting)
                | (Self::Participants(_), Self::Participants(_))
                | (Self::Speaking(_), Self::Speaking(_))
                | (Self::LocalLevel(_), Self::LocalLevel(_))
                | (Self::Tracks(_), Self::Tracks(_))
                | (Self::Streams(_), Self::Streams(_))
                | (Self::StreamStatus(_), Self::StreamStatus(_))
        )
    }

    fn terminal(&self) -> bool {
        matches!(self, Self::Error(_) | Self::Left)
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

/// Latest-value request slots shared by the handle and the worker.
#[derive(Default)]
struct Requests {
    controls: Mutex<Option<(Controls, bool)>>,
    local_mutes: Mutex<VecDeque<(String, bool)>>,
    watch: Mutex<Option<Publication>>,
    stream_mute: Mutex<Option<bool>>,
    viewport: Mutex<Option<(u32, u32)>>,
    leave_stream: AtomicBool,
    leave: AtomicBool,
    notify: Notify,
}

enum Pending {
    Leave,
    LeaveStream,
    Controls(Controls, bool),
    Watch(Publication),
    StreamMute(bool),
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
        if self.leave_stream.swap(false, Ordering::SeqCst) {
            if let Ok(mut watch) = self.watch.lock() {
                *watch = None;
            }
            return Some(Pending::LeaveStream);
        }
        if let Ok(mut controls) = self.controls.lock() {
            if let Some((controls, self_muted)) = controls.take() {
                return Some(Pending::Controls(controls, self_muted));
            }
        }
        if let Ok(mut watch) = self.watch.lock() {
            if let Some(publication) = watch.take() {
                return Some(Pending::Watch(publication));
            }
        }
        if let Ok(mut mute) = self.stream_mute.lock() {
            if let Some(muted) = mute.take() {
                return Some(Pending::StreamMute(muted));
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
    pub width: u32,
    pub height: u32,
    /// BGRA pixels ready for `RenderImage`; the UI must not swap channels.
    pub bgra: Vec<u8>,
}

pub struct VoiceHandle {
    requests: Arc<Requests>,
    updates: Arc<Updates>,
    frame: Arc<Mutex<Option<StreamFrame>>>,
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
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let (worker_requests, worker_updates) = (requests.clone(), updates.clone());
        let (worker_frame, worker_finished) = (frame.clone(), finished.clone());
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
    /// not picked up B yet and the user chooses C, only C is applied.
    pub fn watch_stream(&self, publication: Publication) {
        if let Ok(mut slot) = self.requests.watch.lock() {
            *slot = Some(publication);
        }
        self.requests.leave_stream.store(false, Ordering::SeqCst);
        if let Ok(mut frame) = self.frame.lock() {
            frame.take();
        }
        self.requests.wake();
    }

    pub fn leave_stream(&self) {
        if let Ok(mut slot) = self.requests.watch.lock() {
            slot.take();
        }
        self.requests.leave_stream.store(true, Ordering::SeqCst);
        if let Ok(mut frame) = self.frame.lock() {
            frame.take();
        }
        self.requests.wake();
    }

    pub fn stream_mute(&self, muted: bool) {
        if let Ok(mut slot) = self.requests.stream_mute.lock() {
            *slot = Some(muted);
        }
        self.requests.wake();
    }

    pub fn leave(&self) {
        self.requests.leave.store(true, Ordering::SeqCst);
        if let Ok(mut watch) = self.requests.watch.lock() {
            watch.take();
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
        // Never block the UI for long: on a normal close the worker finishes
        // within the wait; otherwise the leave flag stops it on its own.
        let finished = self
            .finished
            .0
            .lock()
            .map(|done| {
                self.finished
                    .1
                    .wait_timeout_while(done, Duration::from_millis(250), |done| !*done)
                    .map(|(done, _)| *done)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if finished {
            let _ = worker.join();
        }
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
        self.socket
            .send(kind, data, id)
            .await
            .map_err(|_| SessionEnd::Recover)
    }

    fn clear_frame(&self) {
        if let Ok(mut frame) = self.frame.lock() {
            frame.take();
        }
    }

    /// Move the freshly decoded frame into the last-value slot, reclaiming the
    /// buffer the UI has not taken yet so steady state does not allocate.
    fn publish_frame(&mut self) {
        let mut next = self
            .frame
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .map(|frame| frame.bgra)
            .unwrap_or_default();
        if next.len() < self.render.bgra.len() {
            next.resize(self.render.bgra.len(), 0);
        }
        let (width, height) = (self.render.width(), self.render.height());
        std::mem::swap(&mut next, &mut self.render.bgra);
        if let Ok(mut slot) = self.frame.lock() {
            *slot = Some(StreamFrame {
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
            let actions = controller.media_event(event);
            if !actions.is_empty() {
                self.updates
                    .push(VoiceUpdate::Speaking(controller.speaking.clone()));
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
            if !self.execute(actions).await? {
                return Ok(false);
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
                    self.publish_frame();
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
                            if let Err(end) = self.send_signal(request).await {
                                self.stream = None;
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
                ViewerOutcome::Mute(muted) => {
                    if let Some(stream) = &mut self.stream {
                        if let Err(message) = stream.set_muted(muted) {
                            self.updates.push(VoiceUpdate::StreamStatus(message));
                        }
                    }
                }
                ViewerOutcome::Status(status) => {
                    self.updates.push(VoiceUpdate::StreamStatus(status));
                }
                ViewerOutcome::Clear => {
                    self.stream = None;
                    self.clear_frame();
                    self.updates.push(VoiceUpdate::StreamStatus(String::new()));
                }
                ViewerOutcome::Failed(message) => {
                    self.stream = None;
                    self.clear_frame();
                    self.updates.push(VoiceUpdate::StreamError(message));
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
    let media = match Media::new() {
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
                    // Silence the capture before the asynchronous leave
                    // handshake so no audio remains on the old lease while the
                    // server acknowledgement is pending.
                    let _ = live.media.command(Command::Stop);
                    let outcomes = viewer.leave(Instant::now());
                    let _ = live.apply(outcomes, viewer).await;
                    let _ = live
                        .socket
                        .send("voice.leave", serde_json::json!({}), None)
                        .await;
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
                    live.clear_frame();
                    return SessionEnd::Leave;
                }
                Pending::LeaveStream => {
                    let outcomes = viewer.leave(Instant::now());
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::Watch(publication) => {
                    let outcomes = viewer.select(&publication, Instant::now());
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::StreamMute(muted) => {
                    let outcomes = viewer.set_muted(muted);
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::Controls(controls, self_muted) => {
                    let actions = controller.update_controls(controls, self_muted);
                    match live.execute(actions).await {
                        Ok(true) => {}
                        Ok(false) => return SessionEnd::Recover,
                        Err(end) => return end,
                    }
                }
                Pending::LocalMute(connection, muted) => {
                    controller.set_local_mute(&connection, muted);
                    live.updates
                        .push(VoiceUpdate::Tracks(controller.connections_with_audio()));
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
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        updates.push(VoiceUpdate::Error(
            "Не удалось запустить голосовое соединение".into(),
        ));
        updates.push(VoiceUpdate::Left);
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
        ));
        controller.joined = false;
        let _ = viewer.set_joined(false, Instant::now());
        if let Ok(mut frame) = frame.lock() {
            frame.take();
        }
        match end {
            SessionEnd::Leave => break,
            SessionEnd::Error(message) => {
                updates.push(VoiceUpdate::Error(message));
                break;
            }
            SessionEnd::Recover => {}
        }
        if started.elapsed() > LONG_SESSION {
            retries = 0;
        }
        retries += 1;
        if retries > MAX_RECONNECTS {
            updates.push(VoiceUpdate::Error(
                "Не удалось восстановить голосовое соединение".into(),
            ));
            break;
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
                    Pending::Watch(publication) => {
                        let _ = viewer.select(&publication, Instant::now());
                    }
                    Pending::LeaveStream => {
                        let _ = viewer.leave(Instant::now());
                    }
                    Pending::StreamMute(muted) => {
                        let _ = viewer.set_muted(muted);
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
    updates.push(VoiceUpdate::Left);
}

#[cfg(test)]
mod tests;
