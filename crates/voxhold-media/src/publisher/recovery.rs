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
        if !self.access.joined {
            self.phase = Phase::Publishing;
            return self.fail("Сначала подключитесь к голосовому каналу.", now);
        }
        if !self.access.stream_permission {
            self.phase = Phase::Publishing;
            return self.fail("Нет права запускать трансляцию.", now);
        }
        if !self.access.slots_available {
            self.phase = Phase::Publishing;
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
            // Mirror `publish`: the viewer is asked to leave while the
            // publisher still waits in `Picking`, so the normal `viewer_left`
            // path can emit exactly one `StartEngine`/`stream.start`.
            self.phase = Phase::Picking;
            self.viewer_wait = Some(now + LEAVE_TIMEOUT);
            self.status = "Завершаем просмотр…".into();
            return vec![Outcome::LeaveViewer, Outcome::State(self.sharing())];
        }
        self.start(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::{Access, Outcome as PublisherOutcome, Publisher};

    fn request() -> PublishRequest {
        PublishRequest {
            source: crate::screen_capture::ScreenSource {
                id: 1,
                kind: crate::screen_capture::SourceKind::Monitor,
                title: "Экран 1".into(),
                display_id: -1,
                width: 1920,
                height: 1080,
                monitor: None,
            },
            quality: crate::publisher::Quality::default(),
            codec: Codec::Vp8,
            audio: false,
            audio_scope: crate::system_audio::AudioScope::Device,
        }
    }

    fn publisher() -> Publisher {
        let mut publisher = Publisher::new(1, 10);
        publisher.set_connection(Some("me".into()));
        publisher.set_access(Access {
            joined: true,
            stream_permission: true,
            slots_available: true,
        });
        publisher
    }

    fn start_signals(outcomes: &[PublisherOutcome]) -> usize {
        outcomes
            .iter()
            .filter(|outcome| {
                matches!(outcome, PublisherOutcome::Send(signal) if signal.kind == "stream.start")
            })
            .count()
    }

    #[test]
    fn a_recovery_with_an_active_viewer_leaves_it_before_exactly_one_start() {
        let now = Instant::now();
        let mut publisher = publisher();
        let outcomes = publisher.recover(request(), true, now);
        assert!(
            matches!(
                outcomes.as_slice(),
                [PublisherOutcome::LeaveViewer, PublisherOutcome::State(_)]
            ),
            "the viewer is asked to leave first: {outcomes:?}"
        );
        assert_eq!(
            publisher.phase(),
            Phase::Picking,
            "the publisher must stay startable while the viewer leaves"
        );
        assert!(
            !outcomes
                .iter()
                .any(|outcome| matches!(outcome, PublisherOutcome::StartEngine { .. })),
            "no capture before the viewer is gone: {outcomes:?}"
        );

        let left = publisher.viewer_left(now + LEAVE_TIMEOUT / 2);
        assert!(
            matches!(left.as_slice(), [PublisherOutcome::StartEngine { .. }]),
            "the acknowledged leave starts the one attempt: {left:?}"
        );
        assert_eq!(publisher.phase(), Phase::Publishing);

        let started = publisher.engine_ready(now, false, None);
        assert_eq!(
            start_signals(&started),
            1,
            "exactly one stream.start: {started:?}"
        );
        assert!(
            publisher.viewer_left(now).is_empty(),
            "a repeated leave acknowledgement cannot start another attempt"
        );
    }

    #[test]
    fn a_slow_viewer_still_starts_the_recovery_after_the_timeout() {
        let now = Instant::now();
        let mut publisher = publisher();
        publisher.recover(request(), true, now);
        // The viewer never sends `voice.left`; the bounded wait still starts
        // the attempt through the ordinary clock path.
        let started = publisher.tick(now + LEAVE_TIMEOUT);
        assert!(
            matches!(started.as_slice(), [PublisherOutcome::StartEngine { .. }]),
            "the deadline starts the one attempt: {started:?}"
        );
        assert_eq!(publisher.phase(), Phase::Publishing);
    }
}
