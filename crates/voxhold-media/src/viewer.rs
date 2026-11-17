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
use std::time::{Duration, Instant};

/// Go waits this long for `stream.left` before it abandons the switch.
pub const LEAVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Go re-arms this deadline on every viewing attempt and recovery request.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Two recovery requests, then the third failure ends the viewer.
pub const MAX_RETRIES: u8 = 2;

#[derive(Clone, Debug)]
struct Intent {
    attempt: u64,
    publication: Publication,
    muted: bool,
}

#[derive(Clone, Debug)]
struct Active {
    attempt: u64,
    watch_request: String,
    request_start: u64,
    publication: Publication,
    connected: bool,
}

/// One decision produced by the state machine. The runtime executes them in
/// order and maps them to WebSocket signals, media commands or UI updates.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Create an engine with this attempt's watch request.
    Start {
        publication: Publication,
        muted: bool,
        request: Signal,
    },
    /// Drop the engine, decoder and frame buffers of the previous attempt.
    Close,
    Send(Signal),
    Offer(String),
    Ice(Candidate),
    Mute(bool),
    Status(String),
    Clear,
    Failed(String),
    /// The publication list changed and the panel should read it again.
    Catalog,
}

/// Identity of the active attempt: the publication plus its attempt number.
#[derive(Clone, Debug)]
pub struct Watching {
    pub attempt: u64,
    pub publication: Publication,
    pub connected: bool,
}

pub struct Viewer {
    server: i64,
    channel: i64,
    catalog: StreamCatalog,
    joined: bool,
    intent: Option<Intent>,
    active: Option<Active>,
    pending_leave: Option<(String, Instant)>,
    deadline: Option<Instant>,
    retries: u8,
    attempt: u64,
    sequence: u64,
}

