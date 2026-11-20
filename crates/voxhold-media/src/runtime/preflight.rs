//! Off-loop preflight of a live quality change.
//!
//! The live attempt is never stopped on the strength of the dialog alone. The
//! new capture and audio scope are opened in a temporary attempt on the
//! preparation thread, a bounded first frame is awaited and both devices are
//! released. Only a successful result hands the request to
//! [`Publisher::restart`], which performs the confirmed stop+start.
//!
//! The session loop only starts this work, observes one event per tick and
//! cancels it. A failure keeps the live stream in place and is shown as an
//! error without emitting `stream.stop`.

use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use crate::{
    publisher::{
        Access, Outcome as PublisherOutcome, Phase, PublishRequest, Publisher,
        preflight::{PreflightReport, run},
    },
    runtime::prep::{PrepBinding, PrepEvent, ScreenPrep, access_violation},
};

/// Upper bound of the whole preflight. Both device stages have their own
/// bounds; this one covers a driver call that never returns.
pub(super) const PREFLIGHT_BUDGET: Duration = Duration::from_secs(10);
pub(super) const PREFLIGHT_TIMEOUT_MESSAGE: &str =
    "Проверка новых настроек не завершилась за 10 секунд. Текущая трансляция не тронута.";

/// One bounded result of the preflight worker. A successful run only proves
/// that the new capture and scope opened; the picture itself is discarded.
pub(super) enum PreflightEvent {
    Ready,
    Failed(String),
    Timeout,
}

/// A preflight in flight. It reuses the task-7 preparation lifecycle: bounded
/// poll, cancellation token checked at every stage and an off-loop release of
/// a result nobody wants.
pub(super) struct PendingRestart {
    prep: ScreenPrep<PreflightReport>,
    request: PublishRequest,
}

impl PendingRestart {
    pub(super) fn start(
        binding: PrepBinding,
        now: Instant,
        request: PublishRequest,
    ) -> Result<Self, String> {
        let plan = request.clone();
        Self::start_with(binding, now, request, move |cancelled| {
            run(&plan, cancelled, PREFLIGHT_BUDGET)
        })
    }

