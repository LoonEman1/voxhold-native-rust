//! Deterministic state machine for publishing one screen over the SFU.
//!
//! The backend creates the SFU session and sends `stream.webrtc_offer`; the
//! publisher answers it. `stream.started` may arrive after that offer, so the
//! answer path never waits for it. The runtime owns the socket and the media
//! engine; this module only decides when a capture starts, which signaling
//! belongs to the current attempt and when the capture must be released.
//!
//! States: `Idle → Picking → Publishing → Live → Stopping`, plus a terminal
//! local failure. The clock is an argument, so cancel, the viewer leave wait
//! and both deadlines are testable without WebRTC, Windows or a socket.
//!
//! The capture belongs to the current attempt. A late picker result, a leave
//! or a revoked session bumps the attempt and releases it; a request from an
//! older attempt is rejected instead of starting a stream.

use crate::controller::Signal;
use crate::screen_capture::ScreenSource;
use crate::system_audio::AudioScope;
use crate::viewer::LEAVE_TIMEOUT;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

/// The server creates the offer after `stream.start`; the answer must be
/// produced within this window.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);
/// The stop handshake is bounded; a stalled socket never holds the capture.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// `MaxChannelPublishers` in `internal/streamcontract/limits.go`.
pub const MAX_CHANNEL_PUBLISHERS: usize = 8;
/// SDR is the only rendition of this stage; HDR is a later task.
pub const SDR_RENDITION: &str = "sdr";

/// The one video codec this stage can prove. VP8 is in the pinned software
/// encoder set and has a passing in-process publishing loopback; the picker
/// must not offer anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Vp8,
}

impl Codec {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Vp8 => "vp8",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Vp8 => "VP8",
        }
    }

    /// The codecs the picker may offer. A codec is added only together with a
    /// publishing run that proves frames reach the SFU with it.
    pub fn available() -> Vec<Codec> {
        if engine::vp8_encoder_available() {
            vec![Self::Vp8]
        } else {
            Vec::new()
        }
    }
}

mod quality;
pub use quality::{
    AUDIO_BITRATES, Preset, Quality, QualityChange, QualityOp, QualityOutcome, QualityStatus,
    Resolution, normalize_audio_bitrate,
};

mod sharing;
pub use sharing::{Confirmed, Sharing};

/// One capture attempt chosen in the picker.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishRequest {
    pub source: ScreenSource,
    pub quality: Quality,
    pub codec: Codec,
    /// The user asked for system audio. `stream.start` carries the value the
    /// engine actually opened, so a missing loopback is an explicit failure
    /// instead of a silent video-only publication.
    pub audio: bool,
    /// What the requested audio covers. Only used when `audio` is true; the
    /// engine resolves it to a process id for this attempt. `stream.start` has
    /// no scope field, so this stays a local engine parameter.
    pub audio_scope: AudioScope,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Picking,
    Publishing,
    Live,
    Stopping,
    Failed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Picking => "picking",
            Self::Publishing => "publishing",
            Self::Live => "live",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }
}

/// What the server and the voice session confirmed. The button is gated by the
/// same values; the state machine repeats the check so a stale button can
/// never start a capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub joined: bool,
    pub stream_permission: bool,
    /// Fewer than [`MAX_CHANNEL_PUBLISHERS`] streams are active in the channel.
    pub slots_available: bool,
}

/// One decision produced by the state machine. The runtime executes them in
/// order and maps them to socket signals or capture ownership.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Create the capture and the peer for this attempt.
    StartEngine {
        attempt: u64,
        request: PublishRequest,
    },
    /// Release the capture and the peer; the server state is separate.
    StopEngine,
    /// Drop the viewer first and wait for `stream.left` (or five seconds).
    LeaveViewer,
    Send(Signal),
    /// The engine must apply this video/audio bitrate limit to its live
    /// sender. No renegotiation and no new `stream.start` follows.
    SetBitrate {
        attempt: u64,
        op: QualityOp,
        quality: Quality,
    },
    /// The engine must apply this remote offer and produce an answer.
    Offer(String),
    Ice(crate::Candidate),
    State(Sharing),
}

