//! Deterministic viewer regressions. No sockets, no WebRTC and no timers:
//! the state machine receives the clock and explicit server events.

use super::*;

mod stall;

fn publication(connection: &str, user: i64) -> Publication {
    Publication {
        server_id: 1,
        channel_id: 10,
        publisher_user_id: user,
        publisher_connection_id: connection.into(),
        mode: "server".into(),
        codec: "vp8".into(),
        has_audio: true,
        viewer_count: 1,
        started_at_ms: 1,
    }
}

fn snapshot(viewer: &mut Viewer, streams: &[Publication], now: Instant) -> Vec<Outcome> {
    viewer.on_server_event("stream.snapshot", "", &json!({"streams": streams}), now)
}

/// Pull the one `Start` out of the outcome list.
fn start_of(outcomes: &[Outcome]) -> (String, Publication) {
    let starts: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Start {
                publication,
                request,
                ..
            } => Some((
                request.request_id.clone().unwrap_or_default(),
                publication.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1, "expected exactly one start: {outcomes:?}");
    starts.into_iter().next().unwrap()
}

fn sends(outcomes: &[Outcome], kind: &str) -> Vec<String> {
    outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Send(signal) if signal.kind == kind => signal.request_id.clone(),
            _ => None,
        })
        .collect()
}

#[test]
fn switching_to_b_then_c_starts_only_c_after_the_leave_ack() {
    let now = Instant::now();
    let a = publication("a", 1);
    let b = publication("b", 2);
    let c = publication("c", 3);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone(), b.clone(), c.clone()], now);
    assert!(viewer.set_joined(true, now).is_empty());

    let started = viewer.select(&a, 1, now);
    let (watch_a, current) = start_of(&started);
    assert_eq!(current.publisher_connection_id, "a");
    assert_eq!(
        viewer
            .watching()
            .unwrap()
            .publication
            .publisher_connection_id,
        "a"
    );

    // A is running; the user picks B and immediately C.
    let to_b = viewer.select(&b, 1, now);
    let leave = sends(&to_b, "stream.leave");
    assert_eq!(leave.len(), 1, "the first switch must leave A: {to_b:?}");
    let to_c = viewer.select(&c, 1, now);
    assert!(
        sends(&to_c, "stream.leave").is_empty(),
        "leave is already in flight"
    );
    assert!(
        viewer.watching().is_none(),
        "A must be closed before B/C start"
    );
    assert_eq!(viewer.intent().unwrap().publisher_connection_id, "c");
    // Neither B nor C may watch before A acknowledged the leave.
    assert!(
        !to_c
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Start { .. }))
    );

    // Signals of attempts that never became active are ignored.
    let stale = viewer.on_server_event(
        "stream.ice_candidate",
        &leave[0],
        &json!({"candidate":"candidate:1 1 udp 1 127.0.0.1 9 typ host"}),
        now,
    );
    assert!(stale.is_empty(), "ICE of the closing attempt: {stale:?}");
    let stale = viewer.on_server_event("stream.webrtc_offer", "", &json!({"sdp":"offer-b"}), now);
    assert!(
        stale.is_empty(),
        "offer of B must not start anything: {stale:?}"
    );

    let acked = viewer.on_server_event("stream.left", &leave[0], &json!({}), now);
    let (watch_c, current) = start_of(&acked);
    assert_eq!(current.publisher_connection_id, "c");
    assert_ne!(watch_c, watch_a);
    assert_eq!(
        viewer
            .watching()
            .unwrap()
            .publication
            .publisher_connection_id,
        "c"
    );
}