impl Viewer {
    pub fn new(server: i64, channel: i64) -> Self {
        Self {
            server,
            channel,
            catalog: StreamCatalog::new(server, channel),
            joined: false,
            intent: None,
            active: None,
            pending_leave: None,
            deadline: None,
            retries: 0,
            attempt: 0,
            sequence: 0,
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

    pub fn watching(&self) -> Option<Watching> {
        self.active.as_ref().map(|active| Watching {
            attempt: active.attempt,
            publication: active.publication.clone(),
            connected: active.connected,
        })
    }

    pub fn muted(&self) -> bool {
        self.intent.as_ref().is_some_and(|intent| intent.muted)
    }

    /// `voice.joined`/transport state. On a reconnect the old engine and its
    /// leave request are gone; the selected publication stays and is watched
    /// again only after the fresh join is confirmed.
    pub fn set_joined(&mut self, joined: bool, now: Instant) -> Vec<Outcome> {
        if self.joined == joined {
            return Vec::new();
        }
        self.joined = joined;
        if !joined {
            self.pending_leave = None;
            self.deadline = None;
            let mut out = Vec::new();
            if self.active.take().is_some() {
                out.push(Outcome::Close);
            }
            if self.intent.is_some() {
                out.push(Outcome::Status("Восстанавливаем трансляцию…".into()));
            }
            return out;
        }
        if self.intent.is_some() && self.pending_leave.is_none() && self.active.is_none() {
            return self.start(now);
        }
        Vec::new()
    }

    /// A user picked a publication. The previous attempt is closed and left,
    /// and the new watch starts only after `stream.left` or the timeout.
    pub fn select(&mut self, publication: &Publication, now: Instant) -> Vec<Outcome> {
        if !self.catalog.visible(publication) {
            return Vec::new();
        }
        let Some(current) = self
            .catalog
            .get(&publication.publisher_connection_id)
            .cloned()
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if self.intent.is_some() {
            out.extend(self.close_active(now));
        }
        self.attempt = self.attempt.wrapping_add(1);
        self.intent = Some(Intent {
            attempt: self.attempt,
            publication: current,
            muted: false,
        });
        self.retries = 0;
        if self.joined && self.pending_leave.is_none() && self.active.is_none() {
            out.extend(self.start(now));
        }
        out
    }

    /// The user closed the player. Voice stays joined.
    pub fn leave(&mut self, now: Instant) -> Vec<Outcome> {
        if self.intent.is_none() && self.active.is_none() {
            return Vec::new();
        }
        self.intent = None;
        let mut out = self.close_active(now);
        out.push(Outcome::Clear);
        out
    }

    pub fn set_muted(&mut self, muted: bool) -> Vec<Outcome> {
        if let Some(intent) = &mut self.intent {
            intent.muted = muted;
        }
        if self.active.is_some() {
            vec![Outcome::Mute(muted)]
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
            "stream.snapshot" => {
                let streams =
                    match serde_json::from_value::<Vec<Publication>>(data["streams"].clone()) {
                        Ok(streams) => streams,
                        Err(_) => return Vec::new(),
                    };
                let changed = self.catalog.replace(streams);
                if !self.joined {
                    if let Some(intent) = &mut self.intent {
                        if let Some(next) = self.catalog.retarget(
                            intent.publication.publisher_user_id,
                            &intent.publication.publisher_connection_id,
                        ) {
                            intent.publication = next;
                        }
                    }
                }
                if changed {
                    vec![Outcome::Catalog]
                } else {
                    Vec::new()
                }
            }
            "stream.started" | "stream.updated" => {
                let Ok(publication) = serde_json::from_value::<Publication>(data.clone()) else {
                    return Vec::new();
                };
                if self.catalog.upsert(publication) {
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
            "stream.watching" => {
                let Some(active) = &self.active else {
                    return Vec::new();
                };
                if request_id != active.watch_request {
                    return Vec::new();
                }
                let Ok(publication) = serde_json::from_value::<Publication>(data["stream"].clone())
                else {
                    return self.fail("Некорректный ответ сервера", now);
                };
                if publication.publisher_connection_id != active.publication.publisher_connection_id
                {
                    return self.fail("Сервер выбрал другую трансляцию", now);
                }
                if let Some(active) = &mut self.active {
                    active.publication = publication.clone();
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
            "stream.webrtc_offer" | "stream.p2p_offer" => {
                let Some(active) = &self.active else {
                    return Vec::new();
                };
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
                if accepted {
                    vec![Outcome::Offer(sdp.to_owned())]
                } else {
                    Vec::new()
                }
            }
            "stream.ice_candidate" | "stream.p2p_ice_candidate" => {
                let Some(active) = &self.active else {
                    return Vec::new();
                };
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
                vec![Outcome::Ice(Candidate {
                    candidate: data["candidate"].as_str().unwrap_or_default().into(),
                    sdp_mid: data["sdp_mid"].as_str().map(str::to_owned),
                    sdp_mline_index: data["sdp_mline_index"]
                        .as_u64()
                        .and_then(|value| u16::try_from(value).ok()),
                    username_fragment: data["username_fragment"].as_str().map(str::to_owned),
                })]
            }
            "stream.left" => {
                if self
                    .pending_leave
                    .as_ref()
                    .is_some_and(|(id, _)| id == request_id)
                {
                    self.pending_leave = None;
                    if self.joined && self.intent.is_some() && self.active.is_none() {
                        return self.start(now);
                    }
                    return Vec::new();
                }
                if self.active.is_some() && request_id.is_empty() {
                    return self.fail("Просмотр трансляции завершён сервером", now);
                }
                Vec::new()
            }
            "stream.webrtc_closed" => {
                if self.active.is_none() {
                    return Vec::new();
                }
                let reason = data["reason"].as_str().unwrap_or_default();
                if final_close(reason) || self.retries >= MAX_RETRIES {
                    return self.fail("Трансляция завершена", now);
                }
                self.retries += 1;
                let mut out = vec![Outcome::Status("Восстанавливаем трансляцию…".into())];
                out.extend(self.close_active(now));
                out
            }
            "error" => {
                let Some(active) = &self.active else {
                    return Vec::new();
                };
                let Some(number) = request_number(request_id) else {
                    return Vec::new();
                };
                if number >= active.request_start {
                    return self.fail("Сервер отклонил просмотр трансляции", now);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
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

    /// First decoded frame of this attempt confirms the picture.
    pub fn on_frame(&mut self) -> Vec<Outcome> {
        let Some(active) = &mut self.active else {
            return Vec::new();
        };
        if active.connected {
            return Vec::new();
        }
        active.connected = true;
        self.deadline = None;
        vec![Outcome::Status("Подключено".into())]
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
        self.deadline = Some(now + START_TIMEOUT);
        let mut out = vec![Outcome::Status("Восстанавливаем трансляцию…".into())];
        if self.retries < MAX_RETRIES {
            self.retries += 1;
            let p2p = active.publication.mode == "p2p";
            let data = if p2p {
                json!({"target_connection_id":active.publication.publisher_connection_id})
            } else {
                json!({"server_id":active.publication.server_id,"channel_id":active.publication.channel_id,"action":"ice_restart"})
            };
            out.push(Outcome::Send(self.signal(
                if p2p {
                    "stream.p2p_restart"
                } else {
                    "stream.recovery_request"
                },
                data,
            )));
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
                    out.push(Outcome::Status("Восстанавливаем трансляцию…".into()));
                }
            }
        }
        out
    }

    fn start(&mut self, now: Instant) -> Vec<Outcome> {
        let Some(intent) = self.intent.clone() else {
            return Vec::new();
        };
        let request = self.signal(
            "stream.watch",
            json!({
                "server_id": intent.publication.server_id,
                "channel_id": intent.publication.channel_id,
                "publisher_connection_id": intent.publication.publisher_connection_id,
                "supported_dynamic_ranges": ["sdr"],
            }),
        );
        let request_start = request_number(request.request_id.as_deref().unwrap_or_default())
            .unwrap_or(self.sequence);
        self.active = Some(Active {
            attempt: intent.attempt,
            watch_request: request.request_id.clone().unwrap_or_default(),
            request_start,
            publication: intent.publication.clone(),
            connected: false,
        });
        self.deadline = Some(now + START_TIMEOUT);
        vec![
            Outcome::Status("Подключение…".into()),
            Outcome::Start {
                publication: intent.publication,
                muted: intent.muted,
                request,
            },
        ]
    }

    fn close_active(&mut self, now: Instant) -> Vec<Outcome> {
        let mut out = Vec::new();
        if self.active.take().is_some() {
            out.push(Outcome::Close);
        }
        self.deadline = None;
        out.extend(self.request_leave(now));
        out
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
        out.push(Outcome::Failed(message.into()));
        out
    }

    fn signal(&mut self, kind: &'static str, data: Value) -> Signal {
        self.sequence = self.sequence.wrapping_add(1);
        Signal {
            kind,
            data,
            request_id: Some(format!("native-stream-{}", self.sequence)),
        }
    }
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
