//! ICE-restart answers: a repeated offer on a live publication is answered
//! again, and its confirmation must leave the on-air card untouched.

use super::*;

/// Publish, answer the first offer, confirm it and reach `Live`. Returns the
/// request id of the initial answer.
fn live_after_the_initial_answer(publisher: &mut Publisher, now: Instant) -> String {
    publishing(publisher, now);
    publisher.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp": "v=0 initial offer"}),
        now,
    );
    let answer = publisher.on_engine(EngineEvent::Answer("v=0 initial answer".into()), now);
    let id = sends(&answer)[0]
        .request_id
        .clone()
        .expect("tagged initial answer");
    let confirmed = publisher.on_server_event(
        "stream.webrtc_answered",
        &id,
        &json!({"accepted": true}),
        now,
    );
    assert!(matches!(confirmed.as_slice(), [Outcome::State(_)]));
    assert_eq!(publisher.phase(), Phase::Publishing);
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Live);
    id
}

#[test]
fn a_repeated_offer_while_live_is_answered_again_without_touching_the_publication() {
    let now = Instant::now();
    let mut publisher = publisher();
    let mut all = publishing(&mut publisher, now);
    // The initial negotiation: offer → answer → confirmation.
    let offer = publisher.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp": "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n"}),
        now,
    );
    assert!(matches!(offer.as_slice(), [Outcome::Offer(_)]));
    all.extend(offer);
    let answer = publisher.on_engine(EngineEvent::Answer("v=0 initial answer".into()), now);
    let initial = sends(&answer)[0]
        .request_id
        .clone()
        .expect("tagged initial answer");
    all.extend(answer);
    let confirmed = publisher.on_server_event(
        "stream.webrtc_answered",
        &initial,
        &json!({"accepted": true}),
        now,
    );
    assert!(matches!(confirmed.as_slice(), [Outcome::State(_)]));
    all.extend(confirmed);

    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Live);
    let attempt = publisher.sharing().attempt;
    let on_air = publisher.sharing().confirmed.clone();
    assert!(on_air.is_some(), "the live attempt is the confirmed one");

    // The backend restarts ICE on the same publication and offers again.
    let offer = publisher.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp": "v=0\r\nm=video 9 restart\r\n"}),
        now,
    );
    assert!(
        matches!(offer.as_slice(), [Outcome::Offer(sdp)] if sdp.contains("restart")),
        "{offer:?}"
    );
    all.extend(offer);
    let answer = publisher.on_engine(EngineEvent::Answer("v=0 restart answer".into()), now);
    let sent = sends(&answer);
    assert_eq!(sent.len(), 1, "exactly one answer: {answer:?}");
    assert_eq!(sent[0].kind, "stream.webrtc_answer");
    assert_eq!(sent[0].data["sdp"], "v=0 restart answer");
    let restart = sent[0].request_id.clone().expect("tagged restart answer");
    assert_ne!(restart, initial, "every answer gets its own request id");
    assert_eq!(publisher.phase(), Phase::Live);
    all.extend(answer);

    let confirmation = publisher.on_server_event(
        "stream.webrtc_answered",
        &restart,
        &json!({"accepted": true}),
        now,
    );
    assert!(
        confirmation.is_empty(),
        "a restart confirmation changes nothing: {confirmation:?}"
    );
    all.extend(confirmation);

    let sharing = publisher.sharing();
    assert_eq!(sharing.phase, Phase::Live);
    assert_eq!(sharing.status, "В эфире");
    assert_eq!(sharing.attempt, attempt);
    assert_eq!(
        sharing.confirmed, on_air,
        "the on-air settings stay confirmed"
    );

    // The whole flow sends exactly one start and no stop at all; the restart
    // produces no capture lifecycle either.
    assert_eq!(
        all.iter()
            .filter(
                |outcome| matches!(outcome, Outcome::Send(signal) if signal.kind == "stream.start")
            )
            .count(),
        1,
        "{all:?}"
    );
    assert_eq!(
        all.iter()
            .filter(
                |outcome| matches!(outcome, Outcome::Send(signal) if signal.kind == "stream.stop")
            )
            .count(),
        0,
        "{all:?}"
    );
    assert!(
        !all.iter()
            .any(|outcome| matches!(outcome, Outcome::StopEngine))
    );
    assert_eq!(
        all.iter()
            .filter(|outcome| matches!(outcome, Outcome::StartEngine { .. }))
            .count(),
        1,
        "{all:?}"
    );
}