/// Engine events that need a state-machine decision.
pub mod engine_events {
    use crate::Candidate;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Connection {
        Connected,
        FirstFrame,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Event {
        Answer(String),
        Ice(Candidate),
        State(Connection),
        Stats(crate::publisher::engine::PublishStats),
        Error(String),
        Closed,
    }
}

pub use engine_events::{Connection, Event as EngineEvent};

pub struct Publisher {
    server: i64,
    channel: i64,
    self_connection: Option<String>,
    access: Access,
    phase: Phase,
    attempt: u64,
    request: Option<PublishRequest>,
    /// What the attempt that is really on air accepted. It is only replaced
    /// when an attempt turns `Live` or a live bitrate change is applied, so a
    /// preflight, a restart or a rollback keeps the previous configuration.
    confirmed: Option<Confirmed>,
    /// Quality change of a live attempt. The backend has no runtime
    /// reconfigure, so the new request waits for the current `stream.stop`
    /// acknowledgement (or its bounded timeout) and then starts a new attempt.
    pending_restart: Option<PublishRequest>,
    /// The request of the attempt that was live when a restart began. A local
    /// failure of the new attempt may restore it exactly once.
    rollback: Option<PublishRequest>,
    /// The one automatic restore was already used; a second local failure is
    /// terminal instead of looping.
    rollback_used: bool,
    restarting: bool,
    /// A restart preflight is validating a new capture; the live stream was
    /// not touched. The UI keeps the dialog open so closing it can cancel.
    preflighting: bool,
    /// The accepted quality-change operation and its last result. Results of
    /// an older operation are ignored instead of overwriting a newer one.
    quality_status: Option<QualityStatus>,
    /// The backend broadcasts `stream.stopped` to the whole server and also
    /// answers `stream.stop` directly, so one stop produces two events. After
    /// a confirmed stop the second copy must not end the freshly restarted
    /// attempt.
    ignore_stop_echo: bool,
    /// Actual system-audio track of the engine; reported by `stream.start`.
    has_audio: bool,
    /// Actual scope of that track, for the local on-air line.
    audio_scope: Option<String>,
    /// Picker id of the current attempt. A publish for another id is stale.
    picker: Option<u64>,
    sequence: u64,
    start_request: Option<String>,
    requests: HashSet<String>,
    started: Option<String>,
    start_sent: bool,
    /// The media engine of this attempt was created. A failed start has no
    /// engine, so its terminal decision must not emit `StopEngine`.
    engine_live: bool,
    answered: bool,
    stop_sent: bool,
    connected: bool,
    sent_frame: bool,
    viewer_wait: Option<Instant>,
    deadline: Option<Instant>,
    stop_deadline: Option<Instant>,
    status: String,
    error: Option<String>,
    stats: Option<engine::PublishStats>,
    /// Picker ids at or below this number belong to a session that already
    /// ended; a late result must not start a capture.
    stale_before: u64,
}

impl Publisher {
    pub fn new(server: i64, channel: i64) -> Self {
        Self {
            server,
            channel,
            self_connection: None,
            access: Access::default(),
            phase: Phase::Idle,
            attempt: 0,
            request: None,
            confirmed: None,
            pending_restart: None,
            rollback: None,
            rollback_used: false,
            restarting: false,
            preflighting: false,
            quality_status: None,
            ignore_stop_echo: false,
            has_audio: false,
            audio_scope: None,
            picker: None,
            sequence: 0,
            start_request: None,
            requests: HashSet::new(),
            started: None,
            start_sent: false,
            engine_live: false,
            answered: false,
            stop_sent: false,
            connected: false,
            sent_frame: false,
            viewer_wait: None,
            deadline: None,
            stop_deadline: None,
            status: String::new(),
            error: None,
            stats: None,
            stale_before: 0,
        }
    }

