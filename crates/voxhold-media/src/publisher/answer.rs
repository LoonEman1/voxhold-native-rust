//! Answers of the current attempt and their server confirmations.
//!
//! The first answer completes the initial negotiation. The backend may send a
//! second `stream.webrtc_offer` for an ICE restart on the same publication; its
//! answer must be sent too, but its confirmation must leave the live card, the
//! attempt and the confirmed settings exactly where they are. A newer offer
//! supersedes an answer that is still unconfirmed, so a late confirmation of
//! the older SDP cannot decide the current negotiation.
//!
//! The pinned backend has no `accepted:false`: a refused answer arrives as
//! `error` tagged with the request id of that `stream.webrtc_answer`. Only the
//! ids of the last superseded answers are remembered (a bounded history), so a
//! refusal of an answer this attempt has already replaced is ignored while the
//! current answer's error still fails the publication.

use serde_json::{Value, json};
use std::time::Instant;

use super::{Outcome, Phase, Publisher};

/// How many replaced answer ids are remembered. Only the immediate
/// renegotiation matters: a newer offer supersedes the previous answer, so two
/// slots cover an offer/answer that itself replaced another one.
const SUPERSEDED_ANSWERS: usize = 2;

/// The answer whose `stream.webrtc_answered` is still awaited. The backend
/// echoes the request id of `stream.webrtc_answer`, so only this tag may
/// confirm the negotiation.
#[derive(Default)]
pub(super) struct PendingAnswer {
    request: Option<String>,
    /// The answer belongs to a renegotiation after the initial one was
    /// confirmed. Its confirmation changes nothing on air.
    restart: bool,
    /// Request ids of answers that a newer offer/answer replaced before they
    /// were decided. Oldest first, bounded by [`SUPERSEDED_ANSWERS`].
    superseded: Vec<String>,
}

impl PendingAnswer {
    /// Start a new answer, remembering the one it replaces.
    fn begin(&mut self, request: Option<String>, restart: bool) {
        self.supersede();
        self.request = request;
        self.restart = restart;
    }

    /// A newer offer or answer replaces the unconfirmed one. The replaced id
    /// stays in the bounded history so its refusal cannot decide the attempt.
    fn supersede(&mut self) {
        if let Some(request) = self.request.take()
            && !self.superseded.contains(&request)
        {
            if self.superseded.len() == SUPERSEDED_ANSWERS {
                self.superseded.remove(0);
            }
            self.superseded.push(request);
        }
        self.restart = false;
    }

    /// The current answer was decided; the superseded history belongs to the
    /// same attempt and stays until the attempt itself ends.
    fn resolve(&mut self) {
        self.request = None;
        self.restart = false;
    }

    /// A new attempt never inherits the answers or their history.
    fn clear(&mut self) {
        self.resolve();
        self.superseded.clear();
    }

    /// The backend rejected this answer, and a newer offer/answer has already
    /// taken over the negotiation.
    pub(super) fn is_superseded(&self, request_id: &str) -> bool {
        !request_id.is_empty() && self.superseded.iter().any(|id| id == request_id)
    }
}

impl Publisher {
    /// The engine answered the current offer. Exactly one `stream.webrtc_answer`
    /// is sent and its request id becomes the only confirmation still awaited.
    pub(super) fn on_answer(&mut self, sdp: String) -> Vec<Outcome> {
        if !self.active() || sdp.is_empty() {
            return Vec::new();
        }
        // Once the initial negotiation is confirmed, a new offer is an ICE
        // restart on the same publication, even if the card is not live yet.
        let restart = self.answered || self.phase == Phase::Live;
        let signal = self.signal("stream.webrtc_answer", json!({"sdp": sdp}));
        self.pending_answer
            .begin(signal.request_id.clone(), restart);
        vec![Outcome::Send(signal)]
    }

    /// The start or end of an attempt drops the answer that is still
    /// unconfirmed (and its superseded history) before a new one decides.
    pub(super) fn clear_answer(&mut self) {
        self.pending_answer.clear();
    }

    /// A newer offer replaces the answer that is still unconfirmed. The
    /// replaced id is remembered so its refusal cannot decide this attempt.
    pub(super) fn supersede_answer(&mut self) {
        self.pending_answer.supersede();
    }

    /// `stream.webrtc_answered`. A tagged confirmation must name the answer
    /// this attempt still awaits. An untagged one can only belong to it,
    /// because at most one answer is ever pending.
    pub(super) fn on_answer_confirmed(
        &mut self,
        request_id: &str,
        data: &Value,
        now: Instant,
    ) -> Vec<Outcome> {
        if !self.active() {
            return Vec::new();
        }
        let Some(pending) = self.pending_answer.request.clone() else {
            return Vec::new();
        };
        if !request_id.is_empty() && request_id != pending {
            return Vec::new();
        }
        let restart = self.pending_answer.restart;
        self.pending_answer.resolve();
        if !data["accepted"].as_bool().unwrap_or(false) {
            // A refusal is real even for a restart: the backend did not apply
            // the new SDP, so the publication cannot continue.
            return self.fail("Сервер отклонил ответ трансляции.", now);
        }
        if restart {
            // The ICE restart keeps the same publication: same attempt, same
            // confirmed settings, no `stream.start`, `stream.stop` or engine
            // outcome.
            return Vec::new();
        }
        self.answered = true;
        if self.phase == Phase::Live {
            // The first frame and the transport can win the race against the
            // server's confirmation. The publication is already on air, so a
            // late initial confirmation must not put the card back to
            // "Подключаемся…" (the same rule as a restart confirmation).
            return Vec::new();
        }
        self.status = "Подключаемся…".into();
        vec![Outcome::State(self.sharing())]
    }
}