#[test]
fn the_initial_answer_confirmation_still_moves_the_card_to_connecting() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "v=0 offer"}), now);
    let answer = publisher.on_engine(EngineEvent::Answer("v=0 answer".into()), now);
    let id = sends(&answer)[0].request_id.clone().expect("tagged answer");
    let confirmed = publisher.on_server_event(
        "stream.webrtc_answered",
        &id,
        &json!({"accepted": true}),
        now,
    );
    assert!(
        matches!(confirmed.as_slice(), [Outcome::State(_)]),
        "{confirmed:?}"
    );
    assert_eq!(publisher.phase(), Phase::Publishing);
    assert_eq!(publisher.sharing().status, "Подключаемся…");
    assert!(publisher.answered, "the initial negotiation is confirmed");
}

#[test]
fn a_late_initial_confirmation_does_not_regress_a_live_card() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "v=0 offer"}), now);
    let answer = publisher.on_engine(EngineEvent::Answer("v=0 answer".into()), now);
    let id = sends(&answer)[0].request_id.clone().expect("tagged answer");

    // The transport and the first frame reach the air before the server
    // confirmation of the initial answer.
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Live);

    let confirmed = publisher.on_server_event(
        "stream.webrtc_answered",
        &id,
        &json!({"accepted": true}),
        now,
    );
    assert!(
        confirmed.is_empty(),
        "the confirmation changes nothing on air: {confirmed:?}"
    );
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().status, "В эфире");
    assert!(publisher.answered);
}

#[test]
fn a_confirmation_of_a_superseded_answer_is_ignored() {
    let now = Instant::now();
    let mut publisher = publisher();
    live_after_the_initial_answer(&mut publisher, now);
    let attempt = publisher.sharing().attempt;

    // Offer A is answered but not confirmed yet.
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "offer a"}), now);
    let answer_a = publisher.on_engine(EngineEvent::Answer("answer a".into()), now);
    let first = sends(&answer_a)[0]
        .request_id
        .clone()
        .expect("tagged answer a");

    // Offer B supersedes A before the server confirms A.
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "offer b"}), now);
    let answer_b = publisher.on_engine(EngineEvent::Answer("answer b".into()), now);
    let second = sends(&answer_b)[0]
        .request_id
        .clone()
        .expect("tagged answer b");
    assert_ne!(first, second);

    let stale = publisher.on_server_event(
        "stream.webrtc_answered",
        &first,
        &json!({"accepted": true}),
        now,
    );
    assert!(
        stale.is_empty(),
        "the superseded answer cannot confirm this negotiation: {stale:?}"
    );
    // Even a refusal of the superseded answer must not end the live attempt.
    let refusal = publisher.on_server_event(
        "stream.webrtc_answered",
        &first,
        &json!({"accepted": false}),
        now,
    );
    assert!(
        refusal.is_empty(),
        "a stale refusal is not this attempt's: {refusal:?}"
    );
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().status, "В эфире");
    assert_eq!(publisher.sharing().attempt, attempt);

    let current = publisher.on_server_event(
        "stream.webrtc_answered",
        &second,
        &json!({"accepted": true}),
        now,
    );
    assert!(current.is_empty());
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().status, "В эфире");
    assert_eq!(publisher.sharing().attempt, attempt);
}

#[test]
fn a_refused_restart_answer_fails_and_stops_the_publication() {
    let now = Instant::now();
    let mut publisher = publisher();
    live_after_the_initial_answer(&mut publisher, now);
    publisher.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp": "offer restart"}),
        now,
    );
    let answer = publisher.on_engine(EngineEvent::Answer("answer restart".into()), now);
    let id = sends(&answer)[0].request_id.clone().expect("tagged answer");
    let failed = publisher.on_server_event(
        "stream.webrtc_answered",
        &id,
        &json!({"accepted": false}),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(failed.contains(&Outcome::StopEngine), "{failed:?}");
    assert_eq!(
        failed
            .iter()
            .filter(
                |outcome| matches!(outcome, Outcome::Send(signal) if signal.kind == "stream.stop")
            )
            .count(),
        1,
        "{failed:?}"
    );
}

