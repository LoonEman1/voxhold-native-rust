//! Token-rotation and publication recovery shared by the voice worker and the
//! handle. The state here is deliberately free of sockets, media engines and
//! the UI, so the two close/refresh orderings and every cancellation path are
//! testable headlessly.

use super::*;
use crate::publisher::{Phase, PublishRequest, Sharing};
use crate::viewer::Viewer;

/// A rotated token closes the old socket and clears the voice session. Wait
/// this long for the refresh answer before falling back to an ordinary
/// reconnect with whatever credential the shared client carries.
pub(super) const ROTATION_DEADLINE: Duration = Duration::from_secs(30);

/// The confirmed publication captured before transport loss. Only a request
/// the publisher really put on air becomes an intent, and only its exact
/// source/quality/audio scope is replayed.
#[derive(Clone, Debug)]
pub(super) struct PublishIntent {
    pub(super) request: PublishRequest,
    /// The last UI snapshot, changed to an explicit "recovering" state.
    pub(super) sharing: Sharing,
    /// Cancellation epoch at the moment the live attempt was confirmed. A
    /// Stop, Leave or newer publish bumps the epoch and drops a late store.
    pub(super) epoch: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RotationPhase {
    #[default]
    Idle,
    /// The refresh request is in flight. Do not re-authorize yet.
    Pending,
    /// The refresh answer arrived. A socket close may reconnect now.
    Ready,
}

/// One rotation of the current credential. `closed` remembers that the old
/// socket ended, so a refresh answer that arrives before or after that close
/// produces exactly one reconnect.
#[derive(Debug, Default)]
pub(super) struct Rotation {
    phase: RotationPhase,
    deadline: Option<Instant>,
    closed: bool,
    generation: u64,
}

impl Rotation {
    /// Start one rotation attempt. A repeated call is the same single-flight
    /// refresh with a refreshed deadline; it never creates a second join.
    pub(super) fn start(&mut self, now: Instant) -> u64 {
        self.phase = RotationPhase::Pending;
        self.deadline = Some(now + ROTATION_DEADLINE);
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// The refresh answer arrived. A late answer after the bounded wait is
    /// ignored instead of starting another reconnect.
    pub(super) fn finish(&mut self) -> bool {
        if self.phase != RotationPhase::Pending {
            return false;
        }
        self.phase = RotationPhase::Ready;
        self.deadline = None;
        true
    }

    pub(super) fn socket_closed(&mut self) {
        self.closed = true;
    }

    /// A reconnect is needed while a rotation is pending or just finished.
    pub(super) fn active(&self) -> bool {
        self.phase != RotationPhase::Idle
    }

    /// The first authorization of a new handle must wait while a refresh is in
    /// flight. The revoked credential cannot open a socket, so the one new
    /// socket is deferred until the answer arrives or the bounded deadline
    /// expires. A finished rotation authorizes immediately.
    pub(super) fn authorization_allowed(&self, now: Instant) -> bool {
        self.phase != RotationPhase::Pending || self.deadline.is_none_or(|deadline| now >= deadline)
    }

    /// The ordinary reconnect delay applies only when no rotation owns the
    /// link. While the refresh is in flight the worker waits for its answer or
    /// the bounded deadline instead.
    pub(super) fn should_reconnect(&self, now: Instant, ordinary: Instant) -> bool {
        if !self.closed {
            return false;
        }
        match self.phase {
            RotationPhase::Idle => now >= ordinary,
            RotationPhase::Ready => true,
            RotationPhase::Pending => self.deadline.is_none_or(|deadline| now >= deadline),
        }
    }

    /// The worker is opening the next socket: no close from the old generation
    /// can be treated as current, and a late refresh answer is ignored.
    pub(super) fn reconnected(&mut self) {
        self.phase = RotationPhase::Idle;
        self.deadline = None;
        self.closed = false;
    }

    #[cfg(test)]
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Keep the confirmed configuration but say plainly that it is being restored.
/// `Phase::Publishing` keeps the screen controls in their active state and
/// makes the panel publish its fresh gate for the new session.
pub(super) fn recovering_sharing(sharing: &Sharing) -> Sharing {
    let mut next = sharing.clone();
    next.phase = Phase::Publishing;
    next.live = false;
    next.status = "Восстанавливаем трансляцию…".into();
    next.error = None;
    next.restarting = false;
    next.preflighting = false;
    next.quality_status = None;
    next.stats = None;
    next
}

/// The intent taken by one session's recovery, kept until that recovered
/// attempt is really confirmed or fails visibly.
///
/// Taking the shared intent is not the end of the recovery: a second transport
/// loss between `publisher.recover()` and `maybe_live()` must not silently drop
/// the broadcast. The next session then finds the intent in the shared slot
/// again. One session starts the recovery at most once; a visible failure is
/// reported and is not retried behind the user's back.
#[derive(Debug, Default)]
pub(super) struct RecoveredAttempt {
    intent: Option<PublishIntent>,
    started: bool,
}

impl RecoveredAttempt {
    /// Take the pending intent and start this session's one recovery. A
    /// repeated late join confirmation or snapshot returns `None`.
    pub(super) fn start(
        &mut self,
        recovery: &PublishRecovery,
        epoch: u64,
    ) -> Option<PublishIntent> {
        if self.started {
            return None;
        }
        let intent = recovery.take(epoch)?;
        self.started = true;
        self.intent = Some(intent.clone());
        Some(intent)
    }

    /// The recovered attempt reached the air (its own confirmed request now
    /// describes what is live) or failed visibly. Either way this session no
    /// longer needs the old intent.
    pub(super) fn settle(&mut self, phase: Phase) {
        if matches!(phase, Phase::Live | Phase::Failed) {
            self.intent = None;
        }
    }

    /// The session ended before the recovered attempt was confirmed. Put the
    /// exact intent back under the epoch that still owns it, unless a Stop,
    /// Leave or newer publish already bumped it.
    pub(super) fn restore(&mut self, recovery: &PublishRecovery, current_epoch: u64) -> bool {
        let Some(intent) = self.intent.take() else {
            return false;
        };
        if intent.epoch != current_epoch {
            return false;
        }
        recovery.store(intent, current_epoch)
    }

    /// An explicit cancel or a visible failure ends the recovery for this
    /// session without putting the intent back.
    pub(super) fn clear(&mut self) {
        self.intent = None;
    }
}

/// Whether a `voice.left`/`voice.webrtc_closed`/`error` event is the teardown
/// of a socket that was authorized with the rotated-away token, rather than a
/// real end of this call.
///
/// Only a credential transport close is stale. An explicit moderation kick or
/// move and the "moved to another connection" replacement stay terminal even
/// while a rotation is pending or ready.
pub(super) fn stale_transport_close(
    kind: &str,
    data: &serde_json::Value,
    socket_token: &str,
    current_token: &str,
    rotation_active: bool,
) -> bool {
    if socket_token == current_token && !rotation_active {
        return false;
    }
    match kind {
        "voice.left" => {
            // `kicked`/`moved` carry a reason and a move carries the
            // destination channel. A bare leave is the transport teardown.
            let reason = data["reason"].as_str().unwrap_or_default();
            reason.is_empty() && data["destination_channel_id"].as_i64().unwrap_or(0) == 0
        }
        "voice.webrtc_closed" => {
            // A replaced session, a permission/encryption revocation or a
            // channel access loss is explicit and must end the call.
            let reason = data["reason"].as_str().unwrap_or_default();
            !matches!(
                reason,
                "voice session moved to another connection"
                    | "voice e2ee device was revoked"
                    | "voice permission was revoked"
                    | "channel access was revoked"
                    | "voice channel encryption changed"
            )
        }
        // The backend refuses the old credential with `unauthorized`; the
        // rotation's one retry must open the next socket with the new token.
        "error" => data["code"].as_str() == Some("unauthorized"),
        _ => false,
    }
}

impl Live {
    /// A fresh snapshot is authoritative for this socket; recovery may only
    /// act after the viewer really accepted one.
    pub(super) fn note_snapshot(&mut self, accepted: bool) {
        if accepted {
            self.snapshot_ready = true;
        }
    }

    /// The panel confirmed the screen gate for this session. Recovery waits
    /// for this value so a revoked permission can never restart the capture.
    pub(super) fn note_access(&mut self) {
        self.access_seen = true;
    }

    /// Restore the confirmed publication once the fresh snapshot, the join and
    /// the fresh permission gate all arrived. The intent is taken exactly once:
    /// a failed preparation/source is reported and never retried behind the
    /// user's back.
    pub(super) async fn recover_publish(&mut self, viewer: &mut Viewer) -> Result<(), SessionEnd> {
        if !self.joined || !self.snapshot_ready || !self.access_seen {
            return Ok(());
        }
        let epoch = self.cancel_epoch.load(Ordering::SeqCst);
        let Some(intent) = self.recovered.start(&self.recovery, epoch) else {
            return Ok(());
        };
        let outcomes = self.publisher.recover(
            intent.request,
            crate::runtime::screen::viewer_conflicts(viewer),
            Instant::now(),
        );
        self.apply_screen(outcomes, viewer).await?;
        // Keep the taken intent until the recovered attempt is confirmed. A
        // terminal failure is visible and must not be retried silently.
        self.recovered.settle(self.publisher.phase());
        Ok(())
    }

    /// Before this socket session is dropped, remember only the confirmed
    /// on-air request. A Stop/Leave/newer publish bumped the epoch after the
    /// capture, so a late drop cannot resurrect that older intent.
    ///
    /// A recovered attempt that did not reach the air puts its exact intent
    /// back, so the next session can retry it.
    pub(super) fn capture_publish_recovery(&mut self) {
        let epoch = self.cancel_epoch.load(Ordering::SeqCst);
        if epoch != self.recovery_epoch || self.publisher.phase() == Phase::Failed {
            // An explicit cancel or a visible terminal failure owns the UI
            // now; neither may silently resurrect the old broadcast.
            self.recovered.clear();
            return;
        }
        if let Some(request) = self.publisher.recovery_request() {
            self.recovered.clear();
            let sharing = recovering_sharing(&self.publisher.sharing());
            self.recovery.store(
                PublishIntent {
                    request,
                    sharing,
                    epoch: self.recovery_epoch,
                },
                epoch,
            );
            return;
        }
        self.recovered.restore(&self.recovery, epoch);
    }

    /// A close of a socket authenticated with the previous token belongs to the
    /// rotation, not to the new session. Explicit moderation and replacement
    /// reasons stay terminal.
    pub(super) fn stale_session(
        &self,
        client: &voxhold_api::Client,
        kind: &str,
        data: &serde_json::Value,
    ) -> bool {
        let rotation_active = self
            .rotation
            .lock()
            .map(|rotation| rotation.active())
            .unwrap_or(true);
        let current_token = client.token();
        stale_transport_close(
            kind,
            data,
            &self.session_token,
            &current_token,
            rotation_active,
        )
    }
}

/// The shared last-value slot for one recovery intent. `take` also consumes an
/// intent whose epoch is already stale.
#[derive(Debug, Default)]
pub(super) struct PublishRecovery {
    slot: Mutex<Option<PublishIntent>>,
}

impl PublishRecovery {
    /// Keep the intent only under the cancellation epoch that still owns it.
    /// A Stop/Leave/newer publish that landed between the capture and this
    /// store must win.
    pub(super) fn store(&self, intent: PublishIntent, current_epoch: u64) -> bool {
        if intent.epoch != current_epoch {
            return false;
        }
        let Ok(mut slot) = self.slot.lock() else {
            return false;
        };
        *slot = Some(intent);
        true
    }

    pub(super) fn take(&self, epoch: u64) -> Option<PublishIntent> {
        let mut slot = self.slot.lock().ok()?;
        let intent = slot.take()?;
        (intent.epoch == epoch).then_some(intent)
    }

    pub(super) fn clear(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = None;
        }
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.slot.lock().map(|slot| slot.is_none()).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(epoch: u64) -> PublishIntent {
        PublishIntent {
            request: crate::publisher::PublishRequest {
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
                codec: crate::publisher::Codec::Vp8,
                audio: true,
                audio_scope: crate::system_audio::AudioScope::Device,
                encoder: Default::default(),
            },
            sharing: Sharing::default(),
            epoch,
        }
    }

    #[test]
    fn a_close_before_the_refresh_answer_reconnects_once_after_it() {
        let now = Instant::now();
        let ordinary = now + RECONNECT_DELAY;
        let mut rotation = Rotation::default();
        rotation.start(now);
        // The backend closes the old socket before `/auth/refresh` answers.
        rotation.socket_closed();
        assert!(
            !rotation.should_reconnect(now, ordinary),
            "the old token must not be used for a second authorization"
        );
        assert!(rotation.finish());
        assert!(rotation.should_reconnect(now, ordinary));
        rotation.reconnected();
        assert!(
            !rotation.should_reconnect(now, ordinary),
            "one re-join only"
        );
        assert!(
            !rotation.finish(),
            "a duplicate late answer cannot arm another join"
        );
    }

    #[test]
    fn a_refresh_answer_before_the_close_reconnects_once_on_the_close() {
        let now = Instant::now();
        let ordinary = now + RECONNECT_DELAY;
        let mut rotation = Rotation::default();
        rotation.start(now);
        assert!(rotation.finish());
        assert!(
            !rotation.should_reconnect(now, ordinary),
            "a healthy old socket is not torn down a second time"
        );
        rotation.socket_closed();
        assert!(rotation.should_reconnect(now, ordinary));
        rotation.reconnected();
        assert!(
            !rotation.should_reconnect(now, ordinary),
            "one re-join only"
        );
    }

    #[test]
    fn a_lost_refresh_answer_is_bounded_and_not_retried_later() {
        let now = Instant::now();
        let ordinary = now + RECONNECT_DELAY;
        let mut rotation = Rotation::default();
        rotation.start(now);
        rotation.socket_closed();
        assert!(!rotation.should_reconnect(now + RECONNECT_DELAY, ordinary));
        assert!(rotation.should_reconnect(now + ROTATION_DEADLINE, ordinary));
        rotation.reconnected();
        assert!(!rotation.finish(), "a late answer is stale");
        assert!(!rotation.should_reconnect(now + ROTATION_DEADLINE, ordinary));
    }

    #[test]
    fn a_stop_or_leave_drops_the_pending_publish_intent() {
        let recovery = PublishRecovery::default();
        recovery.store(intent(7), 7);
        // The late join confirmation asks after the user pressed Stop.
        recovery.clear();
        assert!(
            recovery.take(7).is_none(),
            "no intent survives an explicit cancel"
        );
    }

    #[test]
    fn an_intent_from_a_newer_epoch_supersedes_the_old_one() {
        let recovery = PublishRecovery::default();
        recovery.store(intent(1), 1);
        assert!(recovery.take(2).is_none(), "a stale epoch is discarded");
        assert!(recovery.is_empty());
        assert!(
            !recovery.store(intent(1), 2),
            "a Stop between capture and store wins"
        );
        recovery.store(intent(3), 3);
        let kept = recovery.take(3).expect("the current intent survives");
        assert_eq!(kept.epoch, 3);
        assert_eq!(
            kept.request.audio_scope,
            crate::system_audio::AudioScope::Device
        );
    }

    #[test]
    fn the_handle_stop_and_leave_clear_a_stored_intent_synchronously() {
        let handle = VoiceHandle::detached_for_tests();
        let epoch = handle.requests.cancellation_epoch();
        handle.requests.publish_recovery.store(intent(epoch), epoch);
        handle.stop_screen_share();
        assert!(
            handle
                .requests
                .publish_recovery
                .take(handle.requests.cancellation_epoch())
                .is_none(),
            "a late join confirmation after Stop finds no intent"
        );

        let epoch = handle.requests.cancellation_epoch();
        handle.requests.publish_recovery.store(intent(epoch), epoch);
        handle.leave();
        assert!(
            handle
                .requests
                .publish_recovery
                .take(handle.requests.cancellation_epoch())
                .is_none(),
            "Leave drops recovery as well"
        );
    }

    fn live_publisher(
        request: crate::publisher::PublishRequest,
        now: Instant,
    ) -> crate::publisher::Publisher {
        use crate::publisher::{Access, Connection, EngineEvent, Phase, Publisher};
        let mut publisher = Publisher::new(1, 10);
        publisher.set_connection(Some("me".into()));
        publisher.set_access(Access {
            joined: true,
            stream_permission: true,
            slots_available: true,
        });
        publisher.begin_picker(1);
        publisher.publish(1, request, false, now);
        publisher.engine_ready(now, true, Some("Экран 1".into()));
        publisher.on_engine(EngineEvent::State(Connection::Connected), now);
        publisher.on_engine(EngineEvent::State(Connection::FirstFrame), now);
        assert_eq!(publisher.phase(), Phase::Live);
        publisher
    }

    #[test]
    fn a_live_publication_restarts_once_with_its_confirmed_request() {
        use crate::publisher::{Access, Outcome as PublisherOutcome, Publisher};
        let now = Instant::now();
        let request = intent(0).request;
        let live = live_publisher(request.clone(), now);
        let confirmed = live
            .recovery_request()
            .expect("a live attempt is recoverable");
        assert_eq!(confirmed.source, request.source);
        assert_eq!(confirmed.quality, request.quality);
        assert!(confirmed.audio);
        assert_eq!(
            confirmed.audio_scope,
            crate::system_audio::AudioScope::Device
        );

        let mut recovered = Publisher::new(1, 10);
        recovered.set_connection(Some("me".into()));
        recovered.set_access(Access {
            joined: true,
            stream_permission: true,
            slots_available: true,
        });
        let first = recovered.recover(confirmed.clone(), false, now);
        assert!(matches!(
            first.as_slice(),
            [PublisherOutcome::StartEngine { .. }]
        ));
        // A repeated/late join confirmation must not start a second attempt.
        assert!(recovered.recover(confirmed.clone(), false, now).is_empty());
        let started = recovered.engine_ready(now, true, Some("Экран 1".into()));
        assert_eq!(
            started
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    PublisherOutcome::Send(signal) if signal.kind == "stream.start"
                ))
                .count(),
            1,
            "exactly one stream.start: {started:?}"
        );
        let start = started
            .iter()
            .find_map(|outcome| match outcome {
                PublisherOutcome::Send(signal) if signal.kind == "stream.start" => Some(signal),
                _ => None,
            })
            .expect("the one start signal");
        assert_eq!(start.data["codec"], "vp8");
        assert_eq!(start.data["has_audio"], true);
        assert_eq!(start.data["server_id"], 1);
        assert_eq!(start.data["channel_id"], 10);
    }

