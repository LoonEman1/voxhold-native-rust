//! Live-bitrate state-machine tests: `Sent` only queues the command, and
//! only a verified engine result may replace the confirmed configuration.

use super::quality::live;
use super::*;

fn bitrate_only() -> PublishRequest {
    PublishRequest {
        quality: Quality {
            bitrate_kbps: 7200,
            audio_bitrate_kbps: 256,
            ..Quality::default()
        },
        ..request()
    }
}

#[test]
fn a_bitrate_only_change_is_applied_to_the_live_sender_without_a_restart() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;
    let next = bitrate_only();
    assert_eq!(
        publisher.request().unwrap().change_to(&next),
        QualityChange::LiveBitrate
    );

    let previous = publisher.request().cloned().expect("live request");
    let previous_confirmed = publisher.sharing().confirmed.clone();
    let outcomes = publisher.apply_quality(1, next.clone(), now);
    assert_eq!(
        outcomes,
        vec![
            Outcome::SetBitrate {
                attempt,
                op: 1,
                quality: next.quality,
            },
            Outcome::State(publisher.sharing()),
        ],
        "one live limit and one state update: {outcomes:?}"
    );
    assert_eq!(publisher.phase(), Phase::Live, "the phase stays Live");
    assert_eq!(publisher.sharing().attempt, attempt, "no new attempt");
    assert!(!publisher.sharing().restarting);
    assert!(!publisher.sharing().preflighting);
    // The command only reached the worker queue: nothing is confirmed yet.
    assert_eq!(
        publisher
            .sharing()
            .quality_status
            .map(|status| status.outcome),
        Some(QualityOutcome::Applying)
    );
    assert_eq!(
        publisher.sharing().quality,
        Some(previous.quality),
        "the card still shows the on-air limit"
    );
    assert_eq!(publisher.request(), Some(&previous));
    assert_eq!(publisher.sharing().confirmed, previous_confirmed);
    assert!(
        publisher.sharing().status.contains("Применяем"),
        "the card says the limit is being applied: {:?}",
        publisher.sharing().status
    );
    assert!(
        !outcomes.iter().any(|outcome| matches!(
            outcome,
            Outcome::Send(_)
                | Outcome::StartEngine { .. }
                | Outcome::StopEngine
                | Outcome::LeaveViewer
        )),
        "no stop, no start and no viewer teardown on a bitrate-only change: {outcomes:?}"
    );

    // The engine verified the readback and confirms the change. Only now do
    // the request and the confirmed configuration follow it.
    let applied = publisher.on_engine(
        EngineEvent::Bitrate {
            attempt,
            op: 1,
            result: BitrateResult::Applied,
        },
        now,
    );
    assert!(
        matches!(applied.as_slice(), [Outcome::State(_)]),
        "{applied:?}"
    );
    assert_eq!(publisher.phase(), Phase::Live, "the phase stays Live");
    assert_eq!(publisher.sharing().attempt, attempt, "no new attempt");
    assert_eq!(publisher.sharing().quality, Some(next.quality));
    assert_eq!(publisher.request(), Some(&next));
    assert_eq!(
        publisher
            .sharing()
            .confirmed
            .map(|confirmed| confirmed.quality),
        Some(next.quality)
    );
    assert_eq!(
        publisher
            .sharing()
            .quality_status
            .map(|status| status.outcome),
        Some(QualityOutcome::Applied)
    );
    assert!(
        publisher.sharing().status.contains("Битрейт"),
        "the card can say the bitrate changed: {:?}",
        publisher.sharing().status
    );
    assert!(
        !applied.iter().any(|outcome| matches!(
            outcome,
            Outcome::Send(_)
                | Outcome::StartEngine { .. }
                | Outcome::StopEngine
                | Outcome::LeaveViewer
        )),
        "no stop, no start and no viewer teardown on a confirmed live change: {applied:?}"
    );

    // The published-audio bitrate is part of the same live path.
    let with_audio = PublishRequest {
        audio: true,
        ..request()
    };
    let louder_audio = PublishRequest {
        quality: Quality {
            audio_bitrate_kbps: 192,
            ..request().quality
        },
        ..with_audio.clone()
    };
    assert_eq!(
        with_audio.change_to(&louder_audio),
        QualityChange::LiveBitrate
    );
    let mut audio_publisher = self::publisher();
    audio_publisher.begin_picker(9);
    audio_publisher.publish(9, with_audio.clone(), false, now);
    audio_publisher.engine_ready(now, true, Some("Всё устройство вывода".into()));
    audio_publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    audio_publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    let applied = audio_publisher.apply_quality(1, louder_audio.clone(), now);
    assert!(matches!(
        applied.first(),
        Some(Outcome::SetBitrate { quality, .. }) if *quality == louder_audio.quality
    ));
    assert_eq!(audio_publisher.phase(), Phase::Live);
}