/// The pinned backend has no `accepted:false` for an answer: it sends `error`
/// with that answer's request id. A newer offer/answer already took over the
/// negotiation, so that refusal must not end the live publication.
#[test]
fn an_error_tagged_with_a_superseded_answer_does_not_fail_the_live_publication() {
    let now = Instant::now();
    let mut publisher = publisher();
    live_after_the_initial_answer(&mut publisher, now);
    let attempt = publisher.sharing().attempt;

    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "offer a"}), now);
    let answer_a = publisher.on_engine(EngineEvent::Answer("answer a".into()), now);
    let superseded = sends(&answer_a)[0]
        .request_id
        .clone()
        .expect("tagged answer a");
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "offer b"}), now);
    let answer_b = publisher.on_engine(EngineEvent::Answer("answer b".into()), now);
    let current = sends(&answer_b)[0]
        .request_id
        .clone()
        .expect("tagged answer b");

    let stale = publisher.on_server_event(
        "error",
        &superseded,
        &json!({
            "code": "invalid_payload",
            "message": "invalid stream WebRTC answer payload",
        }),
        now,
    );
    assert!(
        stale.is_empty(),
        "a refusal of the replaced answer is not this negotiation: {stale:?}"
    );
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().status, "В эфире");
    assert_eq!(publisher.sharing().attempt, attempt);

    let failed = publisher.on_server_event(
        "error",
        &current,
        &json!({
            "code": "invalid_payload",
            "message": "invalid stream WebRTC answer payload",
        }),
        now,
    );
    assert_eq!(
        publisher.phase(),
        Phase::Failed,
        "the current answer's refusal still fails: {failed:?}"
    );
    assert!(failed.contains(&Outcome::StopEngine), "{failed:?}");
    assert_eq!(
        failed
            .iter()
            .filter(
                |outcome| matches!(outcome, Outcome::Send(signal) if signal.kind == "stream.stop")
            )
            .count(),
        1,
        "{failed:?}"
    );
}

/// Wire order `offer #1 → answer #1 → offer #2 → answered(#1)`: the newer
/// offer replaces #1 before its confirmation, so the late `answered(#1)` is a
/// no-op on the live card and the answer of #2 is still sent.
#[test]
fn a_late_confirmation_of_a_replaced_answer_keeps_live_and_the_new_answer_still_sends() {
    let now = Instant::now();
    let mut publisher = publisher();
    live_after_the_initial_answer(&mut publisher, now);
    let attempt = publisher.sharing().attempt;

    let first_offer = publisher.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp": "v=0 offer one"}),
        now,
    );
    assert!(
        matches!(first_offer.as_slice(), [Outcome::Offer(sdp)] if sdp.contains("offer one")),
        "{first_offer:?}"
    );
    let answer_one = publisher.on_engine(EngineEvent::Answer("v=0 answer one".into()), now);
    let superseded = sends(&answer_one)[0]
        .request_id
        .clone()
        .expect("tagged answer one");

    let second_offer = publisher.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp": "v=0 offer two"}),
        now,
    );
    assert!(
        matches!(second_offer.as_slice(), [Outcome::Offer(sdp)] if sdp.contains("offer two")),
        "{second_offer:?}"
    );
    let answer_two = publisher.on_engine(EngineEvent::Answer("v=0 answer two".into()), now);
    let sent = sends(&answer_two);
    assert_eq!(sent.len(), 1, "exactly one answer: {answer_two:?}");
    assert_eq!(sent[0].kind, "stream.webrtc_answer");
    assert_eq!(sent[0].data["sdp"], "v=0 answer two");
    let current = sent[0].request_id.clone().expect("tagged answer two");
    assert_ne!(superseded, current);

    let stale = publisher.on_server_event(
        "stream.webrtc_answered",
        &superseded,
        &json!({"accepted": true}),
        now,
    );
    assert!(
        stale.is_empty(),
        "the replaced answer cannot confirm the negotiation: {stale:?}"
    );
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().status, "В эфире");
    assert_eq!(publisher.sharing().attempt, attempt);

    let current_confirmation = publisher.on_server_event(
        "stream.webrtc_answered",
        &current,
        &json!({"accepted": true}),
        now,
    );
    assert!(
        current_confirmation.is_empty(),
        "a restart confirmation stays a no-op: {current_confirmation:?}"
    );
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().status, "В эфире");
}

/// The first answer is replaced while the attempt is still `Publishing`, so it
/// would have been classified as the initial one. Its late confirmation must
/// not mark the attempt answered or move the launch state: only the answer of
/// the current offer may decide the initial negotiation.
#[test]
fn a_stale_initial_confirmation_does_not_advance_the_replacement_negotiation() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "offer one"}), now);
    let answer_one = publisher.on_engine(EngineEvent::Answer("answer one".into()), now);
    let replaced = sends(&answer_one)[0]
        .request_id
        .clone()
        .expect("tagged answer one");
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "offer two"}), now);
    let answer_two = publisher.on_engine(EngineEvent::Answer("answer two".into()), now);
    let current = sends(&answer_two)[0]
        .request_id
        .clone()
        .expect("tagged answer two");

    let stale = publisher.on_server_event(
        "stream.webrtc_answered",
        &replaced,
        &json!({"accepted": true}),
        now,
    );
    assert!(
        stale.is_empty(),
        "the replaced initial answer cannot decide: {stale:?}"
    );
    assert_eq!(publisher.phase(), Phase::Publishing);
    assert!(
        !publisher.answered,
        "the replaced confirmation is not the current one"
    );
    assert_eq!(publisher.sharing().status, "Подключаемся…");

    let current_confirmation = publisher.on_server_event(
        "stream.webrtc_answered",
        &current,
        &json!({"accepted": true}),
        now,
    );
    assert!(
        matches!(current_confirmation.as_slice(), [Outcome::State(_)]),
        "{current_confirmation:?}"
    );
    assert_eq!(publisher.phase(), Phase::Publishing);
    assert_eq!(publisher.sharing().status, "Подключаемся…");
    assert!(
        publisher.answered,
        "the current initial answer confirmed the negotiation"
    );
}