    #[test]
    fn stop_and_leave_clear_the_confirmed_recovery_request() {
        let now = Instant::now();
        let request = intent(0).request;
        let mut stopped = live_publisher(request.clone(), now);
        stopped.stop(now);
        assert!(
            stopped.recovery_request().is_none(),
            "a stopped broadcast is not replayed"
        );
        let mut left = live_publisher(request, now);
        left.abandon();
        assert!(
            left.recovery_request().is_none(),
            "leaving the call is not a recovery intent"
        );
    }

    #[test]
    fn revoked_access_fails_recovery_without_opening_a_capture() {
        use crate::publisher::{Access, Outcome as PublisherOutcome, Publisher};
        let now = Instant::now();
        let mut publisher = Publisher::new(1, 10);
        publisher.set_connection(Some("me".into()));
        publisher.set_access(Access {
            joined: true,
            stream_permission: false,
            slots_available: true,
        });
        let outcomes = publisher.recover(intent(0).request, false, now);
        assert!(
            !outcomes
                .iter()
                .any(|outcome| matches!(outcome, PublisherOutcome::StartEngine { .. })),
            "a revoked permission starts no capture: {outcomes:?}"
        );
        assert!(matches!(outcomes.as_slice(), [PublisherOutcome::State(_)]));
        assert_eq!(publisher.phase(), crate::publisher::Phase::Failed);
        assert!(publisher.sharing().error.is_some());
    }