#[test]
fn stopping_a_does_not_stop_the_active_b() {
    let now = Instant::now();
    let a = publication("a", 1);
    let b = publication("b", 2);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone(), b.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    // Switch to B and let A leave.
    let switching = viewer.select(&b, 1, now);
    let leave = sends(&switching, "stream.leave");
    let ack = viewer.on_server_event("stream.left", &leave[0], &json!({}), now);
    start_of(&ack);
    assert_eq!(
        viewer
            .watching()
            .unwrap()
            .publication
            .publisher_connection_id,
        "b"
    );

    // A late stop for A only updates the catalog.
    let outcomes = viewer.on_server_event(
        "stream.stopped",
        "",
        &json!({"server_id":1,"channel_id":10,"publisher_connection_id":"a"}),
        now,
    );
    assert_eq!(outcomes, vec![Outcome::Catalog]);
    assert_eq!(
        viewer
            .watching()
            .unwrap()
            .publication
            .publisher_connection_id,
        "b"
    );
    assert_eq!(viewer.catalog().list().len(), 1);

    // A stop for B ends the viewer.
    let outcomes = viewer.on_server_event(
        "stream.stopped",
        "",
        &json!({"server_id":1,"channel_id":10,"publisher_connection_id":"b"}),
        now,
    );
    assert!(viewer.watching().is_none());
    assert!(outcomes.iter().any(
        |outcome| matches!(outcome, Outcome::Failed { message, .. } if message == "Трансляция завершена")
    ));
}

#[test]
fn a_republication_keeps_the_old_entry_until_it_stops() {
    let now = Instant::now();
    let old = publication("old", 3);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[old], now);
    let mut new = publication("new", 3);
    new.viewer_count = 2;
    viewer.on_server_event("stream.started", "", &json!(new.clone()), now);
    assert_eq!(
        viewer.catalog().list().len(),
        2,
        "same user, new connection"
    );
    viewer.on_server_event(
        "stream.stopped",
        "",
        &json!({"server_id":1,"channel_id":10,"publisher_connection_id":"old"}),
        now,
    );
    assert_eq!(viewer.catalog().list().len(), 1);
    assert_eq!(viewer.catalog().list()[0].publisher_connection_id, "new");
    assert_eq!(viewer.catalog().list()[0].viewer_count, 2);
}

#[test]
fn a_closed_call_keeps_the_selection_but_not_the_engine() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    let (_, _) = start_of(&viewer.select(&a, 1, now));
    let gone = viewer.set_joined(false, now);
    assert!(gone.contains(&Outcome::Close));
    assert!(viewer.watching().is_none());
    assert_eq!(viewer.intent().unwrap().publisher_connection_id, "a");
    // Neither the stale selection nor the old catalog may start before the
    // fresh snapshot arrives, in either order.
    assert!(viewer.set_joined(true, now).is_empty());
    let without_snapshot = viewer.tick(now + START_TIMEOUT);
    assert!(without_snapshot.is_empty());
    let back = snapshot(&mut viewer, &[a.clone()], now);
    let (watch, publication) = start_of(&back);
    assert!(watch.starts_with("native-stream-"));
    assert_eq!(publication.publisher_connection_id, "a");
}

#[test]
fn request_errors_and_stale_requests_are_separated_by_id() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    let (watch, _) = start_of(&viewer.select(&a, 1, now));
    // An error that is not a stream request belongs to the voice join.
    let ignored =
        viewer.on_server_event("error", "voice-join-1", &json!({"code":"forbidden"}), now);
    assert!(ignored.is_empty());
    // The real watch request failing ends the viewer with its own text.
    let failed = viewer.on_server_event("error", &watch, &json!({"code":"forbidden"}), now);
    assert!(failed.iter().any(
        |outcome| matches!(outcome, Outcome::Failed { message, .. } if message.contains("просмотр"))
    ));
    assert!(viewer.watching().is_none());
    assert!(viewer.intent().is_none());
    // A duplicate error after the attempt ended changes nothing.
    let ignored = viewer.on_server_event("error", &watch, &json!({"code":"forbidden"}), now);
    assert!(ignored.is_empty());
}

#[test]
fn revocation_and_server_leave_end_the_viewer_with_their_reason() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    let revoked = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"reason":"viewer permission was revoked"}),
        now,
    );
    assert!(revoked.iter().any(
        |outcome| matches!(outcome, Outcome::Failed { message, .. } if message == "Трансляция завершена")
    ));

    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    let left = viewer.on_server_event("stream.left", "", &json!({}), now);
    assert!(
        left.iter()
            .any(|outcome| matches!(outcome, Outcome::Failed { message, .. } if message.contains("завершён сервером")))
    );
    assert!(viewer.intent().is_none());
}

