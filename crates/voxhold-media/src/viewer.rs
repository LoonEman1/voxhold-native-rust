//! Deterministic state machine for watching one screen publication.
//!
//! The runtime owns sockets and media engines; this module only decides when a
//! watch starts, when the previous attempt must be left first, which signaling
//! belongs to the current attempt and when a viewer is closed. It takes the
//! clock as an argument, so switching publications, the five-second leave wait
//! and the recovery deadline are testable without WebRTC or timers.

use crate::Candidate;
use crate::catalog::StreamCatalog;
use crate::controller::{Publication, Signal};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

/// Go waits this long for `stream.left` before it abandons the switch.
pub const LEAVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Go re-arms this deadline on every viewing attempt and recovery request.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Two recovery requests, then the third failure ends the viewer.
pub const MAX_RETRIES: u8 = 2;
/// A close from the attempt we just left can arrive late. Until the new
/// attempt has produced a signal of its own, one untagged close is ignored.
const STALE_CLOSE_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
struct Intent {
    selection: u64,
    attempt: u64,
    publication: Publication,
    muted: bool,
}

#[derive(Clone, Debug)]
struct Active {
    selection: u64,
    attempt: u64,
    watch_request: String,
    /// Request ids whose error belongs to this attempt. Numbering is not
    /// enough: a request from a previous attempt may have the same number.
    requests: HashSet<String>,
    publication: Publication,
    connected: bool,
    recovering: bool,
    received_signal: bool,
    remote_ufrags: Vec<String>,
}

/// One decision produced by the state machine. The runtime executes them in
/// order and maps them to WebSocket signals, media commands or UI updates.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Create an engine with this attempt's watch request.
    Start {
        selection: u64,
        publication: Publication,
        muted: bool,
        request: Signal,
    },
    /// Drop the engine, decoder and frame buffers of the previous attempt.
    Close,
    Send(Signal),
    Offer(String),
    Ice(Candidate),
    Mute {
        selection: u64,
        muted: bool,
    },
    Status {
        selection: u64,
        text: String,
    },
    Connected {
        selection: u64,
        connected: bool,
    },
    Clear {
        selection: u64,
    },
    Failed {
        selection: u64,
        message: String,
    },
    /// The publication list changed and the panel should read it again.
    Catalog,
}

/// Identity of the active attempt: the publication plus its attempt number.
#[derive(Clone, Debug)]
pub struct Watching {
    pub selection: u64,
    pub attempt: u64,
    pub publication: Publication,
    pub connected: bool,
    pub recovering: bool,
}

pub struct Viewer {
    server: i64,
    channel: i64,
    catalog: StreamCatalog,
    joined: bool,
    /// The current stream snapshot is authoritative for this connection. A
    /// reconnect clears it together with the catalog.
    snapshot_ready: bool,
    intent: Option<Intent>,
    active: Option<Active>,
    pending_leave: Option<(String, Instant)>,
    deadline: Option<Instant>,
    retries: u8,
    attempt: u64,
    sequence: u64,
    /// Bumped on every leave so a delayed untagged close from the old attempt
    /// cannot end the new one before it has its own signaling.
    stale_close_until: Option<Instant>,
    stale_close_available: bool,
    retired_ufrags: HashSet<String>,
}

impl Viewer {
    pub fn new(server: i64, channel: i64) -> Self {
        Self {
            server,
            channel,
            catalog: StreamCatalog::new(server, channel),
            joined: false,
            snapshot_ready: false,
            intent: None,
            active: None,
            pending_leave: None,
            deadline: None,
            retries: 0,
            attempt: 0,
            sequence: 0,
            stale_close_until: None,
            stale_close_available: false,
            retired_ufrags: HashSet::new(),
        }
    }

    pub fn catalog(&self) -> &StreamCatalog {
        &self.catalog
    }

    /// The publication the user selected, even while it is waiting for the
    /// previous attempt to leave.
    pub fn intent(&self) -> Option<&Publication> {
        self.intent.as_ref().map(|intent| &intent.publication)
    }

