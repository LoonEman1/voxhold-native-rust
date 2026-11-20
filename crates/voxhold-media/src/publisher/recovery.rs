//! Replaying one confirmed publication after a transport loss.
//!
//! The request is always the configuration that really reached the air; a
//! picker draft, a preflight or a stopped attempt never becomes a recovery
//! intent. The ordinary preparation path is reused, so a missing permission,
//! slot or source fails with its reason and never sends `stream.start`.

use super::*;
use crate::viewer::LEAVE_TIMEOUT;
use std::time::Instant;

impl Publisher {
    /// The exact confirmed request of the publication that was on air, for a
    /// recovery after a transport loss. A Stop/Leave/revocation clears or has
    /// already cleared `confirmed`, so it can never be mistaken for intent.
    pub fn recovery_request(&self) -> Option<PublishRequest> {
        if !matches!(self.phase, Phase::Live | Phase::Publishing) {
            return None;
        }
        self.confirmed.as_ref().map(Confirmed::request)
    }

    /// Start a recovered attempt with the confirmed configuration. This is the
    /// ordinary capture path: the engine is prepared off the session loop, and
    /// `stream.start` is sent exactly once after the capture really opened. A
    /// missing permission, slot or source fails the attempt with its reason
    /// instead of publishing anything.
    pub fn recover(
        &mut self,
        request: PublishRequest,
        viewer_active: bool,
        now: Instant,
    ) -> Vec<Outcome> {
        // One captured intent starts once. A second join confirmation cannot
        // begin another attempt on the same publisher.
        if self.phase != Phase::Idle {
            return Vec::new();
        }
        // The attempt is visible to the state machine before the gate check,
        // so a refused recovery reports a terminal error instead of looking
        // like an untouched idle publisher.
        self.phase = Phase::Publishing;
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
        self.attempt = self.attempt.wrapping_add(1);
        self.picker = None;
        self.request = Some(request);
        if viewer_active {
            self.viewer_wait = Some(now + LEAVE_TIMEOUT);
            self.status = "Завершаем просмотр…".into();
            return vec![Outcome::LeaveViewer, Outcome::State(self.sharing())];
        }
        self.start(now)
    }
}