#[test]
fn recovery_is_bounded_and_always_targets_the_active_publisher() {
    let now = Instant::now();
    let mut a = publication("a", 1);
    a.mode = "p2p".into();
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    let first = viewer.on_engine_state("disconnected", now);
    let restart = sends(&first, "stream.p2p_restart");
    assert_eq!(restart.len(), 1);
    // The same ICE failure reported again as `failed` is still one retry.
    let duplicate = viewer.on_engine_state("failed", now);
    assert!(sends(&duplicate, "stream.p2p_restart").is_empty());
    // A decoded frame ends the recovery; the next loss spends retry two.
    viewer.on_frame();
    let second = viewer.on_engine_state("failed", now);
    assert_eq!(sends(&second, "stream.p2p_restart").len(), 1);
    // The third failure does not send another restart; the deadline ends it.
    let third = viewer.on_engine_state("failed", now);
    assert!(sends(&third, "stream.p2p_restart").is_empty());
    let deadline = viewer.tick(now + START_TIMEOUT + Duration::from_millis(1));
    assert!(deadline.iter().any(
        |outcome| matches!(outcome, Outcome::Failed { message, .. } if message.contains("восстановить"))
    ));
}

#[test]
fn a_leave_without_ack_is_abandoned_after_the_deadline() {
    let now = Instant::now();
    let a = publication("a", 1);
    let b = publication("b", 2);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone(), b.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    viewer.select(&b, 1, now);
    assert!(
        viewer
            .tick(now + LEAVE_TIMEOUT - Duration::from_millis(1))
            .is_empty(),
        "A is still waiting"
    );
    let expired = viewer.tick(now + LEAVE_TIMEOUT + Duration::from_millis(1));
    assert!(expired.iter().any(
        |outcome| matches!(outcome, Outcome::Failed { message, .. } if message.contains("предыдущий"))
    ));
    assert!(viewer.intent().is_none());
}

#[test]
fn mute_only_changes_the_stream_attempt() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    assert_eq!(
        viewer.set_muted(1, true),
        vec![Outcome::Mute {
            selection: 1,
            muted: true
        }]
    );
    assert!(viewer.muted());
    viewer.leave(now);
    assert!(!viewer.muted());
}

#[test]
fn late_signaling_and_an_untagged_close_from_a_do_not_touch_b() {
    let now = Instant::now();
    let a = publication("a", 1);
    let b = publication("b", 2);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone(), b.clone()], now);
    viewer.set_joined(true, now);
    let (watch_a, _) = start_of(&viewer.select(&a, 1, now));
    // A's accepted offer establishes the gereration that must be retired.
    let accepted = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp":"v=0\r\na=ice-ufrag:a-ufrag\r\n"}),
        now,
    );
    assert_eq!(
        accepted,
        vec![Outcome::Offer("v=0\r\na=ice-ufrag:a-ufrag\r\n".into())]
    );

    let switching = viewer.select(&b, 2, now);
    let leave = sends(&switching, "stream.leave");
    let ack = viewer.on_server_event("stream.left", &leave[0], &json!({}), now);
    start_of(&ack);

    let stale_offer = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp":"v=0\r\na=ice-ufrag:a-ufrag\r\n"}),
        now,
    );
    assert!(stale_offer.is_empty(), "retired offer: {stale_offer:?}");
    let stale_ice = viewer.on_server_event(
        "stream.ice_candidate",
        "",
        &json!({"candidate":"candidate:1 1 udp 1 127.0.0.1 9 typ host ufrag a-ufrag"}),
        now,
    );
    assert!(stale_ice.is_empty(), "retired ICE: {stale_ice:?}");
    let stale_close = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"reason":"disconnected"}),
        now,
    );
    assert!(stale_close.is_empty(), "stale close: {stale_close:?}");
    let tagged_stale = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"watch_id":watch_a,"reason":"permission revoked"}),
        now,
    );
    assert!(
        tagged_stale.is_empty(),
        "a close tagged with A's watch id must not end B: {tagged_stale:?}"
    );
    assert_eq!(
        viewer
            .watching()
            .unwrap()
            .publication
            .publisher_connection_id,
        "b"
    );

    // B's own offer is accepted and ends the stale-close grace.
    let current = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp":"v=0\r\na=ice-ufrag:b-ufrag\r\n"}),
        now,
    );
    assert_eq!(
        current,
        vec![Outcome::Offer("v=0\r\na=ice-ufrag:b-ufrag\r\n".into())]
    );
    let close_b = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"reason":"disconnected"}),
        now,
    );
    assert!(
        !close_b.is_empty(),
        "a close after B's own signaling belongs to B"
    );
}

