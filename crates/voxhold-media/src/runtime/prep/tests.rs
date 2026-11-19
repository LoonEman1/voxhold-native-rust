//! Deterministic tests of the off-loop screen preparation. The engine is a
//! fake, so no desktop, server or WebRTC peer is required.

use super::super::*;
use super::*;
use crate::publisher::{
    Access, Codec, Outcome as PublisherOutcome, Phase, PublishRequest, Publisher, Quality,
};
use crate::screen_capture::{ScreenSource, SourceKind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct DropCounter(Arc<AtomicUsize>);

impl DropCounter {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

struct FakeEngine {
    dropped: DropCounter,
}

impl FakeEngine {
    fn new(dropped: &DropCounter) -> Self {
        Self {
            dropped: DropCounter(dropped.0.clone()),
        }
    }
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        self.dropped.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn access() -> Access {
    Access {
        joined: true,
        stream_permission: true,
        slots_available: true,
    }
}

fn binding(attempt: u64, connection: &str) -> PrepBinding {
    PrepBinding {
        attempt,
        server: 1,
        channel: 10,
        connection: Some(connection.into()),
        access: access(),
    }
}

fn screen_request() -> PublishRequest {
    PublishRequest {
        source: ScreenSource {
            id: 1,
            kind: SourceKind::Monitor,
            title: "Экран 1".into(),
            display_id: -1,
            width: 1920,
            height: 1080,
        },
        quality: Quality::default(),
        codec: Codec::Vp8,
        audio: false,
        audio_scope: crate::system_audio::AudioScope::Device,
    }
}

/// A publisher in `Publishing`, exactly where `StartEngine` is emitted.
fn publisher() -> Publisher {
    let now = Instant::now();
    let mut publisher = Publisher::new(1, 10);
    publisher.set_connection(Some("me".into()));
    publisher.set_access(access());
    publisher.begin_picker(1);
    let outcomes = publisher.publish(1, screen_request(), false, now);
    assert!(
        matches!(outcomes.as_slice(), [PublisherOutcome::StartEngine { .. }]),
        "the fixture must reach StartEngine: {outcomes:?}"
    );
    publisher
}

fn signal_count(outcomes: &[PublisherOutcome], kind: &str) -> usize {
    outcomes
        .iter()
        .filter(|outcome| matches!(outcome, PublisherOutcome::Send(signal) if signal.kind == kind))
        .count()
}

fn wait_for<E: Send + 'static>(prep: &mut ScreenPrep<E>, budget: Duration) -> PrepEvent<E> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(event) = prep.poll(Instant::now()) {
            return event;
        }
        assert!(Instant::now() < deadline, "the preparation did not answer");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_dropped(counter: &DropCounter, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while counter.count() == 0 {
        assert!(Instant::now() < deadline, "{label} was not released");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_delayed_preparation_does_not_block_the_session_requests() {
    let requests = Requests::default();
    // A push-to-talk/mute snapshot is queued while the capture would open.
    *requests.controls.lock().unwrap() = Some((
        Controls {
            muted: false,
            ..Controls::default()
        },
        false,
    ));
    let dropped = DropCounter::default();
    let worker = DropCounter(dropped.0.clone());
    let started = Instant::now();
    let mut prep = ScreenPrep::start(
        binding(1, "me"),
        Instant::now(),
        Duration::from_secs(5),
        move |_| {
            std::thread::sleep(Duration::from_millis(250));
            Ok((FakeEngine::new(&worker), false, None))
        },
    )
    .expect("the preparation thread starts");
    assert!(
        started.elapsed() < Duration::from_millis(150),
        "the capture start must not run on the session loop"
    );
    // The same loop drains the control request and observes the prep without
    // waiting for the blocking closure.
    assert!(matches!(requests.take(), Some(Pending::Controls(..))));
    assert!(
        prep.poll(Instant::now()).is_none(),
        "the capture is still opening"
    );
    let event = wait_for(&mut prep, Duration::from_secs(2));
    assert!(matches!(event, PrepEvent::Ready { .. }));
    assert_eq!(dropped.count(), 0, "the accepted engine is still owned");
}

#[test]
fn stream_start_waits_for_the_prepared_engine() {
    let now = Instant::now();
    let mut publisher = publisher();
    let binding = binding(publisher.sharing().attempt, "me");
    let dropped = DropCounter::default();
    let worker = DropCounter(dropped.0.clone());
    let mut prep = ScreenPrep::start(binding.clone(), now, Duration::from_secs(5), move |_| {
        std::thread::sleep(Duration::from_millis(150));
        Ok((
            FakeEngine::new(&worker),
            true,
            Some("Только звук test.exe (PID 1)".into()),
        ))
    })
    .expect("the preparation thread starts");

    // Nothing may reach the server while the capture is being prepared.
    assert!(publisher.tick(now).is_empty());
    assert!(prep.poll(now).is_none());
    assert_eq!(publisher.sharing().phase, Phase::Publishing);
    assert!(!publisher.sharing().live);

    let event = wait_for(&mut prep, Duration::from_secs(2));
    let finish = finish_prep(&mut publisher, &binding, &binding, access(), event, now);
    assert!(
        finish.engine.is_some(),
        "the engine is handed to the session"
    );
    assert!(finish.discard.is_none());
    assert_eq!(signal_count(&finish.outcomes, "stream.start"), 1);
    assert_eq!(
        signal_count(&finish.outcomes, "stream.stop"),
        0,
        "a successful start needs no compensating stop"
    );
    assert!(publisher.sharing().has_audio);
    assert_eq!(
        publisher.sharing().audio_scope.as_deref(),
        Some("Только звук test.exe (PID 1)"),
        "the prepared engine's real scope reaches the state machine"
    );
}

#[test]
fn cancel_before_completion_drops_the_late_engine_without_stream_start() {
    let now = Instant::now();
    let mut publisher = publisher();
    let dropped = DropCounter::default();
    let worker = DropCounter(dropped.0.clone());
    let prep = ScreenPrep::start(binding(1, "me"), now, Duration::from_secs(5), move |_| {
        std::thread::sleep(Duration::from_millis(150));
        Ok((FakeEngine::new(&worker), false, None))
    })
    .expect("the preparation thread starts");

    // A user Stop while the capture is opening.
    let mut prep = Some(prep);
    let mut outcomes = cancel_prep(&mut prep, &mut publisher, None, now);
    outcomes.extend(publisher.stop(now));
    assert_eq!(publisher.sharing().phase, Phase::Idle);
    assert_eq!(signal_count(&outcomes, "stream.start"), 0);
    assert_eq!(
        signal_count(&outcomes, "stream.stop"),
        0,
        "a cancelled preparation has no phantom stop"
    );
    wait_dropped(&dropped, "the cancelled engine");
}

#[test]
fn leave_and_access_loss_cancel_the_preparation() {
    let now = Instant::now();

    // Leave/logout uses the publisher transition, not an error.
    let mut leaving_publisher = publisher();
    let dropped = DropCounter::default();
    let worker = DropCounter(dropped.0.clone());
    let mut prep = Some(
        ScreenPrep::start(binding(1, "me"), now, Duration::from_secs(5), move |_| {
            std::thread::sleep(Duration::from_millis(100));
            Ok((FakeEngine::new(&worker), false, None))
        })
        .expect("the preparation thread starts"),
    );
    let _ = cancel_prep(&mut prep, &mut leaving_publisher, None, now);
    let outcomes = leaving_publisher.abandon();
    assert_eq!(leaving_publisher.sharing().phase, Phase::Idle);
    assert_eq!(signal_count(&outcomes, "stream.start"), 0);
    assert_eq!(signal_count(&outcomes, "stream.stop"), 0);
    wait_dropped(&dropped, "the abandoned engine");

    // A revoked Stream permission terminates the attempt visibly.
    let mut revoked_publisher = publisher();
    let dropped = DropCounter::default();
    let worker = DropCounter(dropped.0.clone());
    let mut prep = Some(
        ScreenPrep::start(binding(1, "me"), now, Duration::from_secs(5), move |_| {
            std::thread::sleep(Duration::from_millis(100));
            Ok((FakeEngine::new(&worker), false, None))
        })
        .expect("the preparation thread starts"),
    );
    let revoked = Access {
        stream_permission: false,
        ..access()
    };
    let message = access_violation(revoked).expect("a revoked gate has a reason");
    let outcomes = cancel_prep(&mut prep, &mut revoked_publisher, Some(message), now);
    assert_eq!(revoked_publisher.sharing().phase, Phase::Failed);
    assert_eq!(
        revoked_publisher.sharing().error.as_deref(),
        Some("Нет права запускать трансляцию.")
    );
    assert_eq!(signal_count(&outcomes, "stream.start"), 0);
    assert_eq!(signal_count(&outcomes, "stream.stop"), 0);
    wait_dropped(&dropped, "the engine of the failed attempt");
}

#[test]
fn a_late_result_of_an_old_attempt_room_or_connection_is_rejected() {
    let old = binding(1, "me");
    let cases = [
        ("attempt", binding(2, "me")),
        (
            "server",
            PrepBinding {
                server: 2,
                ..old.clone()
            },
        ),
        (
            "channel",
            PrepBinding {
                channel: 11,
                ..old.clone()
            },
        ),
        ("connection", binding(1, "other")),
    ];
    for (label, current) in cases {
        let now = Instant::now();
        let mut publisher = publisher();
        let dropped = DropCounter::default();
        let worker = DropCounter(dropped.0.clone());
        let mut prep = ScreenPrep::start(old.clone(), now, Duration::from_secs(5), move |_| {
            Ok((FakeEngine::new(&worker), false, None))
        })
        .expect("the preparation thread starts");
        let event = wait_for(&mut prep, Duration::from_secs(2));
        let finish = finish_prep(&mut publisher, &old, &current, access(), event, now);
        assert!(
            finish.engine.is_none(),
            "{label}: no engine for an old attempt"
        );
        assert!(finish.discard.is_some(), "{label}: the engine is released");
        assert!(
            finish.outcomes.is_empty(),
            "{label}: a stale result must not touch the publisher"
        );
        drop(finish);
        assert_eq!(dropped.count(), 1, "{label}: the engine was dropped");
        assert_eq!(
            publisher.sharing().phase,
            Phase::Publishing,
            "{label}: the current attempt is untouched"
        );
    }
}

#[test]
fn a_revoked_gate_rejects_the_prepared_engine() {
    let now = Instant::now();
    let mut publisher = publisher();
    let binding = binding(1, "me");
    let dropped = DropCounter::default();
    let worker = DropCounter(dropped.0.clone());
    let mut prep = ScreenPrep::start(binding.clone(), now, Duration::from_secs(5), move |_| {
        Ok((FakeEngine::new(&worker), false, None))
    })
    .expect("the preparation thread starts");
    let event = wait_for(&mut prep, Duration::from_secs(2));
    let revoked = Access {
        slots_available: false,
        ..access()
    };
    let finish = finish_prep(&mut publisher, &binding, &binding, revoked, event, now);
    assert!(finish.engine.is_none());
    assert!(finish.discard.is_some());
    assert_eq!(publisher.sharing().phase, Phase::Failed);
    assert!(publisher.sharing().error.is_some());
    assert_eq!(signal_count(&finish.outcomes, "stream.start"), 0);
    assert_eq!(signal_count(&finish.outcomes, "stream.stop"), 0);
    drop(finish);
    assert_eq!(dropped.count(), 1);
}

#[test]
fn a_preparation_timeout_fails_visibly_and_releases_the_engine() {
    let now = Instant::now();
    let mut publisher = publisher();
    let binding = binding(1, "me");
    let dropped = DropCounter::default();
    let worker = DropCounter(dropped.0.clone());
    let mut prep = ScreenPrep::start(binding.clone(), now, Duration::from_millis(60), move |_| {
        std::thread::sleep(Duration::from_millis(250));
        Ok((FakeEngine::new(&worker), false, None))
    })
    .expect("the preparation thread starts");
    let event = wait_for(&mut prep, Duration::from_secs(2));
    assert!(matches!(event, PrepEvent::Timeout));
    let finish = finish_prep(
        &mut publisher,
        &binding,
        &binding,
        access(),
        event,
        now + Duration::from_millis(61),
    );
    assert!(finish.engine.is_none());
    assert_eq!(publisher.sharing().phase, Phase::Failed);
    assert!(
        publisher.sharing().error.is_some(),
        "the timeout must be visible"
    );
    assert_eq!(signal_count(&finish.outcomes, "stream.start"), 0);
    assert_eq!(signal_count(&finish.outcomes, "stream.stop"), 0);
    // The worker still finishes its blocking stage, then drops the engine
    // because the receiver is gone.
    wait_dropped(&dropped, "the timed-out engine");
}

#[test]
fn a_new_publish_replaces_the_pending_preparation() {
    let now = Instant::now();
    let mut publisher = publisher();
    let first = DropCounter::default();
    let worker = DropCounter(first.0.clone());
    let mut prep = Some(
        ScreenPrep::start(binding(1, "me"), now, Duration::from_secs(5), move |_| {
            std::thread::sleep(Duration::from_millis(150));
            Ok((FakeEngine::new(&worker), false, None))
        })
        .expect("the preparation thread starts"),
    );
    // The StartEngine handler of a newer attempt cancels the one before it.
    let _ = cancel_prep(&mut prep, &mut publisher, None, now);
    assert!(prep.is_none(), "only one preparation may exist");

    let second = DropCounter::default();
    let worker = DropCounter(second.0.clone());
    let next = ScreenPrep::start(binding(1, "me"), now, Duration::from_secs(5), move |_| {
        Ok((FakeEngine::new(&worker), false, None))
    })
    .expect("the replacement starts");
    let mut next = Some(next);
    let event = wait_for(next.as_mut().unwrap(), Duration::from_secs(2));
    let finish = finish_prep(
        &mut publisher,
        &binding(1, "me"),
        &binding(1, "me"),
        access(),
        event,
        now,
    );
    assert!(finish.engine.is_some(), "the newer preparation wins");
    assert_eq!(signal_count(&finish.outcomes, "stream.start"), 1);
    wait_dropped(&first, "the replaced preparation");
    assert_eq!(second.count(), 0, "the accepted engine is still owned");
}

#[test]
fn a_duplicate_ready_is_discarded_instead_of_replacing_the_accepted_engine() {
    // A second `Ready` of the same attempt must never become the installed
    // engine: the publisher already sent `stream.start`, so the new peer was
    // never negotiated with the SFU.
    let now = Instant::now();
    let mut publisher = publisher();
    let binding = binding(publisher.sharing().attempt, "me");
    let accepted = DropCounter::default();
    let first = finish_prep(
        &mut publisher,
        &binding,
        &binding,
        access(),
        PrepEvent::Ready {
            engine: FakeEngine::new(&accepted),
            has_audio: true,
            audio_scope: Some("Только звук test.exe (PID 1)".into()),
        },
        now,
    );
    assert!(first.engine.is_some(), "the first engine is accepted");
    assert_eq!(signal_count(&first.outcomes, "stream.start"), 1);

    let duplicate = DropCounter::default();
    let second = finish_prep(
        &mut publisher,
        &binding,
        &binding,
        access(),
        PrepEvent::Ready {
            engine: FakeEngine::new(&duplicate),
            has_audio: false,
            audio_scope: None,
        },
        now,
    );
    assert!(
        second.engine.is_none(),
        "a second Ready never becomes the current attempt"
    );
    assert!(second.discard.is_some(), "the declined engine is released");
    assert!(
        second.outcomes.is_empty(),
        "the second Ready sends no stream.start/stop and no state: {:?}",
        second.outcomes
    );
    drop(second);
    assert_eq!(duplicate.count(), 1, "the declined engine is dropped");
    assert_eq!(accepted.count(), 0, "the accepted engine is untouched");
    assert_eq!(
        publisher.sharing().attempt,
        binding.attempt,
        "the duplicate does not begin a new attempt"
    );
    assert!(
        publisher.sharing().has_audio,
        "the duplicate must not change the accepted audio flag"
    );
}

#[test]
fn a_revocation_wins_over_a_ready_engine_the_session_has_not_observed() {
    // The exact order of `Live::poll_screen_prep`: the newest gate is applied
    // before the preparation is polled. `Live` needs a socket, so this drives
    // the same shared decision (`screen::apply_access`) over the same publisher
    // and preparation state, in the worst case where the engine is already
    // queued in the result channel and only the poll is missing.
    let now = Instant::now();
    let mut publisher = publisher();
    let binding = binding(publisher.sharing().attempt, "me");
    let dropped = DropCounter::default();
    let worker = DropCounter(dropped.0.clone());
    let prepared = Arc::new(AtomicBool::new(false));
    let flag = prepared.clone();
    let mut prep = Some(
        ScreenPrep::start(binding, now, Duration::from_secs(5), move |_| {
            let engine = FakeEngine::new(&worker);
            flag.store(true, Ordering::SeqCst);
            Ok((engine, false, None))
        })
        .expect("the preparation thread starts"),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !prepared.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the fake capture did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    // The worker sends the engine into the one-slot channel right after the
    // closure; the session has not observed it yet.
    std::thread::sleep(Duration::from_millis(50));

    let mut current = ScreenAccess {
        stream_permission: true,
        slot_available: true,
    };
    let outcomes = super::super::screen::apply_access(
        true,
        &mut current,
        &mut prep,
        &mut publisher,
        ScreenAccess {
            stream_permission: false,
            slot_available: true,
        },
    );
    assert!(
        prep.is_none(),
        "the pending preparation is cancelled before it is observed"
    );
    assert_eq!(publisher.sharing().phase, Phase::Failed);
    assert_eq!(
        publisher.sharing().error.as_deref(),
        Some("Нет права запускать трансляцию.")
    );
    assert_eq!(signal_count(&outcomes, "stream.start"), 0);
    assert_eq!(
        signal_count(&outcomes, "stream.stop"),
        0,
        "the engine never reached stream.start, so there is nothing to stop"
    );
    // The queued engine is released on the cancellation thread.
    wait_dropped(&dropped, "the engine of the revoked preparation");
}

#[test]
fn a_failure_of_an_old_attempt_does_not_fail_the_current_one() {
    let now = Instant::now();
    let mut publisher = publisher();
    let finish = finish_prep(
        &mut publisher,
        &binding(9, "old"),
        &binding(1, "me"),
        access(),
        PrepEvent::<()>::Failed("старый запуск".into()),
        now,
    );
    assert!(finish.outcomes.is_empty());
    assert_eq!(publisher.sharing().phase, Phase::Publishing);
    assert!(publisher.sharing().error.is_none());
}
