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
use crate::publisher::CodecSnapshot;
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
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
/// Completed ICE generations are remembered long enough to reject stragglers,
/// but the set stays bounded across a long viewing session.
const RETIRED_UFRAG_TTL: Duration = Duration::from_secs(600);
const RETIRED_UFRAG_CAPACITY: usize = 64;
/// Keyframe request ids of the current attempt remembered for error routing.
/// The decoder budget permits two per stall cycle and a long session may run
/// several cycles, so the set is bounded instead of growing forever.
const KEYFRAME_REQUEST_CAPACITY: usize = 8;

mod stats;
use stats::StatsWatch;

/// Decoding capability of this build: the catalogue check before
/// `stream.watch` and the offer profile check after it.
mod codec;

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
    /// Keyframe requests of this attempt. Their error reply rejects only that
    /// bounded recovery and must not end the view.
    keyframes: VecDeque<String>,
    publication: Publication,
    connected: bool,
    recovering: bool,
    received_signal: bool,
    remote_ufrags: Vec<String>,
    /// Inbound RTP/video counters of this engine. A stale report from an older
    /// engine of the same selection is rejected by `attempt`.
    stats: StatsWatch,
}

/// One decision produced by the state machine. The runtime executes them in
/// order and maps them to WebSocket signals, media commands or UI updates.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Create an engine with this attempt's watch request.
    Start {
        selection: u64,
        /// Viewer attempt the engine belongs to. Statistics of an engine of an
        /// older attempt are ignored even when the selection id is unchanged.
        attempt: u64,
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
    retired_ufrags: VecDeque<(String, Instant)>,
    /// Receiver-side capability matrix of this build. `None` reads the
    /// process-wide probe; a test injects a matrix so a missing decoder is
    /// deterministic on any host.
    snapshot: Option<CodecSnapshot>,
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
            retired_ufrags: VecDeque::new(),
            snapshot: None,
        }
    }

    pub fn catalog(&self) -> &StreamCatalog {
        &self.catalog
    }

    /// A `stream.snapshot` was parsed and accepted for this connection. A
    /// malformed snapshot leaves the previous value untouched, so callers that
    /// gate recovery on a fresh list must ask this before acting.
    pub fn snapshot_accepted(&self) -> bool {
        self.snapshot_ready
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

    /// A `stream.leave` is still waiting for its acknowledgement.
    pub fn leaving(&self) -> bool {
        self.pending_leave.is_some()
    }

    /// `voice.joined`/transport state. On a reconnect the old engine, catalog
    /// and leave request are gone; the selected publication stays and is
    /// watched again only after both a fresh snapshot and the join confirm.
    pub fn set_joined(&mut self, joined: bool, now: Instant) -> Vec<Outcome> {
        if !joined {
            // A snapshot may arrive before `ready` on the same socket. Until
            // the first `voice.joined` there is no transport to end, so a
            // repeated pre-join `false` must not discard that snapshot.
            if !self.joined {
                return Vec::new();
            }
            return self.transport_ended(now);
        }
        if self.joined == joined {
            return Vec::new();
        }
        self.joined = joined;
        if self.snapshot_ready {
            return self.start_if_ready(now);
        }
        Vec::new()
    }

    /// End of one WebSocket session, even before `voice.joined`. The fresh
    /// snapshot is no longer authoritative and the panel must show recovery
    /// for the retained selection while the last decoded frame stays visible.
    pub fn transport_ended(&mut self, now: Instant) -> Vec<Outcome> {
        self.joined = false;
        self.pending_leave = None;
        self.deadline = None;
        self.snapshot_ready = false;
        self.retired_ufrags.clear();
        self.catalog.replace(std::iter::empty());
        let mut out = Vec::new();
        if let Some(active) = self.active.take() {
            self.retire(&active, now);
            out.push(Outcome::Close);
        }
        if let Some(intent) = &mut self.intent {
            // The rejoin is a new viewing attempt: its engine must not inherit
            // statistics reports of the peer that was just closed, and the
            // automatic-reconnect budget belongs to the previous socket
            // session. Without the reset the first stall of the fresh session
            // would fail with no re-watch at all.
            self.attempt = self.attempt.wrapping_add(1);
            intent.attempt = self.attempt;
            self.retries = 0;
            out.push(Outcome::Status {
                selection: intent.selection,
                text: "Восстанавливаем трансляцию…".into(),
            });
            out.push(Outcome::Connected {
                selection: intent.selection,
                connected: false,
            });
        }
        out
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
    /// must not end B after the user has already selected it. A close newer
    /// than the current view still closes the picture that is actually
    /// running, because an intermediate watch may have been displaced.
    pub fn leave_selection(&mut self, selection: u64, now: Instant) -> Vec<Outcome> {
        let current = self
            .selection()
            .or_else(|| self.active.as_ref().map(|active| active.selection));
        if current.is_some_and(|current| selection < current) {
            return Vec::new();
        }
        self.leave_with(Some(selection), now)
    }

    /// The user closed the player. Voice stays joined.
    pub fn leave(&mut self, now: Instant) -> Vec<Outcome> {
        self.leave_with(self.selection(), now)
    }

    fn leave_with(&mut self, clear_selection: Option<u64>, now: Instant) -> Vec<Outcome> {
        if self.intent.is_none() && self.active.is_none() {
            return Vec::new();
        }
        self.intent = None;
        let mut out = self.close_active(now);
        if let Some(selection) = clear_selection {
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
            "stream.webrtc_offer" | "stream.p2p_offer" => {
                self.on_offer(kind, request_id, data, now)
            }
            "stream.ice_candidate" | "stream.p2p_ice_candidate" => {
                self.on_ice_event(kind, request_id, data, now)
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
                if request_id.is_empty() {
                    return Vec::new();
                }
                if active.keyframes.iter().any(|id| id == request_id) {
                    // The server rejected the keyframe request, not the view.
                    // The bounded keyframe/ICE/rewatch cascade owns this
                    // failure; ending the viewer here would turn a repairable
                    // decoder stall into a terminal error.
                    return Vec::new();
                }
                if !active.requests.contains(request_id) {
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
        if explicit_watch_tag(request_id, data) != Some(active.watch_request.as_str())
            || !watch_tags_match(request_id, data, &active.watch_request)
        {
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
            // The ACK does not attribute later untagged pushes. Leave
            // `received_signal` false until an offer/ICE/frame belongs to this
            // attempt, so the first ambiguous close after a switch stays
            // ignored.
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

    fn on_offer(
        &mut self,
        kind: &str,
        request_id: &str,
        data: &Value,
        now: Instant,
    ) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        if explicit_watch_tag(request_id, data).is_some()
            && !watch_tags_match(request_id, data, &active.watch_request)
        {
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
        // The offer names the rendition profile. A stream this build cannot
        // decode is released with the ordinary `stream.leave` instead of
        // hanging on a black picture until the start deadline. The codec the
        // catalogue promised decides which payload really carries the stream;
        // audio m-lines belong to the separate voice path.
        if let Some(message) = self.offer_refusal(sdp, &active.publication) {
            return self.fail(&message, now);
        }
        let ufrags = sdp_ufrags(sdp);
        if !ufrags.is_empty() && ufrags.iter().all(|ufrag| self.retired(now, ufrag)) {
            return Vec::new();
        }
        let previous = self
            .active
            .as_mut()
            .map(|active| std::mem::take(&mut active.remote_ufrags))
            .unwrap_or_default();
        for ufrag in previous {
            if !ufrags.contains(&ufrag) {
                self.retire_ufrag(ufrag, now);
            }
        }
        if let Some(active) = &mut self.active {
            active.remote_ufrags = ufrags;
            active.received_signal = true;
            self.stale_close_until = None;
            self.stale_close_available = false;
        }
        vec![Outcome::Offer(sdp.to_owned())]
    }

    fn on_ice_event(
        &mut self,
        kind: &str,
        request_id: &str,
        data: &Value,
        now: Instant,
    ) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        if explicit_watch_tag(request_id, data).is_some()
            && !watch_tags_match(request_id, data, &active.watch_request)
        {
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
            if self.retired(now, ufrag) {
                return Vec::new();
            }
            // An unknown next generation is not stale: Pion may emit the first
            // ICE candidate after SetLocalDescription but before the restarted
            // offer reaches this client. The engine keeps it in its bounded
            // pending buffer until an offer with that ufrag is accepted.
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
        // A tag that identifies the watch is authoritative. Publisher and
        // sender ids are not watch ids: a repeated P2P view of the same
        // publisher has the same `from_connection_id` but a new attempt.
        if let Some(tag) = explicit_watch_tag(request_id, data) {
            if tag != active.watch_request
                || !watch_tags_match(request_id, data, &active.watch_request)
            {
                return Vec::new();
            }
        } else if !active.received_signal
            && self.stale_close_available
            && self
                .stale_close_until
                .is_some_and(|deadline| now < deadline)
        {
            // The pinned backend does not tag pushed closes. One untagged close
            // immediately after a switch is ambiguous; keep the new attempt and
            // document that a strict attribution needs the backend watch id.
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
        out.extend(self.restart(now));
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
        // A real decoded frame is the only proof that the picture is moving. A
        // cached repaint never reaches this method. A decoded frame clears the
        // idle/keyframe counters, but only a full healthy stats window clears
        // the reconnect budget: one frame after a reconnect must not hide a
        // stream that freezes again immediately.
        active.stats.on_decoded();
        if active.connected && !active.recovering {
            return Vec::new();
        }
        active.connected = true;
        active.recovering = false;
        active.received_signal = true;
        self.deadline = None;
        self.stale_close_until = None;
        self.stale_close_available = false;
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
    /// bounded recovery and keep the picture until the deadline. This is the
    /// only transport-loss path: RTP silence on a connected peer is a static
    /// screen (see `stats`), not a disconnect. One ICE failure often surfaces
    /// as `disconnected` followed by `failed`; while its recovery deadline is
    /// armed the duplicate state must not spend a second retry.
    pub fn on_engine_state(&mut self, state: &str, now: Instant) -> Vec<Outcome> {
        if state != "failed" && state != "disconnected" {
            return Vec::new();
        }
        let already_recovering = self
            .active
            .as_ref()
            .is_some_and(|active| active.recovering && self.deadline.is_some());
        if already_recovering {
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
                    out.extend(self.restart(now));
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
        // Frozen-picture detection runs on the browser's two-second video-stats
        // cadence, not on this 25 ms session tick.
        out.extend(self.poll_stats(now));
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
        // The catalogue already names the codec. A build without its decoder
        // refuses before `stream.watch`: no subscription and no engine.
        if let Some(message) = self.catalogue_refusal(&publication) {
            self.intent = None;
            return vec![Outcome::Failed {
                selection: intent.selection,
                message,
            }];
        }
        self.start(intent, publication, now)
    }

    fn start(&mut self, intent: Intent, publication: Publication, now: Instant) -> Vec<Outcome> {
        let selection = intent.selection;
        let muted = intent.muted;
        let mut request = self.signal(
            "stream.watch",
            json!({
                "server_id": publication.server_id,
                "channel_id": publication.channel_id,
                "publisher_connection_id": publication.publisher_connection_id,
                "supported_dynamic_ranges": ["sdr"],
            }),
        );
        let watch_request = request.request_id.clone().unwrap_or_default();
        // Give a compatible backend the attempt id it must echo on every
        // offer/ICE/close. The pinned backend ignores the field, so the client
        // keeps the compatibility heuristics documented in docs/status.md.
        request.data["watch_id"] = json!(watch_request);
        self.active = Some(Active {
            selection,
            attempt: intent.attempt,
            watch_request: watch_request.clone(),
            requests: HashSet::from([watch_request]),
            keyframes: VecDeque::new(),
            publication: publication.clone(),
            connected: false,
            recovering: false,
            received_signal: false,
            remote_ufrags: Vec::new(),
            stats: StatsWatch::new(now),
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
                attempt: intent.attempt,
                publication,
                muted,
                request,
            },
        ]
    }

    /// Close the active attempt for an automatic recovery and give the next
    /// watch a fresh attempt id. Statistics of the engine that is being closed
    /// must not be credited to its replacement, especially when a re-watch
    /// keeps the same publication and selection.
    fn restart(&mut self, now: Instant) -> Vec<Outcome> {
        if let Some(intent) = &mut self.intent {
            self.attempt = self.attempt.wrapping_add(1);
            intent.attempt = self.attempt;
        }
        self.close_active(now)
    }

    fn close_active(&mut self, now: Instant) -> Vec<Outcome> {
        let mut out = Vec::new();
        if let Some(active) = self.active.take() {
            self.retire(&active, now);
            out.push(Outcome::Close);
            self.stale_close_until = Some(now + STALE_CLOSE_GRACE);
            self.stale_close_available = true;
        }
        self.deadline = None;
        out.extend(self.request_leave(now));
        out
    }

    fn retire(&mut self, active: &Active, now: Instant) {
        for ufrag in &active.remote_ufrags {
            self.retire_ufrag(ufrag.clone(), now);
        }
    }

    fn retire_ufrag(&mut self, ufrag: String, now: Instant) {
        self.retired_ufrags.retain(|(value, _)| value != &ufrag);
        self.retired_ufrags.push_back((ufrag, now));
        while self.retired_ufrags.len() > RETIRED_UFRAG_CAPACITY {
            self.retired_ufrags.pop_front();
        }
    }

    fn retired(&self, now: Instant, ufrag: &str) -> bool {
        self.retired_ufrags
            .iter()
            .any(|(value, at)| value == ufrag && now.duration_since(*at) < RETIRED_UFRAG_TTL)
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

    /// Remember a keyframe request so an `error` tagged with it is routed to
    /// the bounded picture recovery instead of ending the viewer.
    fn track_keyframe(&mut self, request_id: Option<&str>) {
        if let (Some(active), Some(id)) = (&mut self.active, request_id) {
            active.keyframes.push_back(id.to_owned());
            while active.keyframes.len() > KEYFRAME_REQUEST_CAPACITY {
                active.keyframes.pop_front();
            }
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

/// A push event may carry an explicit watch id either inside its data or as
/// the top-level request id. Publisher connection ids are deliberately not
/// accepted: they identify a publisher, not one viewing attempt.
fn explicit_watch_tag<'a>(request_id: &'a str, data: &'a Value) -> Option<&'a str> {
    data.get("watch_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            data.get("request_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .or_else(|| (!request_id.is_empty()).then_some(request_id))
}

/// Every tag that is present must name the current attempt. A conflicting old
/// top-level id cannot be hidden by an empty `data`.
fn watch_tags_match(request_id: &str, data: &Value, watch_request: &str) -> bool {
    let tags = [
        data.get("watch_id").and_then(Value::as_str),
        data.get("request_id").and_then(Value::as_str),
        (!request_id.is_empty()).then_some(request_id),
    ];
    tags.into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .all(|tag| tag == watch_request)
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