#[test]
fn an_offer_before_the_watching_ack_is_kept_for_that_attempt() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    let early = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp":"v=0\r\na=ice-ufrag:early\r\n"}),
        now,
    );
    assert_eq!(
        early,
        vec![Outcome::Offer("v=0\r\na=ice-ufrag:early\r\n".into())]
    );
}

#[test]
fn reconnect_never_substitutes_another_connection_of_the_same_user() {
    let now = Instant::now();
    let a = publication("a", 1);
    let b = publication("b", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone(), b.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 7, now));

    viewer.set_joined(false, now);
    // The fresh snapshot has only B, owned by the same user.
    let fresh = snapshot(&mut viewer, &[b], now);
    assert!(
        !fresh
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Start { .. })),
        "the stale connection must not be re-targeted"
    );
    let joined = viewer.set_joined(true, now);
    assert!(
        !joined
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Start { .. })),
        "only an exact publication can be watched"
    );
    assert!(joined.iter().any(|outcome| matches!(
        outcome,
        Outcome::Failed { selection: 7, message } if message.contains("недоступна")
    )));
    assert!(viewer.intent().is_none());
}

#[test]
fn a_first_frame_after_recovery_disarms_the_old_deadline() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    assert!(!viewer.on_frame().is_empty());
    let left = viewer.on_engine_state("disconnected", now);
    assert!(left.iter().any(|outcome| matches!(
        outcome,
        Outcome::Connected {
            selection: 1,
            connected: false
        }
    )));
    let back = viewer.on_frame();
    assert!(back.iter().any(|outcome| matches!(
        outcome,
        Outcome::Status { selection: 1, text } if text == "Подключено"
    )));
    assert!(
        viewer
            .tick(now + START_TIMEOUT + Duration::from_millis(1))
            .is_empty(),
        "a working picture must not be closed by the old recovery deadline"
    );
    assert!(viewer.watching().is_some());
}

#[test]
fn a_close_for_the_previous_selection_does_not_end_the_new_one() {
    let now = Instant::now();
    let a = publication("a", 1);
    let b = publication("b", 2);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone(), b.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    let switching = viewer.select(&b, 2, now);
    let leave = sends(&switching, "stream.leave");
    viewer.on_server_event("stream.left", &leave[0], &json!({}), now);
    let stale = viewer.leave_selection(1, now);
    assert!(stale.is_empty(), "close A must not close B: {stale:?}");
    assert_eq!(
        viewer
            .watching()
            .unwrap()
            .publication
            .publisher_connection_id,
        "b"
    );
    let current = viewer.leave_selection(2, now);
    assert!(current.contains(&Outcome::Close));
    assert!(viewer.watching().is_none());
}

#[test]
fn a_publication_missing_before_the_watch_command_is_rejected_explicitly() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    let mut missing = publication("gone", 2);
    missing.channel_id = 10;
    let rejected = viewer.select(&missing, 9, now);
    assert!(rejected.iter().any(|outcome| matches!(
        outcome,
        Outcome::Failed { selection: 9, message } if message.contains("недоступна")
    )));
    assert!(viewer.watching().is_none());
}