#[test]
fn a_full_command_queue_keeps_the_live_attempt_and_its_previous_limit() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;
    let applied_limit = publisher.request().expect("live request").quality;
    let queued = bitrate_only();
    publisher.apply_quality(1, queued.clone(), now);

    // The worker queue was full; the newest value was not accepted yet. The
    // operation stays `Applying`, never `Applied`.
    let pending = publisher.bitrate_queued(1);
    assert!(
        pending.is_empty() || pending == vec![Outcome::State(publisher.sharing())],
        "a deferred limit may only repeat its status: {pending:?}"
    );
    assert_eq!(
        publisher.phase(),
        Phase::Live,
        "a full queue must not fail the attempt"
    );
    assert_eq!(publisher.sharing().attempt, attempt, "no new attempt");
    assert!(publisher.sharing().error.is_none());
    assert_eq!(
        publisher
            .sharing()
            .quality_status
            .map(|status| status.outcome),
        Some(QualityOutcome::Applying),
        "a full queue must not claim the limit was applied"
    );
    assert_eq!(
        publisher.request().map(|request| request.quality),
        Some(applied_limit),
        "the previous confirmed limit stays on air"
    );
    assert!(
        publisher.sharing().status.contains("Применяем"),
        "the card says the limit is still being applied: {:?}",
        publisher.sharing().status
    );
    assert!(
        !pending.iter().any(|outcome| matches!(
            outcome,
            Outcome::Send(_) | Outcome::StartEngine { .. } | Outcome::StopEngine
        )),
        "a full queue must not stop or restart the publication: {pending:?}"
    );

    // The retried operation reached the worker and the engine verified it.
    let applied = publisher.on_engine(
        EngineEvent::Bitrate {
            attempt,
            op: 1,
            result: BitrateResult::Applied,
        },
        now,
    );
    assert!(matches!(applied.as_slice(), [Outcome::State(_)]));
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().quality, Some(queued.quality));
    assert_eq!(
        publisher.request().map(|request| request.quality),
        Some(queued.quality)
    );
    assert_eq!(
        publisher.bitrate_applied(1),
        Vec::new(),
        "a repeated confirmation is not a new status"
    );
    assert_ne!(
        applied_limit, queued.quality,
        "the test really changed the limit"
    );

    // A closed worker is terminal: it fails the local attempt instead of
    // pretending the limit reached the sender.
    let failed = publisher.on_engine(
        EngineEvent::Error("Медиадвижок трансляции уже закрыт.".into()),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(failed.contains(&Outcome::StopEngine));
    assert!(publisher.sharing().error.is_some());
    assert!(publisher.bitrate_queued(1).is_empty());
    assert!(publisher.bitrate_applied(1).is_empty());
}

/// A refused live change keeps the previous confirmed configuration on air:
/// phase `Live`, no stop, no restart, and the error names the failure.
#[test]
fn a_refused_live_bitrate_keeps_the_confirmed_limits_and_allows_a_retry() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;
    let previous = publisher.request().cloned().expect("live request");
    let previous_confirmed = publisher.sharing().confirmed.clone();
    let next = bitrate_only();
    publisher.apply_quality(1, next.clone(), now);

    let failed = publisher.on_engine(
        EngineEvent::Bitrate {
            attempt,
            op: 1,
            result: BitrateResult::Failed {
                message: "Не удалось применить новый битрейт (видео): отказ.".into(),
                restored: true,
                partial: false,
            },
        },
        now,
    );
    assert!(
        matches!(failed.as_slice(), [Outcome::State(_)]),
        "{failed:?}"
    );
    assert_eq!(publisher.phase(), Phase::Live, "the phase stays Live");
    assert_eq!(publisher.sharing().attempt, attempt, "the same attempt");
    assert_eq!(
        publisher.request(),
        Some(&previous),
        "the refused request never replaced the live one"
    );
    assert_eq!(
        publisher.sharing().confirmed,
        previous_confirmed,
        "the confirmed configuration stays on air"
    );
    assert!(
        publisher
            .sharing()
            .error
            .as_deref()
            .is_some_and(|error| error.contains("видео")),
        "the refusal is visible: {:?}",
        publisher.sharing().error
    );
    assert_eq!(
        publisher
            .sharing()
            .quality_status
            .map(|status| status.outcome),
        Some(QualityOutcome::Failed(
            "Не удалось применить новый битрейт (видео): отказ.".into()
        ))
    );
    assert!(
        !failed.iter().any(|outcome| matches!(
            outcome,
            Outcome::Send(_)
                | Outcome::StartEngine { .. }
                | Outcome::StopEngine
                | Outcome::LeaveViewer
        )),
        "a refused bitrate must not stop, restart or detach the viewer: {failed:?}"
    );

    // A retry is a new operation and starts from the confirmed request.
    assert_eq!(
        publisher.request().expect("live request").change_to(&next),
        QualityChange::LiveBitrate,
        "the confirmed request still differs from the refused one"
    );
    let retry = publisher.apply_quality(2, next.clone(), now);
    assert!(matches!(
        retry.first(),
        Some(Outcome::SetBitrate { attempt: a, op: 2, quality }) if *a == attempt && *quality == next.quality
    ));
    assert_eq!(
        publisher.sharing().quality_status.map(|status| status.op),
        Some(2)
    );
    let applied = publisher.on_engine(
        EngineEvent::Bitrate {
            attempt,
            op: 2,
            result: BitrateResult::Applied,
        },
        now,
    );
    assert!(matches!(applied.as_slice(), [Outcome::State(_)]));
    assert_eq!(publisher.request(), Some(&next));
    assert!(
        publisher.sharing().error.is_none(),
        "the successful retry clears the previous error"
    );
}