    #[test]
    fn a_rotation_generation_changes_on_every_attempt() {
        let now = Instant::now();
        let mut rotation = Rotation::default();
        let first = rotation.start(now);
        assert!(rotation.finish());
        rotation.reconnected();
        let second = rotation.start(now);
        assert_ne!(first, second);
        assert_eq!(rotation.generation(), second);
    }

    #[test]
    fn a_pending_rotation_defers_the_first_authorization_until_answer_or_deadline() {
        let now = Instant::now();
        let mut rotation = Rotation::default();
        assert!(rotation.authorization_allowed(now), "an idle link waits");

        rotation.start(now);
        assert!(
            !rotation.authorization_allowed(now),
            "the revoked old token must not open the first socket"
        );
        assert!(
            !rotation.authorization_allowed(now + ROTATION_DEADLINE - Duration::from_millis(1))
        );
        assert!(
            rotation.authorization_allowed(now + ROTATION_DEADLINE),
            "a lost answer is bounded, not permanent"
        );

        let mut answered = Rotation::default();
        answered.start(now);
        assert!(answered.finish());
        assert!(
            answered.authorization_allowed(now),
            "the answer releases the deferred authorization"
        );
    }

    #[test]
    fn moderation_and_replacement_stay_terminal_during_a_rotation() {
        let kick = serde_json::json!({"server_id": 1, "channel_id": 10, "reason": "kicked"});
        let moved = serde_json::json!({
            "server_id": 1,
            "channel_id": 10,
            "reason": "moved",
            "destination_channel_id": 20
        });
        let replaced = serde_json::json!({"reason": "voice session moved to another connection"});
        // Rotation is Pending/Ready but the credential itself may not have
        // changed yet, so only the explicit event decides.
        for data in [kick, moved] {
            assert!(
                !stale_transport_close("voice.left", &data, "old", "old", true),
                "a moderator kick/move must end the call: {data}"
            );
        }
        assert!(
            !stale_transport_close("voice.webrtc_closed", &replaced, "old", "old", true),
            "a replaced session must not be swallowed by the rotation"
        );
        assert!(
            !stale_transport_close(
                "voice.webrtc_closed",
                &serde_json::json!({"reason": "channel access was revoked"}),
                "old",
                "old",
                true
            ),
            "an explicit access revocation is terminal"
        );

        // Only the old-generation credential teardown is stale.
        assert!(stale_transport_close(
            "voice.left",
            &serde_json::json!({"server_id": 1, "channel_id": 10}),
            "old",
            "new",
            false
        ));
        assert!(stale_transport_close(
            "voice.webrtc_closed",
            &serde_json::json!({"reason": ""}),
            "old",
            "old",
            true
        ));
        assert!(
            !stale_transport_close(
                "voice.left",
                &serde_json::json!({"server_id": 1, "channel_id": 10}),
                "token",
                "token",
                false
            ),
            "without a rotation the same close is a real end"
        );
    }