#[test]
fn closing_the_player_releases_the_attempt_but_keeps_the_selection_empty() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    let closed = viewer.leave(now);
    assert!(closed.contains(&Outcome::Close));
    assert_eq!(sends(&closed, "stream.leave").len(), 1);
    assert!(
        closed
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Clear { selection: 1 }))
    );
    assert!(viewer.watching().is_none());
    assert!(viewer.intent().is_none());
}

#[test]
fn a_tagged_close_from_the_previous_attempt_is_rejected_after_the_new_ack() {
    let now = Instant::now();
    let a = publication("a", 1);
    let b = publication("b", 2);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone(), b.clone()], now);
    viewer.set_joined(true, now);
    let (watch_a, _) = start_of(&viewer.select(&a, 1, now));
    let switching = viewer.select(&b, 2, now);
    let leave = sends(&switching, "stream.leave");
    let ack = viewer.on_server_event("stream.left", &leave[0], &json!({}), now);
    let (watch_b, _) = start_of(&ack);
    let watching = viewer.on_server_event("stream.watching", &watch_b, &json!({"stream": b}), now);
    assert!(watching.is_empty());
    // A's offer was never seen, so the ufrag is unknown; only the explicit
    // watch id can reject it.
    let stale_offer = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"watch_id": watch_a, "sdp":"v=0\r\na=ice-ufrag:unknown\r\n"}),
        now,
    );
    assert!(stale_offer.is_empty(), "old tagged offer: {stale_offer:?}");
    let stale_close = viewer.on_server_event("stream.webrtc_closed", &watch_a, &json!({}), now);
    assert!(
        stale_close.is_empty(),
        "old top-level request_id with empty data must still be rejected"
    );
    assert_eq!(
        viewer
            .watching()
            .unwrap()
            .publication
            .publisher_connection_id,
        "b"
    );
}

#[test]
fn an_untagged_close_right_after_the_new_watch_ack_is_ambiguous() {
    let now = Instant::now();
    let a = publication("a", 1);
    let b = publication("b", 2);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone(), b.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    let switching = viewer.select(&b, 2, now);
    let leave = sends(&switching, "stream.leave");
    let ack = viewer.on_server_event("stream.left", &leave[0], &json!({}), now);
    let (watch_b, _) = start_of(&ack);
    viewer.on_server_event("stream.watching", &watch_b, &json!({"stream": b}), now);
    // The pinned backend cannot tag this close. Ignoring the first one keeps a
    // late close from A from ending B; a compatible backend must send watch_id.
    let ambiguous = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"reason":"disconnected"}),
        now,
    );
    assert!(ambiguous.is_empty(), "ambiguous close: {ambiguous:?}");
    assert_eq!(
        viewer
            .watching()
            .unwrap()
            .publication
            .publisher_connection_id,
        "b"
    );
    // B's own offer attributes the next close to this attempt.
    let current = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp":"v=0\r\na=ice-ufrag:b\r\n"}),
        now,
    );
    assert!(!current.is_empty());
    let current_close = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"reason":"disconnected"}),
        now,
    );
    assert!(!current_close.is_empty());
}

#[test]
fn an_early_close_tagged_with_the_new_attempt_is_accepted() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    let (watch, _) = start_of(&viewer.select(&a, 1, now));
    let early = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"watch_id": watch, "reason":"viewer permission was revoked"}),
        now,
    );
    assert!(early.iter().any(
        |outcome| matches!(outcome, Outcome::Failed { message, .. } if message == "Трансляция завершена")
    ));
    assert!(viewer.watching().is_none());
}