    pub fn selection(&self) -> Option<u64> {
        self.intent
            .as_ref()
            .map(|intent| intent.selection)
            .or_else(|| self.active.as_ref().map(|active| active.selection))
    }

    pub fn watching(&self) -> Option<Watching> {
        self.active.as_ref().map(|active| Watching {
            selection: active.selection,
            attempt: active.attempt,
            publication: active.publication.clone(),
            connected: active.connected,
            recovering: active.recovering,
        })
    }

    pub fn muted(&self) -> bool {
        self.intent.as_ref().is_some_and(|intent| intent.muted)
    }

    /// `voice.joined`/transport state. On a reconnect the old engine, catalog
    /// and leave request are gone; the selected publication stays and is
    /// watched again only after both a fresh snapshot and the join confirm.
    pub fn set_joined(&mut self, joined: bool, now: Instant) -> Vec<Outcome> {
        if self.joined == joined {
            return Vec::new();
        }
        self.joined = joined;
        if !joined {
            self.pending_leave = None;
            self.deadline = None;
            self.snapshot_ready = false;
            self.retired_ufrags.clear();
            self.catalog.replace(std::iter::empty());
            let mut out = Vec::new();
            if let Some(active) = self.active.take() {
                self.retire(&active);
                out.push(Outcome::Close);
            }
            if let Some(intent) = &self.intent {
                out.push(Outcome::Status {
                    selection: intent.selection,
                    text: "Восстанавливаем трансляцию…".into(),
                });
                out.push(Outcome::Connected {
                    selection: intent.selection,
                    connected: false,
                });
            }
            return out;
        }
        if self.snapshot_ready {
            return self.start_if_ready(now);
        }
        Vec::new()
    }

    /// The user picked a publication. The previous attempt is closed and left,
    /// and the new watch starts only after `stream.left` or the timeout.
    pub fn select(
        &mut self,
        publication: &Publication,
        selection: u64,
        now: Instant,
    ) -> Vec<Outcome> {
        let Some(current) = self
            .catalog
            .get(
                publication.server_id,
                publication.channel_id,
                &publication.publisher_connection_id,
            )
            .cloned()
        else {
            let mut out = self.close_active(now);
            self.intent = None;
            self.deadline = None;
            out.push(Outcome::Failed {
                selection,
                message: "Выбранная трансляция больше недоступна".into(),
            });
            return out;
        };
        let mut out = Vec::new();
        if self.intent.is_some() {
            out.extend(self.close_active(now));
        }
        self.attempt = self.attempt.wrapping_add(1);
        self.intent = Some(Intent {
            selection,
            attempt: self.attempt,
            publication: current,
            muted: false,
        });
        self.retries = 0;
        if self.joined && self.snapshot_ready {
            out.extend(self.start_if_ready(now));
        }
        out
    }

    /// Remember a selection made while a reconnect is in progress. It can only
    /// start once the fresh snapshot and the join both arrive.
    pub fn select_intent(&mut self, publication: Publication, selection: u64) {
        self.attempt = self.attempt.wrapping_add(1);
        self.intent = Some(Intent {
            selection,
            attempt: self.attempt,
            publication,
            muted: false,
        });
        self.active = None;
        self.pending_leave = None;
        self.deadline = None;
        self.retries = 0;
    }

    /// Close only the attempt that the command names. A delayed close for A
    /// must not end B after the user has already selected it.
    pub fn leave_selection(&mut self, selection: u64, now: Instant) -> Vec<Outcome> {
        let matches = self.selection() == Some(selection)
            || self
                .active
                .as_ref()
                .is_some_and(|active| active.selection == selection);
        if !matches {
            return Vec::new();
        }
        self.leave(now)
    }

    /// The user closed the player. Voice stays joined.
    pub fn leave(&mut self, now: Instant) -> Vec<Outcome> {
        let selection = self.selection();
        if self.intent.is_none() && self.active.is_none() {
            return Vec::new();
        }
        self.intent = None;
        let mut out = self.close_active(now);
        if let Some(selection) = selection {
            out.push(Outcome::Clear { selection });
        }
        out
    }