    /// The test entry point: the same worker with an injected capture. The
    /// production closure opens a real capture and loopback.
    pub(super) fn start_with<F>(
        binding: PrepBinding,
        now: Instant,
        request: PublishRequest,
        launch: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(&AtomicBool) -> Result<PreflightReport, String> + Send + 'static,
    {
        let prep = ScreenPrep::start(binding, now, PREFLIGHT_BUDGET, move |cancelled| {
            let report = launch(cancelled)?;
            Ok((report, false, None))
        })?;
        Ok(Self { prep, request })
    }

    pub(super) fn binding(&self) -> &PrepBinding {
        self.prep.binding()
    }

    pub(super) fn request(&self) -> &PublishRequest {
        &self.request
    }

    /// Abandon the preflight. The worker observes the token at its next stage
    /// boundary and drops a capture that was already opened.
    pub(super) fn cancel(&mut self) {
        self.prep.cancel();
    }

    pub(super) fn poll(&mut self, now: Instant) -> Option<PreflightEvent> {
        match self.prep.poll(now)? {
            PrepEvent::Ready { .. } => Some(PreflightEvent::Ready),
            PrepEvent::Failed(message) => Some(PreflightEvent::Failed(message)),
            PrepEvent::Timeout => Some(PreflightEvent::Timeout),
        }
    }
}

/// Apply one preflight event to the live publisher.
///
/// A ready result commits the existing confirmed stop+start. A failure and a
/// timeout keep the attempt exactly where it is: `Live`, same capture, same
/// viewer, no `stream.stop`. A result of an older attempt or room is dropped.
pub(super) fn finish_preflight(
    publisher: &mut Publisher,
    binding: &PrepBinding,
    current: &PrepBinding,
    access: Access,
    event: PreflightEvent,
    request: PublishRequest,
    now: Instant,
) -> Vec<PublisherOutcome> {
    let current_attempt = binding.same_attempt(current)
        && publisher.sharing().attempt == binding.attempt
        && publisher.phase() == Phase::Live;
    match event {
        PreflightEvent::Ready => {
            if !current_attempt {
                return Vec::new();
            }
            if let Some(message) =
                access_violation(binding.access).or_else(|| access_violation(access))
            {
                // The gate that allowed the preflight is gone. The live stream
                // stays untouched; the server will decide about the old one.
                return publisher.preflight_failed(message);
            }
            publisher.restart(request, now)
        }
        PreflightEvent::Failed(message) => {
            if current_attempt {
                publisher.preflight_failed(&message)
            } else {
                Vec::new()
            }
        }
        PreflightEvent::Timeout => {
            if current_attempt {
                publisher.preflight_failed(PREFLIGHT_TIMEOUT_MESSAGE)
            } else {
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::{
        Access, Codec, Connection, EngineEvent, Outcome as PublisherOutcome, Publisher, Quality,
    };
    use crate::screen_capture::{ScreenSource, SourceKind};
    use crate::system_audio::AudioScope;

    fn source() -> ScreenSource {
        ScreenSource {
            id: 1,
            kind: SourceKind::Monitor,
            title: "Экран 1".into(),
            display_id: -1,
            width: 1920,
            height: 1080,
            monitor: None,
            topology: None,
        }
    }

    fn request(quality: Quality) -> PublishRequest {
        PublishRequest {
            source: source(),
            quality,
            codec: Codec::Vp8,
            audio: false,
            audio_scope: AudioScope::Device,
        }
    }

    fn allowed() -> Access {
        Access {
            joined: true,
            stream_permission: true,
            slots_available: true,
        }
    }

    /// A live publisher whose accepted request is `Quality::default()`.
    fn live_publisher(now: Instant) -> Publisher {
        let mut publisher = Publisher::new(1, 10);
        publisher.set_connection(Some("me".into()));
        publisher.set_access(allowed());
        publisher.begin_picker(1);
        publisher.publish(1, request(Quality::default()), false, now);
        publisher.engine_ready(now, false, None);
        publisher.on_engine(EngineEvent::State(Connection::Connected), now);
        publisher.on_engine(EngineEvent::State(Connection::FirstFrame), now);
        assert_eq!(publisher.phase(), Phase::Live);
        publisher
    }

    fn binding(publisher: &Publisher) -> PrepBinding {
        PrepBinding {
            attempt: publisher.sharing().attempt,
            server: 1,
            channel: 10,
            connection: Some("me".into()),
            access: allowed(),
        }
    }

    fn poll_ready(pending: &mut PendingRestart) -> PreflightEvent {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(event) = pending.poll(Instant::now()) {
                return event;
            }
            assert!(
                Instant::now() < deadline,
                "the preflight worker never reported"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn changed() -> Quality {
        Quality {
            resolution: crate::publisher::Resolution::P720,
            fps: 60,
            bitrate_kbps: 9000,
            audio_bitrate_kbps: 128,
        }
    }

    #[test]
    fn a_successful_preflight_commits_one_stop_before_any_start() {
        let now = Instant::now();
        let mut publisher = live_publisher(now);
        let binding = binding(&publisher);
        let mut pending =
            PendingRestart::start_with(binding.clone(), now, request(changed()), |_| {
                Ok(PreflightReport {
                    width: 1280,
                    height: 720,
                    audio_scope: None,
                })
            })
            .expect("the preflight thread starts");
        let event = poll_ready(&mut pending);
        assert!(matches!(event, PreflightEvent::Ready));
        let outcomes = finish_preflight(
            &mut publisher,
            &binding,
            &binding,
            allowed(),
            event,
            request(changed()),
            now,
        );
        let kinds: Vec<&str> = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                PublisherOutcome::Send(signal) => Some(signal.kind),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, vec!["stream.stop"], "{outcomes:?}");
        assert!(
            !outcomes
                .iter()
                .any(|outcome| matches!(outcome, PublisherOutcome::StartEngine { .. })),
            "the new capture waits for the stop acknowledgement: {outcomes:?}"
        );
        assert_eq!(publisher.phase(), Phase::Stopping);
        assert!(publisher.sharing().restarting);

        // Only the acknowledgement starts the new attempt, exactly once.
        let ack = publisher.on_server_event("stream.stopped", "", &serde_json::json!({}), now);
        let starts: Vec<_> = ack
            .iter()
            .filter_map(|outcome| match outcome {
                PublisherOutcome::StartEngine { request, .. } => Some(request.quality),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![changed()]);
        assert_eq!(publisher.phase(), Phase::Publishing);
    }

    #[test]
    fn a_failed_preflight_keeps_the_live_stream_and_sends_nothing() {
        let now = Instant::now();
        let mut publisher = live_publisher(now);
        let binding = binding(&publisher);
        let attempt = publisher.sharing().attempt;
        let mut pending =
            PendingRestart::start_with(binding.clone(), now, request(changed()), |_| {
                Err("Системный звук недоступен".into())
            })
            .expect("the preflight thread starts");
        let event = poll_ready(&mut pending);
        let outcomes = finish_preflight(
            &mut publisher,
            &binding,
            &binding,
            allowed(),
            event,
            request(changed()),
            now,
        );
        assert!(
            !outcomes.iter().any(|outcome| matches!(
                outcome,
                PublisherOutcome::Send(_)
                    | PublisherOutcome::StartEngine { .. }
                    | PublisherOutcome::StopEngine
            )),
            "a failed preflight must not touch the live attempt: {outcomes:?}"
        );
        let sharing = publisher.sharing();
        assert_eq!(sharing.phase, Phase::Live);
        assert_eq!(sharing.attempt, attempt);
        assert!(!sharing.restarting);
        assert!(!sharing.preflighting);
        assert!(
            sharing
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Системный звук")),
            "the reason reaches the dialog: {sharing:?}"
        );
        assert_eq!(
            sharing.quality,
            Some(Quality::default()),
            "the live quality is unchanged"
        );
    }

    #[test]
    fn a_restart_whose_confirmed_display_is_missing_never_stops_the_live_stream() {
        use crate::screen_capture::{
            CapturerMonitor, MonitorIdentity, MonitorRect, ResolveError, resolve_monitor,
        };

        let identity = MonitorIdentity {
            device: r"\\.\DISPLAY1".into(),
            bounds: MonitorRect::new(0, 0, 1920, 1080),
            work: MonitorRect::new(0, 0, 1920, 1080),
            width: 1920,
            height: 1080,
        };
        let other = MonitorIdentity {
            device: r"\\.\DISPLAY2".into(),
            ..identity.clone()
        };
        // The confirmed display is gone; the capturer now only offers another
        // one, possibly under the same numeric id.
        let error = resolve_monitor(
            &identity,
            &[other],
            &[CapturerMonitor {
                id: 0,
                title: "Screen 2".into(),
            }],
        )
        .expect_err("the confirmed display must not resolve");

        let now = Instant::now();
        let mut publisher = live_publisher(now);
        let binding = binding(&publisher);
        let attempt = publisher.sharing().attempt;
        let mut pending =
            PendingRestart::start_with(binding.clone(), now, request(changed()), move |_| {
                Err(error.message().to_owned())
            })
            .expect("the preflight thread starts");
        let event = poll_ready(&mut pending);
        assert!(matches!(event, PreflightEvent::Failed(_)));
        let outcomes = finish_preflight(
            &mut publisher,
            &binding,
            &binding,
            allowed(),
            event,
            request(changed()),
            now,
        );
        assert!(
            !outcomes.iter().any(|outcome| matches!(
                outcome,
                PublisherOutcome::Send(_)
                    | PublisherOutcome::StartEngine { .. }
                    | PublisherOutcome::StopEngine
            )),
            "a missing restart display must not stop or replace the live attempt: {outcomes:?}"
        );
        let sharing = publisher.sharing();
        assert_eq!(sharing.phase, Phase::Live);
        assert_eq!(sharing.attempt, attempt);
        assert!(!sharing.restarting);
        assert_eq!(error, ResolveError::Missing);
    }

    #[test]
    fn a_timed_out_preflight_keeps_the_live_stream() {
        let now = Instant::now();
        let mut publisher = live_publisher(now);
        let binding = binding(&publisher);
        let outcomes = finish_preflight(
            &mut publisher,
            &binding,
            &binding,
            allowed(),
            PreflightEvent::Timeout,
            request(changed()),
            now,
        );
        assert!(
            outcomes
                .iter()
                .all(|outcome| matches!(outcome, PublisherOutcome::State(_))),
            "{outcomes:?}"
        );
        assert_eq!(publisher.phase(), Phase::Live);
        assert!(publisher.sharing().error.is_some());
    }

    #[test]
    fn a_device_loss_during_the_preflight_keeps_the_live_stream() {
        let now = Instant::now();
        let mut publisher = live_publisher(now);
        let binding = binding(&publisher);
        let outcomes = finish_preflight(
            &mut publisher,
            &binding,
            &binding,
            allowed(),
            PreflightEvent::Failed("Устройство вывода отключено.".into()),
            request(changed()),
            now,
        );
        assert!(
            !outcomes.iter().any(|outcome| matches!(
                outcome,
                PublisherOutcome::Send(_)
                    | PublisherOutcome::StartEngine { .. }
                    | PublisherOutcome::StopEngine
            )),
            "a device loss in the preflight must not stop the live stream: {outcomes:?}"
        );
        let sharing = publisher.sharing();
        assert_eq!(sharing.phase, Phase::Live);
        assert!(
            sharing
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Устройство"))
        );
    }

    #[test]
    fn a_result_of_another_attempt_is_discarded() {
        let now = Instant::now();
        let mut publisher = live_publisher(now);
        let binding = binding(&publisher);
        let stale = PrepBinding {
            attempt: binding.attempt.wrapping_sub(1),
            ..binding.clone()
        };
        let outcomes = finish_preflight(
            &mut publisher,
            &stale,
            &binding,
            allowed(),
            PreflightEvent::Failed("late".into()),
            request(changed()),
            now,
        );
        assert!(outcomes.is_empty(), "{outcomes:?}");
        let sharing = publisher.sharing();
        assert_eq!(sharing.phase, Phase::Live);
        assert!(sharing.error.is_none());
    }

    #[test]
    fn a_cancelled_preflight_releases_its_worker_and_result() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let now = Instant::now();
        let publisher = live_publisher(now);
        let binding = binding(&publisher);
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        let mut pending =
            PendingRestart::start_with(binding, now, request(changed()), move |cancelled| {
                // The worker observes the token like the real device stages do.
                while !cancelled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                worker_finished.store(true, Ordering::SeqCst);
                Err(crate::publisher::preflight::PREFLIGHT_CANCELLED.into())
            })
            .expect("the preflight thread starts");
        pending.cancel();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !finished.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "a cancelled preflight kept its worker running"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            pending.poll(Instant::now()).is_none(),
            "a cancelled preflight must not report a result"
        );
    }
}