#[test]
fn two_views_of_the_same_p2p_publisher_use_separate_watch_ids() {
    let now = Instant::now();
    let mut a = publication("same-publisher", 1);
    a.mode = "p2p".into();
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    let (watch_a, _) = start_of(&viewer.select(&a, 1, now));
    let switching = viewer.select(&a, 2, now);
    let leave = sends(&switching, "stream.leave");
    let ack = viewer.on_server_event("stream.left", &leave[0], &json!({}), now);
    let (watch_b, _) = start_of(&ack);
    assert_ne!(watch_a, watch_b);
    let offer = viewer.on_server_event(
        "stream.p2p_offer",
        "",
        &json!({"from_connection_id":"same-publisher","sdp":"v=0\r\na=ice-ufrag:b\r\n"}),
        now,
    );
    assert!(
        !offer.is_empty(),
        "B's own offer must start the new attempt"
    );
    let old_close = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"watch_id": watch_a, "reason":"disconnected"}),
        now,
    );
    assert!(
        old_close.is_empty(),
        "the first run's close must not end the repeat view"
    );
    assert!(viewer.watching().is_some());
    let current_close = viewer.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"watch_id": watch_b, "reason":"disconnected"}),
        now,
    );
    assert!(!current_close.is_empty(), "B's tagged close must apply");
}

#[test]
fn an_unknown_next_ice_generation_reaches_the_engine_buffer() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.set_joined(true, now);
    start_of(&viewer.select(&a, 1, now));
    let old = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp":"v=0\r\na=ice-ufrag:old\r\n"}),
        now,
    );
    assert!(!old.is_empty());
    let next = viewer.on_server_event(
        "stream.ice_candidate",
        "",
        &json!({"candidate":"candidate:1 1 udp 1 127.0.0.1 9 typ host ufrag new"}),
        now,
    );
    assert!(
        next.iter()
            .any(|outcome| matches!(outcome, Outcome::Ice(_))),
        "the first candidate of a restart must be kept for the engine: {next:?}"
    );
    let restarted = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp":"v=0\r\na=ice-ufrag:new\r\n"}),
        now,
    );
    assert!(!restarted.is_empty());
    let old_generation = viewer.on_server_event(
        "stream.ice_candidate",
        "",
        &json!({"candidate":"candidate:1 1 udp 1 127.0.0.1 9 typ host ufrag old"}),
        now,
    );
    assert!(
        old_generation.is_empty(),
        "after the new SDP the previous generation is retired"
    );
}

#[test]
fn transport_end_before_join_still_publishes_recovery_for_the_selection() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[a.clone()], now);
    viewer.select_intent(a.clone(), 7);
    let ended = viewer.transport_ended(now);
    assert!(ended.iter().any(|outcome| matches!(
        outcome,
        Outcome::Connected {
            selection: 7,
            connected: false
        }
    )));
    assert!(ended.iter().any(|outcome| matches!(
        outcome,
        Outcome::Status { selection: 7, text } if text.contains("Восстанавливаем")
    )));
    // The next socket must wait for its own snapshot even if voice.joined
    // arrives first.
    assert!(viewer.set_joined(true, now).is_empty());
    let fresh = snapshot(&mut viewer, &[a], now);
    assert!(
        fresh
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Start { selection: 7, .. }))
    );
}

#[test]
fn a_snapshot_that_arrives_before_ready_starts_after_the_join() {
    let now = Instant::now();
    let a = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    // The pinned backend enqueues the snapshot during socket registration,
    // before `ready`; the runtime reports the still-unjoined voice state on
    // every non-stream event that follows.
    let listed = snapshot(&mut viewer, &[a.clone()], now);
    assert!(
        matches!(listed.as_slice(), [Outcome::Catalog]),
        "the snapshot reaches the panel: {listed:?}"
    );
    let still_unjoined = viewer.set_joined(false, now);
    assert!(
        still_unjoined.is_empty(),
        "there is no transport to end before the first join: {still_unjoined:?}"
    );
    assert_eq!(
        viewer.catalog().list().len(),
        1,
        "the pre-join snapshot is the authoritative list of this socket"
    );

    viewer.select_intent(a.clone(), 4);
    let joined = viewer.set_joined(true, now);
    assert!(
        joined
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Start { selection: 4, .. })),
        "the retained snapshot must start the selection after voice.joined: {joined:?}"
    );
}