/// The result of operation N must not overwrite operation N+1.
#[test]
fn an_engine_result_of_an_older_bitrate_operation_is_ignored() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;
    let previous = publisher.request().cloned().expect("live request");
    let first = bitrate_only();
    let second = PublishRequest {
        quality: Quality {
            bitrate_kbps: 4200,
            ..first.quality
        },
        ..first.clone()
    };
    publisher.apply_quality(1, first.clone(), now);
    publisher.apply_quality(2, second.clone(), now);

    let stale = publisher.on_engine(
        EngineEvent::Bitrate {
            attempt,
            op: 1,
            result: BitrateResult::Applied,
        },
        now,
    );
    assert!(
        stale.is_empty(),
        "the result of operation 1 must not touch operation 2: {stale:?}"
    );
    assert_eq!(
        publisher.request(),
        Some(&previous),
        "the stale result must not confirm operation 1"
    );
    assert_eq!(
        publisher.sharing().quality_status.map(|status| status.op),
        Some(2)
    );
    assert_eq!(
        publisher
            .sharing()
            .quality_status
            .map(|status| status.outcome),
        Some(QualityOutcome::Applying)
    );

    let current = publisher.on_engine(
        EngineEvent::Bitrate {
            attempt,
            op: 2,
            result: BitrateResult::Applied,
        },
        now,
    );
    assert!(matches!(current.as_slice(), [Outcome::State(_)]));
    assert_eq!(publisher.request(), Some(&second));
    assert_ne!(publisher.request(), Some(&first));
}

/// A result of another publication attempt cannot change this one.
#[test]
fn an_engine_result_of_another_attempt_is_ignored() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;
    let previous = publisher.request().cloned().expect("live request");
    let next = bitrate_only();
    publisher.apply_quality(1, next.clone(), now);

    let stale = publisher.on_engine(
        EngineEvent::Bitrate {
            attempt: attempt + 1,
            op: 1,
            result: BitrateResult::Applied,
        },
        now,
    );
    assert!(
        stale.is_empty(),
        "a result of another attempt is not ours: {stale:?}"
    );
    assert_eq!(publisher.request(), Some(&previous));
    assert_eq!(
        publisher
            .sharing()
            .quality_status
            .map(|status| status.outcome),
        Some(QualityOutcome::Applying)
    );
}

/// A re-offer must use the last confirmed quality. A refused change leaves the
/// publisher request untouched, so the next answer keeps the old limit.
#[test]
fn a_reoffer_after_a_refusal_keeps_the_confirmed_quality() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;
    let previous = publisher.request().cloned().expect("live request");
    let next = bitrate_only();
    publisher.apply_quality(1, next.clone(), now);
    publisher.on_engine(
        EngineEvent::Bitrate {
            attempt,
            op: 1,
            result: BitrateResult::Failed {
                message: "Отказ.".into(),
                restored: true,
                partial: false,
            },
        },
        now,
    );
    assert_eq!(
        publisher.request(),
        Some(&previous),
        "the re-offer configuration is the confirmed request"
    );
    assert_eq!(
        publisher
            .sharing()
            .confirmed
            .map(|confirmed| confirmed.quality),
        Some(previous.quality)
    );
}