    pub fn set_muted(&mut self, selection: u64, muted: bool) -> Vec<Outcome> {
        let Some(intent) = &mut self.intent else {
            return Vec::new();
        };
        if intent.selection != selection {
            return Vec::new();
        }
        intent.muted = muted;
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.selection == selection)
        {
            vec![Outcome::Mute { selection, muted }]
        } else {
            Vec::new()
        }
    }

    /// `stream.*` events. Signaling from an attempt that is no longer active
    /// is ignored, so a late offer/ICE of A can never start or break B.
    pub fn on_server_event(
        &mut self,
        kind: &str,
        request_id: &str,
        data: &Value,
        now: Instant,
    ) -> Vec<Outcome> {
        match kind {
            "stream.snapshot" => self.on_snapshot(data, now),
            "stream.started" | "stream.updated" => {
                let Ok(publication) = serde_json::from_value::<Publication>(data.clone()) else {
                    return Vec::new();
                };
                let changed = self.catalog.upsert(publication.clone());
                self.refresh_publication(&publication);
                if changed {
                    vec![Outcome::Catalog]
                } else {
                    Vec::new()
                }
            }
            "stream.stopped" => {
                let channel = data["channel_id"].as_i64().unwrap_or_default();
                let publisher = data["publisher_connection_id"].as_str().unwrap_or_default();
                let mut out = Vec::new();
                if self.catalog.stop(self.server, channel, publisher) {
                    out.push(Outcome::Catalog);
                }
                let matches = self.active.as_ref().is_some_and(|active| {
                    channel == self.channel
                        && (publisher.is_empty()
                            || publisher == active.publication.publisher_connection_id)
                });
                if matches {
                    out.extend(self.fail("Трансляция завершена", now));
                }
                out
            }
            "stream.watching" => self.on_watching(request_id, data, now),
            "stream.webrtc_offer" | "stream.p2p_offer" => self.on_offer(kind, request_id, data),
            "stream.ice_candidate" | "stream.p2p_ice_candidate" => {
                self.on_ice_event(kind, request_id, data)
            }
            "stream.left" => {
                if self
                    .pending_leave
                    .as_ref()
                    .is_some_and(|(id, _)| id == request_id)
                {
                    self.pending_leave = None;
                    if self.joined && self.intent.is_some() && self.active.is_none() {
                        return self.start_if_ready(now);
                    }
                    return Vec::new();
                }
                if self.active.is_some() && request_id.is_empty() {
                    return self.fail("Просмотр трансляции завершён сервером", now);
                }
                Vec::new()
            }
            "stream.webrtc_closed" => self.on_closed(request_id, data, now),
            "error" => {
                let Some(active) = &self.active else {
                    return Vec::new();
                };
                if request_id.is_empty() || !active.requests.contains(request_id) {
                    return Vec::new();
                }
                self.fail("Сервер отклонил просмотр трансляции", now)
            }
            _ => Vec::new(),
        }
    }

    fn on_snapshot(&mut self, data: &Value, now: Instant) -> Vec<Outcome> {
        let streams = match serde_json::from_value::<Vec<Publication>>(data["streams"].clone()) {
            Ok(streams) => streams,
            Err(_) => return Vec::new(),
        };
        let changed = self.catalog.replace(streams);
        self.snapshot_ready = true;
        self.refresh_intent_from_catalog();
        if self.joined && self.intent.is_some() && self.active.is_none() {
            let mut out = if changed {
                vec![Outcome::Catalog]
            } else {
                Vec::new()
            };
            out.extend(self.start_if_ready(now));
            return out;
        }
        if changed {
            vec![Outcome::Catalog]
        } else {
            Vec::new()
        }
    }

    /// Keep the selected publication's counters current without ever changing
    /// its identity (server, channel, user and publisher connection).
    fn refresh_publication(&mut self, publication: &Publication) {
        if let Some(intent) = &mut self.intent {
            if same_identity(&intent.publication, publication) {
                intent.publication = publication.clone();
            }
        }
        if let Some(active) = &mut self.active {
            if same_identity(&active.publication, publication) {
                active.publication = publication.clone();
            }
        }
    }

    fn refresh_intent_from_catalog(&mut self) {
        let Some(intent) = &mut self.intent else {
            return;
        };
        if let Some(current) = self.catalog.get(
            intent.publication.server_id,
            intent.publication.channel_id,
            &intent.publication.publisher_connection_id,
        ) {
            intent.publication = current.clone();
            if let Some(active) = &mut self.active {
                if same_identity(&active.publication, current) {
                    active.publication = current.clone();
                }
            }
        }
    }

    fn on_watching(&mut self, request_id: &str, data: &Value, now: Instant) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        if request_id != active.watch_request {
            return Vec::new();
        }
        let Ok(publication) = serde_json::from_value::<Publication>(data["stream"].clone()) else {
            return self.fail("Некорректный ответ сервера", now);
        };
        if !same_identity(&publication, &active.publication) {
            return self.fail("Сервер выбрал другую трансляцию", now);
        }
        if let Some(active) = &mut self.active {
            active.publication = publication.clone();
            // The new attempt now has its own server confirmation. A close
            // from the attempt we left can no longer be mistaken for this one.
            active.received_signal = true;
            self.stale_close_until = None;
            self.stale_close_available = false;
        }
        if let Some(intent) = &mut self.intent {
            intent.publication = publication.clone();
        }
        let changed = self.catalog.upsert(publication);
        if changed {
            vec![Outcome::Catalog]
        } else {
            Vec::new()
        }
    }

    fn on_offer(&mut self, kind: &str, request_id: &str, data: &Value) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        if !request_matches(request_id, &active.watch_request) {
            return Vec::new();
        }
        let p2p = kind == "stream.p2p_offer";
        let sender = data["from_connection_id"].as_str().unwrap_or_default();
        let mode = active.publication.mode.as_str();
        let accepted = if p2p {
            mode == "p2p" && sender == active.publication.publisher_connection_id
        } else {
            mode == "server"
        };
        let Some(sdp) = data["sdp"].as_str().filter(|sdp| !sdp.is_empty()) else {
            return Vec::new();
        };
        if !accepted {
            return Vec::new();
        }
        let ufrags = sdp_ufrags(sdp);
        if !ufrags.is_empty()
            && ufrags
                .iter()
                .all(|ufrag| self.retired_ufrags.contains(ufrag))
        {
            return Vec::new();
        }
        if let Some(active) = &mut self.active {
            active.remote_ufrags = ufrags;
            active.received_signal = true;
            self.stale_close_until = None;
            self.stale_close_available = false;
        }
        vec![Outcome::Offer(sdp.to_owned())]
    }

    fn on_ice_event(&mut self, kind: &str, request_id: &str, data: &Value) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        if !request_matches(request_id, &active.watch_request) {
            return Vec::new();
        }
        let p2p = kind == "stream.p2p_ice_candidate";
        let sender = data["from_connection_id"].as_str().unwrap_or_default();
        let mode = active.publication.mode.as_str();
        let accepted = if p2p {
            mode == "p2p" && sender == active.publication.publisher_connection_id
        } else {
            mode == "server"
        };
        if !accepted {
            return Vec::new();
        }
        let candidate = Candidate {
            candidate: data["candidate"].as_str().unwrap_or_default().into(),
            sdp_mid: data["sdp_mid"].as_str().map(str::to_owned),
            sdp_mline_index: data["sdp_mline_index"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok()),
            username_fragment: data["username_fragment"].as_str().map(str::to_owned),
        };
        let ufrag = candidate
            .username_fragment
            .as_deref()
            .or_else(|| candidate_ufrag(&candidate.candidate));
        if let Some(ufrag) = ufrag {
            if self.retired_ufrags.contains(ufrag) {
                return Vec::new();
            }
            if !active.remote_ufrags.is_empty()
                && !active.remote_ufrags.iter().any(|current| current == ufrag)
            {
                return Vec::new();
            }
        }
        let Some(active) = &mut self.active else {
            return Vec::new();
        };
        active.received_signal = true;
        self.stale_close_until = None;
        self.stale_close_available = false;
        vec![Outcome::Ice(candidate)]
    }

    fn on_closed(&mut self, request_id: &str, data: &Value, now: Instant) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        // A backend that can attribute the close is always trusted. Older
        // payloads have no attempt id, so one close immediately after our own
        // leave is treated as the tail of the previous attempt until the new
        // one has its own offer or watching acknowledgement.
        let tagged = data["watch_id"]
            .as_str()
            .or_else(|| data["request_id"].as_str())
            .or_else(|| data["publisher_connection_id"].as_str())
            .or_else(|| data["from_connection_id"].as_str());
        let tagged = tagged.filter(|value| !value.is_empty());
        if let Some(tag) = tagged {
            if tag != active.watch_request
                && tag != active.publication.publisher_connection_id
                && request_id != active.watch_request
            {
                return Vec::new();
            }
        } else if !active.received_signal
            && self.stale_close_available
            && self
                .stale_close_until
                .is_some_and(|deadline| now < deadline)
        {
            self.stale_close_available = false;
            return Vec::new();
        }
        let reason = data["reason"].as_str().unwrap_or_default();
        if final_close(reason) || self.retries >= MAX_RETRIES {
            return self.fail("Трансляция завершена", now);
        }
        self.retries += 1;
        let selection = active.selection;
        let mut out = vec![
            Outcome::Status {
                selection,
                text: "Восстанавливаем трансляцию…".into(),
            },
            Outcome::Connected {
                selection,
                connected: false,
            },
        ];
        out.extend(self.close_active(now));
        out
    }

    pub fn on_answer(&mut self, sdp: String) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let p2p = active.publication.mode == "p2p";
        let mut data = json!({"sdp":sdp});
        if p2p {
            data["target_connection_id"] = json!(active.publication.publisher_connection_id);
        }
        vec![Outcome::Send(self.signal(
            if p2p {
                "stream.p2p_answer"
            } else {
                "stream.webrtc_answer"
            },
            data,
        ))]
    }

    pub fn on_ice(&mut self, candidate: Candidate) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let p2p = active.publication.mode == "p2p";
        let mut data = json!({"candidate":candidate.candidate,"sdp_mid":candidate.sdp_mid,"sdp_mline_index":candidate.sdp_mline_index,"username_fragment":candidate.username_fragment});
        if p2p {
            data["target_connection_id"] = json!(active.publication.publisher_connection_id);
        }
        vec![Outcome::Send(self.signal(
            if p2p {
                "stream.p2p_ice_candidate"
            } else {
                "stream.ice_candidate"
            },
            data,
        ))]
    }

    /// First decoded frame of this attempt confirms the picture. While
    /// recovering, the first frame ends recovery and the deadline instead of
    /// leaving the old deadline armed behind a working stream.
    pub fn on_frame(&mut self) -> Vec<Outcome> {
        let Some(active) = &mut self.active else {
            return Vec::new();
        };
        if active.connected && !active.recovering {
            return Vec::new();
        }
        active.connected = true;
        active.recovering = false;
        active.received_signal = true;
        self.deadline = None;
        let selection = active.selection;
        vec![
            Outcome::Status {
                selection,
                text: "Подключено".into(),
            },
            Outcome::Connected {
                selection,
                connected: true,
            },
        ]
    }

    /// The engine reports `failed`/`disconnected`: ask the server for one
    /// bounded recovery and keep the picture until the deadline.
    pub fn on_engine_state(&mut self, state: &str, now: Instant) -> Vec<Outcome> {
        if state != "failed" && state != "disconnected" {
            return Vec::new();
        }
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let selection = active.selection;
        let p2p = active.publication.mode == "p2p";
        let publisher = active.publication.publisher_connection_id.clone();
        let server = active.publication.server_id;
        let channel = active.publication.channel_id;
        self.deadline = Some(now + START_TIMEOUT);
        if let Some(active) = &mut self.active {
            active.recovering = true;
            active.connected = false;
        }
        let mut out = vec![
            Outcome::Status {
                selection,
                text: "Восстанавливаем трансляцию…".into(),
            },
            Outcome::Connected {
                selection,
                connected: false,
            },
        ];
        if self.retries < MAX_RETRIES {
            self.retries += 1;
            let data = if p2p {
                json!({"target_connection_id":publisher})
            } else {
                json!({"server_id":server,"channel_id":channel,"action":"ice_restart"})
            };
            let signal = self.signal(
                if p2p {
                    "stream.p2p_restart"
                } else {
                    "stream.recovery_request"
                },
                data,
            );
            self.track_request(signal.request_id.as_deref());
            out.push(Outcome::Send(signal));
        }
        out
    }

    pub fn on_engine_error(&mut self, message: &str, now: Instant) -> Vec<Outcome> {
        self.fail(message, now)
    }

    pub fn on_start_failed(&mut self, message: &str, now: Instant) -> Vec<Outcome> {
        self.fail(message, now)
    }

    /// Deadline handling. Called by the runtime tick; the state machine never
    /// reads the clock itself.
    pub fn tick(&mut self, now: Instant) -> Vec<Outcome> {
        let mut out = Vec::new();
        if self
            .pending_leave
            .as_ref()
            .is_some_and(|(_, deadline)| now >= *deadline)
        {
            self.pending_leave = None;
            if self.intent.is_some() {
                out.extend(self.fail("Не удалось завершить предыдущий просмотр", now));
            }
        }
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            if self.active.is_some() {
                if self.retries >= MAX_RETRIES {
                    out.extend(self.fail("Не удалось восстановить трансляцию", now));
                } else {
                    self.retries += 1;
                    out.extend(self.close_active(now));
                    if let Some(selection) = self.selection() {
                        out.push(Outcome::Status {
                            selection,
                            text: "Восстанавливаем трансляцию…".into(),
                        });
                        out.push(Outcome::Connected {
                            selection,
                            connected: false,
                        });
                    }
                }
            }
        }
        out
    }

    fn start_if_ready(&mut self, now: Instant) -> Vec<Outcome> {
        if !self.joined || !self.snapshot_ready || self.pending_leave.is_some() {
            return Vec::new();
        }
        if self.active.is_some() {
            return Vec::new();
        }
        let Some(intent) = self.intent.clone() else {
            return Vec::new();
        };
        // The fresh snapshot is authoritative for this connection. A missing
        // exact publication is not re-targeted to another connection of the
        // same user; that would open a different screen.
        let Some(current) = self.catalog.get(
            intent.publication.server_id,
            intent.publication.channel_id,
            &intent.publication.publisher_connection_id,
        ) else {
            self.intent = None;
            return vec![Outcome::Failed {
                selection: intent.selection,
                message: "Выбранная трансляция больше недоступна".into(),
            }];
        };
        let publication = current.clone();
        self.start(intent, publication, now)
    }

    fn start(&mut self, intent: Intent, publication: Publication, now: Instant) -> Vec<Outcome> {
        let selection = intent.selection;
        let muted = intent.muted;
        let request = self.signal(
            "stream.watch",
            json!({
                "server_id": publication.server_id,
                "channel_id": publication.channel_id,
                "publisher_connection_id": publication.publisher_connection_id,
                "supported_dynamic_ranges": ["sdr"],
            }),
        );
        let watch_request = request.request_id.clone().unwrap_or_default();
        self.active = Some(Active {
            selection,
            attempt: intent.attempt,
            watch_request: watch_request.clone(),
            requests: HashSet::from([watch_request]),
            publication: publication.clone(),
            connected: false,
            recovering: false,
            received_signal: false,
            remote_ufrags: Vec::new(),
        });
        self.intent = Some(Intent {
            publication: publication.clone(),
            ..intent
        });
        self.deadline = Some(now + START_TIMEOUT);
        vec![
            Outcome::Status {
                selection,
                text: "Подключение…".into(),
            },
            Outcome::Connected {
                selection,
                connected: false,
            },
            Outcome::Start {
                selection,
                publication,
                muted,
                request,
            },
        ]
    }

    fn close_active(&mut self, now: Instant) -> Vec<Outcome> {
        let mut out = Vec::new();
        if let Some(active) = self.active.take() {
            self.retire(&active);
            out.push(Outcome::Close);
            self.stale_close_until = Some(now + STALE_CLOSE_GRACE);
            self.stale_close_available = true;
        }
        self.deadline = None;
        out.extend(self.request_leave(now));
        out
    }

    fn retire(&mut self, active: &Active) {
        for ufrag in &active.remote_ufrags {
            self.retired_ufrags.insert(ufrag.clone());
        }
    }

    fn request_leave(&mut self, now: Instant) -> Vec<Outcome> {
        if !self.joined || self.pending_leave.is_some() {
            return Vec::new();
        }
        let signal = self.signal("stream.leave", json!({}));
        let id = signal.request_id.clone().unwrap_or_default();
        self.pending_leave = Some((id, now + LEAVE_TIMEOUT));
        vec![Outcome::Send(signal)]
    }

    fn fail(&mut self, message: &str, now: Instant) -> Vec<Outcome> {
        let selection = self.selection();
        if self.intent.is_none() && self.active.is_none() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.active.take().is_some() {
            out.push(Outcome::Close);
            out.extend(self.request_leave(now));
        }
        self.intent = None;
        self.deadline = None;
        if let Some(selection) = selection {
            out.push(Outcome::Failed {
                selection,
                message: message.into(),
            });
        }
        out
    }

    fn signal(&mut self, kind: &'static str, data: Value) -> Signal {
        self.sequence = self.sequence.wrapping_add(1);
        let request_id = Some(format!("native-stream-{}", self.sequence));
        if let Some(active) = &mut self.active {
            if let Some(id) = &request_id {
                active.requests.insert(id.clone());
            }
        }
        Signal {
            kind,
            data,
            request_id,
        }
    }

    fn track_request(&mut self, request_id: Option<&str>) {
        if let (Some(active), Some(id)) = (&mut self.active, request_id) {
            active.requests.insert(id.to_owned());
        }
    }
}