    #[test]
    fn an_auth_rejection_of_the_old_socket_token_recovers_once() {
        let unauthorized = serde_json::json!({"code": "unauthorized"});
        // Session B authorized with the revoked token while the shared client
        // already held the rotated one.
        assert!(
            stale_transport_close("error", &unauthorized, "old", "new", false),
            "the refusal of the token this socket sent must retry"
        );
        // Session C is the one retry, authorized with the current token. The
        // same refusal is now a real terminal rejection.
        assert!(
            !stale_transport_close("error", &unauthorized, "new", "new", false),
            "exactly one retry, never a loop"
        );
        assert!(
            !stale_transport_close(
                "error",
                &serde_json::json!({"code": "forbidden"}),
                "old",
                "new",
                false
            ),
            "a non-auth refusal is not a credential rotation"
        );
    }

    #[test]
    fn a_second_loss_during_recovery_keeps_the_intent_for_the_next_session() {
        use crate::publisher::{Access, Outcome as PublisherOutcome, Phase, Publisher};

        let now = Instant::now();
        let recovery = PublishRecovery::default();
        // Session A was live and its intent was captured before the loss.
        let live = live_publisher(intent(0).request, now);
        let request = live.recovery_request().expect("confirmed live request");
        assert!(recovery.store(
            PublishIntent {
                request,
                sharing: recovering_sharing(&live.sharing()),
                epoch: 0,
            },
            0
        ));

        let mut access = Access {
            joined: true,
            stream_permission: true,
            slots_available: true,
        };
        // Session B starts the recovered attempt and sends `stream.start`.
        let mut b = Publisher::new(1, 10);
        b.set_connection(Some("me".into()));
        b.set_access(access);
        let mut taken_b = RecoveredAttempt::default();
        let first = taken_b.start(&recovery, 0).expect("B recovers the intent");
        let outcomes = b.recover(first.request, false, now);
        assert!(matches!(
            outcomes.as_slice(),
            [PublisherOutcome::StartEngine { .. }]
        ));
        assert!(
            taken_b.start(&recovery, 0).is_none(),
            "one recovery start per session"
        );
        let started = b.engine_ready(now, false, None);
        assert!(started.iter().any(
            |outcome| matches!(outcome, PublisherOutcome::Send(signal) if signal.kind == "stream.start")
        ));
        // The second transport loss happens before the first frame: the
        // recovered attempt never becomes Live, so its publisher has no
        // confirmed request of its own.
        assert_eq!(b.phase(), Phase::Publishing);
        assert!(b.recovery_request().is_none());
        taken_b.settle(b.phase());
        assert!(
            taken_b.restore(&recovery, 0),
            "the unconfirmed intent is put back for the next session"
        );

        // A visible terminal failure consumes the intent instead.
        let mut failed = RecoveredAttempt::default();
        let taken = failed.start(&recovery, 0).expect("the intent is back");
        assert_eq!(taken.epoch, 0);
        failed.settle(Phase::Failed);
        assert!(
            !failed.restore(&recovery, 0),
            "a failure the user saw is not retried silently"
        );

        // Put it back once more to model session C.
        assert!(recovery.store(
            PublishIntent {
                request: intent(0).request,
                sharing: recovering_sharing(&Sharing::default()),
                epoch: 0,
            },
            0
        ));
        access.joined = true;
        let mut c = Publisher::new(1, 10);
        c.set_connection(Some("me".into()));
        c.set_access(access);
        let mut taken_c = RecoveredAttempt::default();
        let third = taken_c.start(&recovery, 0).expect("C still has the intent");
        let outcomes = c.recover(third.request, false, now);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, PublisherOutcome::StartEngine { .. }))
                .count(),
            1,
            "session C starts exactly one recovery: {outcomes:?}"
        );
        let started = c.engine_ready(now, false, None);
        assert_eq!(
            started
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    PublisherOutcome::Send(signal) if signal.kind == "stream.start"
                ))
                .count(),
            1,
            "exactly one stream.start after the retry: {started:?}"
        );
    }
}