    pub fn sharing(&self) -> Sharing {
        Sharing {
            phase: self.phase,
            attempt: self.attempt,
            live: self.phase == Phase::Live,
            quality: self.request.as_ref().map(|request| request.quality),
            codec: self.request.as_ref().map(|request| request.codec),
            confirmed: self.confirmed.clone(),
            has_audio: self.has_audio,
            audio_scope: self.audio_scope.clone(),
            restarting: self.restarting,
            preflighting: self.preflighting,
            quality_status: self.quality_status.clone(),
            status: self.status.clone(),
            error: self.error.clone(),
            stats: self.stats,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// `voice.joined` / a lost voice connection.
    pub fn set_connection(&mut self, connection: Option<String>) {
        self.self_connection = connection;
    }

    /// Confirmed access for the room. The state machine repeats the check so a
    /// stale button can never start a capture.
    pub fn set_access(&mut self, access: Access) {
        self.access = access;
    }

    /// Open the picker for `picker`. Capture does not start yet.
    ///
    /// Every picker starts a fresh attempt: the confirmation flags, the stop
    /// latch and the deadlines of a failed or finished attempt must not leak
    /// into the next publication (otherwise its `stream.stop` is suppressed
    /// and the first `Connected` turns it Live).
    pub fn begin_picker(&mut self, picker: u64) -> Vec<Outcome> {
        if picker <= self.stale_before {
            return Vec::new();
        }
        if matches!(
            self.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping
        ) {
            return Vec::new();
        }
        self.reset();
        self.attempt = self.attempt.wrapping_add(1);
        self.picker = Some(picker);
        self.phase = Phase::Picking;
        self.status = "Выберите экран".into();
        vec![Outcome::State(self.sharing())]
    }

    /// Close the picker. A viewer that was already active is untouched and no
    /// `stream.start` is sent.
    pub fn cancel_picker(&mut self, picker: u64) -> Vec<Outcome> {
        if self.phase != Phase::Picking || self.picker != Some(picker) {
            return Vec::new();
        }
        self.reset();
        vec![Outcome::State(self.sharing())]
    }

    /// Start the capture for the picker that chose this source.
    ///
    /// `viewer_active` is true when this client watches a publication in the
    /// same channel; that viewer is left first so the peers do not conflict.
    pub fn publish(
        &mut self,
        picker: u64,
        request: PublishRequest,
        viewer_active: bool,
        now: Instant,
    ) -> Vec<Outcome> {
        if self.phase != Phase::Picking || self.picker != Some(picker) {
            return Vec::new();
        }
        if !self.access.joined {
            return self.fail("Сначала подключитесь к голосовому каналу.", now);
        }
        if !self.access.stream_permission {
            return self.fail("Нет права запускать трансляцию.", now);
        }
        if !self.access.slots_available {
            return self.fail(
                "В канале уже 8 трансляций. Дождитесь завершения одной из них.",
                now,
            );
        }
        let mut request = request;
        request.quality = request.quality.clamped();
        self.request = Some(request);
        if viewer_active {
            self.viewer_wait = Some(now + LEAVE_TIMEOUT);
            self.status = "Завершаем просмотр…".into();
            return vec![Outcome::LeaveViewer, Outcome::State(self.sharing())];
        }
        self.start(now)
    }

    /// Change the settings of the live attempt. The backend rejects a second
    /// `stream.start` while the publication exists and has no renegotiation
    /// event, so this is a confirmed stop+start: one `stream.stop` now and one
    /// new `stream.start` after the acknowledgement (or its bounded timeout).
    /// Returns nothing outside `Live`, where the UI has no restart action.
    pub fn restart(
        &mut self,
        op: QualityOp,
        request: PublishRequest,
        now: Instant,
    ) -> Vec<Outcome> {
        if self.phase != Phase::Live || !self.operation_current(op) {
            return Vec::new();
        }
        let mut request = request;
        request.quality = request.quality.clamped();
        self.quality_status = Some(QualityStatus::new(op, QualityOutcome::Committed));
        // Keep the confirmed live request. If the new attempt fails locally
        // after the old stream was stopped, it is restored once.
        self.rollback = self.request.clone();
        self.rollback_used = false;
        self.preflighting = false;
        self.pending_restart = Some(request);
        self.restarting = true;
        self.status = "Перезапускаем трансляцию…".into();
        let outcomes = self.stop(now);
        if self.phase != Phase::Stopping {
            // The stop did not commit (defensive): never leave a queued restart.
            self.pending_restart = None;
            self.restarting = false;
        }
        outcomes
    }

    /// `stream.left` for the viewer we asked to close, or a tick that found it
    /// gone. Starts the capture without waiting for the rest of the timeout.
    pub fn viewer_left(&mut self, now: Instant) -> Vec<Outcome> {
        if self.phase != Phase::Picking || self.viewer_wait.take().is_none() {
            return Vec::new();
        }
        self.start(now)
    }

    /// One stop intent: the capture is released now, exactly one `stream.stop`
    /// is sent, and `stream.stopped` is awaited for a bounded time.
    pub fn stop(&mut self, now: Instant) -> Vec<Outcome> {
        match self.phase {
            Phase::Picking => {
                // Nothing was sent and no capture exists.
                self.reset();
                vec![Outcome::State(self.sharing())]
            }
            Phase::Publishing | Phase::Live => {
                let mut out = Vec::new();
                if self.engine_live {
                    out.push(Outcome::StopEngine);
                }
                if self.start_sent && !self.stop_sent {
                    self.stop_sent = true;
                    out.push(Outcome::Send(self.signal("stream.stop", json!({}))));
                }
                self.phase = Phase::Stopping;
                self.ignore_stop_echo = true;
                self.status = if self.pending_restart.is_some() {
                    "Перезапускаем трансляцию…".into()
                } else {
                    "Останавливаем трансляцию…".into()
                };
                self.stats = None;
                self.viewer_wait = None;
                self.deadline = None;
                if self.start_sent {
                    self.stop_deadline = Some(now + STOP_TIMEOUT);
                    out.push(Outcome::State(self.sharing()));
                } else {
                    self.reset();
                    out.push(Outcome::State(self.sharing()));
                }
                out
            }
            Phase::Stopping => Vec::new(),
            Phase::Idle | Phase::Failed => {
                self.reset();
                vec![Outcome::State(self.sharing())]
            }
        }
    }

    /// Leave, logout, revoked access or a lost socket. The capture is closed
    /// immediately and a late picker result for this session is rejected.
    pub fn abandon(&mut self) -> Vec<Outcome> {
        let engine = self.engine_live;
        self.stale_before = self.stale_before.max(self.picker.unwrap_or(0));
        self.reset();
        let mut out = Vec::new();
        if engine {
            out.push(Outcome::StopEngine);
        }
        out.push(Outcome::State(self.sharing()));
        out
    }

    /// The engine could not be created for this attempt, or the local media
    /// path failed. A restart attempt may restore the previous request once.
    pub fn on_start_failed(&mut self, message: &str, now: Instant) -> Vec<Outcome> {
        self.fail_local(message, now)
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Outcome> {
        if self.viewer_wait.is_some_and(|deadline| now >= deadline) {
            return self.viewer_left(now);
        }
        if self.stop_deadline.is_some_and(|deadline| now >= deadline) {
            return self.finish();
        }
        if self.phase == Phase::Publishing && self.deadline.is_some_and(|deadline| now >= deadline)
        {
            return self.fail("Сервер не подтвердил трансляцию.", now);
        }
        Vec::new()
    }

    /// `stream.*` events. Anything that does not belong to this attempt is
    /// ignored, so a late offer of an old attempt cannot start a capture.
    pub fn on_server_event(
        &mut self,
        kind: &str,
        request_id: &str,
        data: &Value,
        now: Instant,
    ) -> Vec<Outcome> {
        match kind {
            "stream.started" => self.on_started(request_id, data, now),
            "stream.webrtc_offer" => self.on_offer(data),
            "stream.ice_candidate" => self.on_ice(data),
            "stream.webrtc_answered" => {
                if !self.active() || !self.tracked(request_id) {
                    return Vec::new();
                }
                if !data["accepted"].as_bool().unwrap_or(false) {
                    return self.fail("Сервер отклонил ответ трансляции.", now);
                }
                self.answered = true;
                self.status = "Подключаемся…".into();
                vec![Outcome::State(self.sharing())]
            }
            "stream.stopped" => self.on_stopped(request_id, data, now),
            // The pinned backend may acknowledge `stream.stop` with
            // `stream.left` instead of `stream.stopped`. Only a tagged answer
            // of our own stop request finishes the attempt; an untagged leave
            // of the viewer stays unrelated.
            "stream.left" => self.on_left_ack(request_id),
            "stream.webrtc_closed" => self.on_closed(data, now),
            "error" => {
                if !self.active() || !self.tracked(request_id) {
                    return Vec::new();
                }
                self.fail(&reason::server_error(data), now)
            }
            _ => Vec::new(),
        }
    }

    /// Events of the current media engine. The engine exists only inside an
    /// active attempt; anything else is a late update of a released capture.
    pub fn on_engine(&mut self, event: EngineEvent, now: Instant) -> Vec<Outcome> {
        if !self.active() {
            return Vec::new();
        }
        match event {
            EngineEvent::Answer(sdp) => {
                if self.phase != Phase::Publishing || sdp.is_empty() {
                    return Vec::new();
                }
                self.answered = false;
                let signal = self.signal("stream.webrtc_answer", json!({"sdp": sdp}));
                vec![Outcome::Send(signal)]
            }
            EngineEvent::Ice(candidate) => {
                let data = json!({
                    "candidate": candidate.candidate,
                    "sdp_mid": candidate.sdp_mid,
                    "sdp_mline_index": candidate.sdp_mline_index,
                    "username_fragment": candidate.username_fragment,
                });
                vec![Outcome::Send(self.signal("stream.ice_candidate", data))]
            }
            EngineEvent::State(Connection::Connected) => {
                self.connected = true;
                self.maybe_live()
            }
            EngineEvent::State(Connection::FirstFrame) => {
                self.sent_frame = true;
                self.maybe_live()
            }
            EngineEvent::Stats(stats) => {
                if self.phase == Phase::Live && self.stats != Some(stats) {
                    self.stats = Some(stats);
                    return vec![Outcome::State(self.sharing())];
                }
                Vec::new()
            }
            EngineEvent::Error(message) => self.fail_local(&message, now),
            EngineEvent::Closed => self.fail_local("Соединение трансляции закрыто.", now),
        }
    }

    fn on_started(&mut self, request_id: &str, data: &Value, now: Instant) -> Vec<Outcome> {
        if self.phase != Phase::Publishing {
            return Vec::new();
        }
        if data["channel_id"]
            .as_i64()
            .is_some_and(|channel| channel != self.channel)
        {
            return Vec::new();
        }
        let publisher = data["publisher_connection_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let by_request =
            !request_id.is_empty() && self.start_request.as_deref() == Some(request_id);
        let by_identity = self
            .self_connection
            .as_deref()
            .is_some_and(|connection| connection == publisher);
        if !by_request && !by_identity {
            return Vec::new();
        }
        if !publisher.is_empty() {
            self.started = Some(publisher);
        }
        self.deadline = Some(now + ANSWER_TIMEOUT);
        self.status = "Подключаемся…".into();
        vec![Outcome::State(self.sharing())]
    }

    fn on_offer(&mut self, data: &Value) -> Vec<Outcome> {
        if !self.active() {
            return Vec::new();
        }
        let Some(sdp) = data["sdp"].as_str().filter(|sdp| !sdp.is_empty()) else {
            return Vec::new();
        };
        // The offer may arrive before `stream.started`; the engine with its
        // sendonly transceiver exists since `StartEngine`, so the answer is
        // produced without waiting for the started event.
        vec![Outcome::Offer(sdp.to_owned())]
    }

    fn on_ice(&mut self, data: &Value) -> Vec<Outcome> {
        if !self.active() {
            return Vec::new();
        }
        let candidate = crate::Candidate {
            candidate: data["candidate"].as_str().unwrap_or_default().into(),
            sdp_mid: data["sdp_mid"].as_str().map(str::to_owned),
            sdp_mline_index: data["sdp_mline_index"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok()),
            username_fragment: data["username_fragment"].as_str().map(str::to_owned),
        };
        if candidate.candidate.is_empty() {
            return Vec::new();
        }
        vec![Outcome::Ice(candidate)]
    }

    fn on_stopped(&mut self, request_id: &str, data: &Value, now: Instant) -> Vec<Outcome> {
        let channel = data["channel_id"].as_i64().unwrap_or_default();
        if channel != 0 && channel != self.channel {
            return Vec::new();
        }
        let publisher = data["publisher_connection_id"].as_str().unwrap_or_default();
        if !self.is_ours(publisher) {
            return Vec::new();
        }
        let reason = data["reason"].as_str().unwrap_or_default();
        match self.phase {
            Phase::Stopping => self.finish(),
            Phase::Publishing | Phase::Live => {
                // The backend publishes the stop to the server and also
                // answers the request directly. The second copy repeats the
                // user-stop reason; it belongs to the attempt we already
                // closed and must not kill the new one. A tagged event is
                // equally stale: no current publication can be stopped by
                // our own older request.
                if self.ignore_stop_echo
                    && (reason.is_empty() || reason == "stream stopped by user")
                {
                    return Vec::new();
                }
                if !request_id.is_empty() && !self.requests.contains(request_id) {
                    return Vec::new();
                }
                let message = if reason.is_empty() || reason == "stream stopped by user" {
                    "Трансляция остановлена."
                } else {
                    reason::reason_message(reason)
                };
                self.fail(message, now)
            }
            _ => Vec::new(),
        }
    }

    /// `stream.left` is a valid acknowledgement of our own `stream.stop`.
    fn on_left_ack(&mut self, request_id: &str) -> Vec<Outcome> {
        if self.phase != Phase::Stopping
            || request_id.is_empty()
            || !self.requests.contains(request_id)
        {
            return Vec::new();
        }
        self.finish()
    }

    fn on_closed(&mut self, data: &Value, now: Instant) -> Vec<Outcome> {
        let reason = data["reason"].as_str().unwrap_or_default();
        match self.phase {
            Phase::Publishing | Phase::Live => {
                // The backend closes the old media session while it handles
                // `stream.stop`; its `stream.webrtc_closed` can arrive after
                // the confirmed stop and belongs to the attempt that is
                // already gone. A real failure of the new peer is reported by
                // the local `EngineEvent::Closed`/error path.
                if self.ignore_stop_echo && reason::stale_media_close(reason) {
                    return Vec::new();
                }
                let message = if reason.is_empty() {
                    "Соединение трансляции закрыто сервером.".to_owned()
                } else {
                    reason::reason_message(reason).to_owned()
                };
                self.fail(&message, now)
            }
            Phase::Stopping => self.finish(),
            _ => Vec::new(),
        }
    }

    fn is_ours(&self, publisher: &str) -> bool {
        publisher.is_empty()
            || self.started.as_deref() == Some(publisher)
            || self
                .self_connection
                .as_deref()
                .is_some_and(|connection| connection == publisher)
    }

    /// An engine exists and its signaling is still wanted.
    fn active(&self) -> bool {
        matches!(self.phase, Phase::Publishing | Phase::Live)
    }

    fn tracked(&self, request_id: &str) -> bool {
        // The pinned backend tags answers and errors with the request id; an
        // untagged error while publishing still belongs to this attempt.
        request_id.is_empty() || self.requests.contains(request_id)
    }

    fn start(&mut self, now: Instant) -> Vec<Outcome> {
        self.viewer_wait = None;
        self.phase = Phase::Publishing;
        self.status = "Запускаем трансляцию…".into();
        // A new attempt never inherits the confirmations or the stop latch of
        // the previous one. `stream.start` is not sent here: `engine_ready`
        // does that only after the media engine really exists.
        self.start_request = None;
        self.start_sent = false;
        self.engine_live = false;
        self.answered = false;
        self.stop_sent = false;
        self.connected = false;
        self.sent_frame = false;
        self.started = None;
        self.deadline = None;
        self.stop_deadline = None;
        self.stats = None;
        let Some(request) = self.request.clone() else {
            return self.fail("Источник трансляции не выбран.", now);
        };
        vec![Outcome::StartEngine {
            attempt: self.attempt,
            request,
        }]
    }

    /// The engine of this attempt was created. Only now may `stream.start`
    /// reach the server: a failed engine start must not leave a phantom SFU
    /// publication or a second "Запускаем…" state behind.
    ///
    /// `has_audio` is what the engine really opened, not the user's request:
    /// the backend adds exactly one recvonly audio m-line from this flag.
    /// `audio_scope` is local only: the pinned `stream.start` contract has no
    /// scope field, so it is shown on the local card and never sent.
    pub fn engine_ready(
        &mut self,
        now: Instant,
        has_audio: bool,
        audio_scope: Option<String>,
    ) -> Vec<Outcome> {
        if self.phase != Phase::Publishing || self.start_sent {
            return Vec::new();
        }
        let Some(request) = self.request.clone() else {
            return self.fail("Источник трансляции не выбран.", now);
        };
        self.has_audio = has_audio;
        self.audio_scope = if has_audio { audio_scope } else { None };
        let signal = self.signal(
            "stream.start",
            json!({
                "server_id": self.server,
                "channel_id": self.channel,
                "mode": "server",
                "codec": request.codec.wire(),
                "has_audio": has_audio,
            }),
        );
        self.start_request = signal.request_id.clone();
        self.start_sent = true;
        self.engine_live = true;
        self.deadline = Some(now + ANSWER_TIMEOUT);
        self.status = "Подключаемся…".into();
        vec![Outcome::Send(signal), Outcome::State(self.sharing())]
    }

    fn maybe_live(&mut self) -> Vec<Outcome> {
        if self.phase != Phase::Publishing || !self.connected || !self.sent_frame {
            return Vec::new();
        }
        self.phase = Phase::Live;
        self.deadline = None;
        self.status = "В эфире".into();
        // Only an attempt that really reached the air becomes the confirmed
        // configuration. A preflight or an in-flight restart never does. The
        // audio flag is the track the engine actually opened, not merely the
        // checkbox the picker sent.
        self.confirmed = self.request.as_ref().map(|request| {
            let mut confirmed = Confirmed::from_request(self.attempt, request);
            confirmed.audio = self.has_audio;
            confirmed
        });
        // A fresh confirmed attempt can serve as the rollback target of the
        // next restart and may restore itself once.
        self.rollback = None;
        self.rollback_used = false;
        self.preflighting = false;
        vec![Outcome::State(self.sharing())]
    }

    fn fail(&mut self, message: &str, _now: Instant) -> Vec<Outcome> {
        if self.phase == Phase::Idle {
            return Vec::new();
        }
        let engine = self.engine_live;
        let mut out = Vec::new();
        if engine {
            out.push(Outcome::StopEngine);
        }
        if self.start_sent && !self.stop_sent && self.phase != Phase::Stopping {
            // The server may already hold a publication for this connection.
            self.stop_sent = true;
            out.push(Outcome::Send(self.signal("stream.stop", json!({}))));
        }
        self.phase = Phase::Failed;
        self.status = message.to_owned();
        self.error = Some(message.to_owned());
        self.stats = None;
        self.viewer_wait = None;
        self.deadline = None;
        self.stop_deadline = None;
        self.pending_restart = None;
        self.rollback = None;
        self.rollback_used = false;
        self.restarting = false;
        self.preflighting = false;
        self.quality_status = None;
        self.ignore_stop_echo = false;
        self.has_audio = false;
        self.audio_scope = None;
        self.confirmed = None;
        out.push(Outcome::State(self.sharing()));
        out
    }

    fn finish(&mut self) -> Vec<Outcome> {
        // `stop` already released the engine when the attempt entered
        // `Stopping`; the acknowledgement only closes the attempt. A queued
        // quality change starts its new attempt here, after the stop really
        // completed, so the backend never sees two overlapping starts.
        if let Some(request) = self.pending_restart.take() {
            return self.start_restart(request);
        }
        self.reset();
        vec![Outcome::State(self.sharing())]
    }

    /// The stop handshake of a live quality change completed. A fresh attempt
    /// with a new capture and a new `stream.start` begins.
    fn start_restart(&mut self, request: PublishRequest) -> Vec<Outcome> {
        self.attempt = self.attempt.wrapping_add(1);
        self.phase = Phase::Publishing;
        self.status = "Запускаем трансляцию…".into();
        self.request = Some(request.clone());
        self.picker = None;
        self.start_request = None;
        self.requests.clear();
        self.started = None;
        self.start_sent = false;
        self.engine_live = false;
        self.answered = false;
        self.stop_sent = false;
        self.connected = false;
        self.sent_frame = false;
        self.viewer_wait = None;
        self.deadline = None;
        self.stop_deadline = None;
        self.stats = None;
        self.error = None;
        self.restarting = false;
        self.preflighting = false;
        self.has_audio = false;
        self.audio_scope = None;
        vec![
            Outcome::StartEngine {
                attempt: self.attempt,
                request,
            },
            Outcome::State(self.sharing()),
        ]
    }

    fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.request = None;
        self.pending_restart = None;
        self.rollback = None;
        self.rollback_used = false;
        self.restarting = false;
        self.preflighting = false;
        self.quality_status = None;
        self.ignore_stop_echo = false;
        self.has_audio = false;
        self.audio_scope = None;
        self.confirmed = None;
        self.picker = None;
        self.start_request = None;
        self.requests.clear();
        self.started = None;
        self.start_sent = false;
        self.engine_live = false;
        self.answered = false;
        self.stop_sent = false;
        self.connected = false;
        self.sent_frame = false;
        self.viewer_wait = None;
        self.deadline = None;
        self.stop_deadline = None;
        self.status.clear();
        self.error = None;
        self.stats = None;
    }

    fn signal(&mut self, kind: &'static str, data: Value) -> Signal {
        self.sequence = self.sequence.wrapping_add(1);
        let request_id = Some(format!("native-screen-{}", self.sequence));
        if let Some(id) = &request_id {
            self.requests.insert(id.clone());
        }
        Signal {
            kind,
            data,
            request_id,
        }
    }
}

pub mod engine;

mod live_change;
pub mod preflight;
mod reason;

#[cfg(test)]
mod tests;