/// Every field that identifies a publication. `viewer_count` and timestamps
/// may change, but these four must match exactly.
pub fn same_identity(left: &Publication, right: &Publication) -> bool {
    left.server_id == right.server_id
        && left.channel_id == right.channel_id
        && left.publisher_user_id == right.publisher_user_id
        && left.publisher_connection_id == right.publisher_connection_id
}

/// An empty id means the older backend could not attribute the event; a
/// non-empty id must exactly match this attempt.
fn request_matches(request_id: &str, watch_request: &str) -> bool {
    request_id.is_empty() || request_id == watch_request
}

fn sdp_ufrags(sdp: &str) -> Vec<String> {
    let mut ufrags = Vec::new();
    for line in sdp.replace("\r\n", "\n").lines() {
        if let Some(ufrag) = line.strip_prefix("a=ice-ufrag:") {
            if !ufrags.iter().any(|value| value == ufrag) {
                ufrags.push(ufrag.to_owned());
            }
        }
    }
    ufrags
}

fn candidate_ufrag(candidate: &str) -> Option<&str> {
    let mut tokens = candidate.split_ascii_whitespace().skip(8);
    while let (Some(key), Some(value)) = (tokens.next(), tokens.next()) {
        if key == "ufrag" {
            return Some(value);
        }
    }
    None
}

pub fn request_number(request_id: &str) -> Option<u64> {
    request_id
        .strip_prefix("native-stream-")
        .and_then(|number| number.parse::<u64>().ok())
}

/// Reasons that end the viewer for good instead of one bounded recovery.
pub fn final_close(reason: &str) -> bool {
    let reason = reason.to_lowercase();
    ["permission", "access", "stopped", "replaced", "e2ee"]
        .iter()
        .any(|needle| reason.contains(needle))
}

#[cfg(test)]
mod tests;