/// The transport and the first frame can reach the air before the server
/// confirms the initial answer. A refusal that decides it is still terminal
/// and tears the live attempt down exactly once.
#[test]
fn a_late_initial_refusal_while_live_tears_down_exactly_once() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "offer"}), now);
    let answer = publisher.on_engine(EngineEvent::Answer("answer".into()), now);
    let id = sends(&answer)[0].request_id.clone().expect("tagged answer");
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Live);

    let failed = publisher.on_server_event(
        "stream.webrtc_answered",
        &id,
        &json!({"accepted": false}),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Failed);
    assert_eq!(
        failed
            .iter()
            .filter(|outcome| **outcome == Outcome::StopEngine)
            .count(),
        1,
        "the live engine is released once: {failed:?}"
    );
    assert_eq!(
        failed
            .iter()
            .filter(
                |outcome| matches!(outcome, Outcome::Send(signal) if signal.kind == "stream.stop")
            )
            .count(),
        1,
        "exactly one stream.stop: {failed:?}"
    );

    let duplicate = publisher.on_server_event(
        "stream.webrtc_answered",
        &id,
        &json!({"accepted": false}),
        now,
    );
    assert!(
        duplicate.is_empty(),
        "a duplicate refusal finds no active attempt: {duplicate:?}"
    );
}

/// The pinned backend's confirmation can arrive without a request id; at most
/// one answer is ever pending, so it belongs to it. Once it is decided, a
/// second untagged confirmation has nothing to decide.
#[test]
fn an_untagged_confirmation_decides_the_only_pending_answer() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    publisher.on_server_event("stream.webrtc_offer", "", &json!({"sdp": "offer"}), now);
    publisher.on_engine(EngineEvent::Answer("answer".into()), now);

    let confirmed = publisher.on_server_event(
        "stream.webrtc_answered",
        "",
        &json!({"accepted": true}),
        now,
    );
    assert!(
        matches!(confirmed.as_slice(), [Outcome::State(_)]),
        "{confirmed:?}"
    );
    assert!(publisher.answered);

    let duplicate = publisher.on_server_event(
        "stream.webrtc_answered",
        "",
        &json!({"accepted": true}),
        now,
    );
    assert!(
        duplicate.is_empty(),
        "there is no pending answer left: {duplicate:?}"
    );
    assert_eq!(publisher.phase(), Phase::Publishing);
}

/// A confirmation that arrives without any answer in flight cannot decide or
/// create a negotiation.
#[test]
fn a_confirmation_without_a_pending_answer_does_nothing() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);

    let untagged = publisher.on_server_event(
        "stream.webrtc_answered",
        "",
        &json!({"accepted": true}),
        now,
    );
    assert!(untagged.is_empty(), "{untagged:?}");
    assert_eq!(publisher.phase(), Phase::Publishing);
    assert!(!publisher.answered);

    let tagged = publisher.on_server_event(
        "stream.webrtc_answered",
        "native-screen-999",
        &json!({"accepted": true}),
        now,
    );
    assert!(tagged.is_empty(), "{tagged:?}");
    assert_eq!(publisher.phase(), Phase::Publishing);
    assert!(!publisher.answered);
}

/// After the initial confirmation moved the attempt to `Live`, the same
/// `stream.webrtc_answered` again has no pending answer and must not touch the
/// on-air card, the attempt or the confirmed settings.
#[test]
fn a_duplicate_confirmation_after_live_changes_nothing() {
    let now = Instant::now();
    let mut publisher = publisher();
    let id = live_after_the_initial_answer(&mut publisher, now);
    let attempt = publisher.sharing().attempt;
    let on_air = publisher.sharing().confirmed.clone();

    let duplicate = publisher.on_server_event(
        "stream.webrtc_answered",
        &id,
        &json!({"accepted": true}),
        now,
    );
    assert!(duplicate.is_empty(), "{duplicate:?}");
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().status, "В эфире");
    assert_eq!(publisher.sharing().attempt, attempt);
    assert_eq!(publisher.sharing().confirmed, on_air);
}
