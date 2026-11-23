//! Codec changes through the publish state machine: a codec is never a live
//! bitrate change, and both the attempted and the confirmed codec survive a
//! restart. Pure state-machine tests: no WebRTC, no device, no socket.

use super::quality::live;
use super::*;
use serde_json::json;
use std::time::Instant;

/// A codec-only change is a restart, never a live bitrate change.
#[test]
fn a_codec_change_is_a_restart_not_a_live_bitrate() {
    let now = Instant::now();
    let live_request = request();
    let next = PublishRequest {
        codec: Codec::Vp9,
        ..live_request.clone()
    };
    assert_eq!(live_request.change_to(&next), QualityChange::Restart);

    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;
    let confirmed = publisher.sharing().confirmed.clone();
    assert!(
        publisher.apply_quality(1, next.clone(), now).is_empty(),
        "a newer codec cannot be applied to the running sender"
    );
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().attempt, attempt, "no new attempt");
    assert!(
        publisher.sharing().confirmed == confirmed,
        "a refused live apply never replaces the confirmed codec"
    );
    assert_eq!(
        publisher.request().map(|request| request.codec),
        Some(Codec::Vp8),
        "the old codec stays on air"
    );
}

/// C06 success: one stop, then one start that carries the new codec and the
/// same concrete selection through the new attempt and its confirmation.
#[test]
fn a_confirmed_codec_restart_starts_one_new_attempt_with_that_codec() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;
    let next = PublishRequest {
        codec: Codec::Vp9,
        ..request()
    };

    publisher.preflight_started(1);
    let stop = publisher.restart(1, next.clone(), now);
    assert_eq!(
        sends(&stop)
            .iter()
            .filter(|signal| signal.kind == "stream.stop")
            .count(),
        1,
        "one stop before the new start: {stop:?}"
    );
    assert!(
        sends(&stop)
            .iter()
            .all(|signal| signal.kind != "stream.start"),
        "no new start before the stop ack: {stop:?}"
    );
    // The confirmed configuration still describes the stream on air.
    assert_eq!(
        publisher
            .sharing()
            .confirmed
            .map(|confirmed| confirmed.codec),
        Some(Codec::Vp8)
    );

    let ack = publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let started = ack
        .iter()
        .find_map(|outcome| match outcome {
            Outcome::StartEngine { attempt, request } => Some((*attempt, request.clone())),
            _ => None,
        })
        .expect("the new attempt starts after the ack");
    assert_eq!(started.0, attempt + 1);
    assert_eq!(started.1.codec, Codec::Vp9);

    let ready = publisher.engine_ready(now, false, None);
    let start = start_signal(&ready);
    assert_eq!(start.data["codec"], "vp9", "the wire carries the new codec");

    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    let confirmed = publisher
        .sharing()
        .confirmed
        .expect("the new attempt is live");
    assert_eq!(confirmed.codec, Codec::Vp9);
    assert_eq!(confirmed.attempt, attempt + 1);
    assert_eq!(
        publisher.request().map(|request| request.codec),
        Some(Codec::Vp9)
    );
}

/// C06 failure: a refused new start restores the confirmed codec exactly once,
/// and the second local failure is terminal instead of looping.
#[test]
fn a_refused_codec_start_restores_the_confirmed_configuration_once() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let previous = publisher.request().cloned().expect("live request");
    let next = PublishRequest {
        codec: Codec::Vp9,
        ..request()
    };
    publisher.preflight_started(1);
    publisher.restart(1, next.clone(), now);
    publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let ready = publisher.engine_ready(now, false, None);
    assert_eq!(start_signal(&ready).data["codec"], "vp9");

    // The new peer dies locally after its `stream.start` reached the server.
    let failed = publisher.on_engine(EngineEvent::Error("Соединение потеряно.".into()), now);
    assert_eq!(
        sends(&failed)
            .iter()
            .filter(|signal| signal.kind == "stream.stop")
            .count(),
        1,
        "the refused attempt releases its slot once: {failed:?}"
    );
    assert_eq!(publisher.phase(), Phase::Stopping);
    assert!(
        !failed
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "the previous attempt waits for the stop ack: {failed:?}"
    );

    let ack = publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let restored = ack
        .iter()
        .find_map(|outcome| match outcome {
            Outcome::StartEngine { request, .. } => Some(request.clone()),
            _ => None,
        })
        .expect("the previous request is restored after the ack");
    assert_eq!(
        restored, previous,
        "the confirmed VP8 configuration comes back"
    );
    let ready = publisher.engine_ready(now, false, None);
    assert_eq!(start_signal(&ready).data["codec"], "vp8");

    // The restored attempt fails too: no second automatic restore.
    let terminal =
        publisher.on_engine(EngineEvent::Error("Соединение потеряно снова.".into()), now);
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(
        !terminal
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "only one automatic restore is allowed: {terminal:?}"
    );
    assert!(publisher.sharing().error.is_some());
}

/// C06 revoke: a late local failure after the gate was revoked must not put the
/// previous publication back on air.
#[test]
fn a_late_failure_after_a_revoked_gate_does_not_restore_the_previous_codec() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    publisher.preflight_started(1);
    publisher.restart(
        1,
        PublishRequest {
            codec: Codec::Vp9,
            ..request()
        },
        now,
    );
    publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let ready = publisher.engine_ready(now, false, None);
    assert_eq!(start_signal(&ready).data["codec"], "vp9");

    // The server revokes the Stream permission while the new attempt starts.
    publisher.set_access(Access {
        joined: true,
        stream_permission: false,
        slots_available: true,
    });

    let failed = publisher.on_engine(EngineEvent::Error("Соединение потеряно.".into()), now);
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(
        !failed
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "a revoked permission must not restore the previous publication: {failed:?}"
    );
    assert_eq!(
        sends(&failed)
            .iter()
            .filter(|signal| signal.kind == "stream.stop")
            .count(),
        1,
        "the dead attempt still releases its server slot: {failed:?}"
    );
    assert_eq!(publisher.sharing().confirmed, None);

    // The error is delivered once; the closed attempt ignores a repeat.
    assert!(
        publisher
            .on_engine(EngineEvent::Error("Соединение потеряно.".into()), now)
            .is_empty(),
        "the terminal reason is not repeated"
    );
}
