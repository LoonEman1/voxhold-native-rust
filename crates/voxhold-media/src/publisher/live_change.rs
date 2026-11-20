//! Live bitrate changes, the preflight status of a restart and the one-shot
//! rollback to the previous request.
//!
//! These decisions stay out of `publisher.rs` so the state machine and its
//! rollback invariants are reviewed together while the file stays inside the
//! size budget. The fields are private to the publishing attempt; this child
//! module only decides when they move.

use serde_json::json;
use std::time::Instant;

use super::{Outcome, Phase, PublishRequest, Publisher, QualityChange, STOP_TIMEOUT};

/// The status of a live bitrate change that the sender really received.
const BITRATE_APPLIED_STATUS: &str = "Битрейт обновлён в эфире.";
/// The bounded command queue of the worker was momentarily full. The value is
/// retried on the next tick; nothing failed and the previous limit still
/// applies until then.
const BITRATE_QUEUED_STATUS: &str = "Применяем новый битрейт…";

impl Publisher {
    /// The request of the current attempt, as the live sender received it.
    pub fn request(&self) -> Option<&PublishRequest> {
        self.request.as_ref()
    }

    /// Apply a bitrate-only change to the live sender.
    ///
    /// The pinned backend has no runtime reconfigure, and the pinned engine
    /// re-applies the limit through `RtpSender::set_parameters` and
    /// `PeerConnection::set_bitrate` without a new offer. The attempt, the
    /// capture, the phase and the viewer stay exactly where they are. Any
    /// other difference is refused here: the runtime must preflight it and
    /// call [`Publisher::restart`] after the confirmed stop.
    pub fn apply_quality(&mut self, request: PublishRequest, _now: Instant) -> Vec<Outcome> {
        if self.phase != Phase::Live {
            return Vec::new();
        }
        let Some(current) = self.request.as_ref() else {
            return Vec::new();
        };
        if current.change_to(&request) != QualityChange::LiveBitrate {
            return Vec::new();
        }
        let mut request = request;
        request.quality = request.quality.clamped();
        let quality = request.quality;
        self.request = Some(request);
        self.error = None;
        self.status = BITRATE_APPLIED_STATUS.into();
        vec![
            Outcome::SetBitrate {
                attempt: self.attempt,
                quality,
            },
            Outcome::State(self.sharing()),
        ]
    }

    /// The bounded command queue of the live worker was full, so the newest
    /// quality is still pending. This is not a failure: no `stream.stop`, no
    /// `Phase::Failed`, and the previous limit keeps applying until the retry.
    pub fn bitrate_queued(&mut self) -> Vec<Outcome> {
        if self.phase != Phase::Live || self.status == BITRATE_QUEUED_STATUS {
            return Vec::new();
        }
        self.status = BITRATE_QUEUED_STATUS.into();
        self.error = None;
        vec![Outcome::State(self.sharing())]
    }

    /// The retried limit reached the worker of the live attempt.
    pub fn bitrate_applied(&mut self) -> Vec<Outcome> {
        if self.phase != Phase::Live || self.status == BITRATE_APPLIED_STATUS {
            return Vec::new();
        }
        self.status = BITRATE_APPLIED_STATUS.into();
        self.error = None;
        vec![Outcome::State(self.sharing())]
    }

    /// A restart preflight started. The live stream keeps running and no
    /// `stream.stop` is sent until the preflight confirmed the new capture.
    pub fn preflight_started(&mut self) -> Vec<Outcome> {
        if self.phase != Phase::Live {
            return Vec::new();
        }
        self.preflighting = true;
        self.restarting = false;
        self.error = None;
        self.status = "Проверяем новую настройку…".into();
        vec![Outcome::State(self.sharing())]
    }

    /// The preflight of the new capture failed: the live stream is untouched
    /// and the reason is shown without leaving `Live`.
    pub fn preflight_failed(&mut self, message: &str) -> Vec<Outcome> {
        if self.phase != Phase::Live {
            return Vec::new();
        }
        self.preflighting = false;
        self.restarting = false;
        self.status = message.to_owned();
        self.error = Some(message.to_owned());
        vec![Outcome::State(self.sharing())]
    }

    /// A preflight of the live attempt was cancelled. `message` reports a
    /// session or device loss; `None` is the user closing the dialog and the
    /// card returns to the plain on-air status.
    pub fn preflight_cancelled(&mut self, message: Option<&str>) -> Vec<Outcome> {
        if self.phase != Phase::Live {
            return Vec::new();
        }
        self.preflighting = false;
        self.restarting = false;
        match message {
            Some(message) => {
                self.status = message.to_owned();
                self.error = Some(message.to_owned());
            }
            None => {
                self.error = None;
                self.status = "В эфире".into();
            }
        }
        vec![Outcome::State(self.sharing())]
    }

    /// A gate or session refusal of the current attempt. It is terminal and
    /// never restores the previous settings: a revoked permission or a lost
    /// voice connection must not start a publication again.
    pub fn on_attempt_rejected(&mut self, message: &str, now: Instant) -> Vec<Outcome> {
        self.fail(message, now)
    }

    /// A local media failure (capture, loopback or peer start). A failed
    /// restart restores the previously confirmed request once; the second
    /// failure is terminal instead of looping.
    pub(super) fn fail_local(&mut self, message: &str, now: Instant) -> Vec<Outcome> {
        if !self.rollback_used {
            if let Some(previous) = self.rollback.take() {
                return self.restore(previous, now);
            }
        }
        self.fail(message, now)
    }

    /// Restore the previous request on the same live stream slot.
    ///
    /// If the failed attempt already sent `stream.start`, the old server
    /// publication must be closed first: one `stream.stop` now and one
    /// `stream.start` after its acknowledgement (or the bounded timeout). The
    /// same `pending_restart`/`finish` path is reused, so the backend never
    /// sees two overlapping starts.
    fn restore(&mut self, previous: PublishRequest, now: Instant) -> Vec<Outcome> {
        self.rollback_used = true;
        let mut out = Vec::new();
        if self.engine_live {
            out.push(Outcome::StopEngine);
            self.engine_live = false;
        }
        self.error = None;
        self.preflighting = false;
        if self.start_sent && !self.stop_sent {
            self.stop_sent = true;
            out.push(Outcome::Send(self.signal("stream.stop", json!({}))));
            self.pending_restart = Some(previous);
            self.restarting = true;
            self.phase = Phase::Stopping;
            self.ignore_stop_echo = true;
            self.deadline = None;
            self.stop_deadline = Some(now + STOP_TIMEOUT);
            self.status = "Возвращаем прежние настройки…".into();
            self.stats = None;
            out.push(Outcome::State(self.sharing()));
            return out;
        }
        // No publication reached the server: the previous request starts
        // immediately on the same attempt slot.
        let mut started = self.start_restart(previous);
        self.status = "Возвращаем прежние настройки…".into();
        if let Some(Outcome::State(sharing)) = started.last_mut() {
            *sharing = self.sharing();
        }
        out.extend(started);
        out
    }
}
