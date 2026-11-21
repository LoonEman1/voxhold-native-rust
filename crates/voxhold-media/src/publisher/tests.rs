//! Headless tests of the publishing state machine: no WebRTC, no Windows and
//! no socket. Every decision is driven by the clock argument.

use super::*;
use crate::publisher::engine::PublishStats;
use crate::screen_capture::{ScreenSource, SourceKind};
use serde_json::json;
use std::time::{Duration, Instant};

fn source() -> ScreenSource {
    ScreenSource {
        id: 7,
        kind: SourceKind::Monitor,
        title: "Экран 1".into(),
        display_id: -1,
        width: 2560,
        height: 1440,
        monitor: None,
    }
}

fn request() -> PublishRequest {
    PublishRequest {
        source: source(),
        quality: Quality::default(),
        codec: Codec::Vp8,
        audio: false,
        audio_scope: crate::system_audio::AudioScope::Device,
    }
}

fn request_with_codec(codec: Codec) -> PublishRequest {
    PublishRequest { codec, ..request() }
}

fn allowed() -> Access {
    Access {
        joined: true,
        stream_permission: true,
        slots_available: true,
    }
}

fn publisher() -> Publisher {
    let mut publisher = Publisher::new(1, 2);
    publisher.set_connection(Some("me".into()));
    publisher.set_access(allowed());
    publisher
}

fn sends(outcomes: &[Outcome]) -> Vec<&Signal> {
    outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Send(signal) => Some(signal),
            _ => None,
        })
        .collect()
}

fn start_signal(outcomes: &[Outcome]) -> &Signal {
    sends(outcomes)
        .into_iter()
        .find(|signal| signal.kind == "stream.start")
        .expect("stream.start")
}

/// Picker → publish → engine ready → `stream.start` sent, `Publishing`.
fn publishing(publisher: &mut Publisher, now: Instant) -> Vec<Outcome> {
    publisher.begin_picker(1);
    let mut outcomes = publisher.publish(1, request(), false, now);
    outcomes.extend(publisher.engine_ready(now, request().audio, None));
    outcomes
}

#[test]
fn the_picker_starts_no_capture_and_a_cancel_keeps_the_viewer() {
    let mut publisher = publisher();
    let opened = publisher.begin_picker(1);
    assert_eq!(publisher.phase(), Phase::Picking);
    assert!(matches!(opened.as_slice(), [Outcome::State(_)]));
    assert!(
        sends(&opened).is_empty(),
        "opening the picker sends nothing"
    );
    let cancelled = publisher.cancel_picker(1);
    assert_eq!(publisher.phase(), Phase::Idle);
    assert!(
        sends(&cancelled).is_empty(),
        "cancel sends no stream.start and does not touch the viewer"
    );
    assert!(matches!(cancelled.as_slice(), [Outcome::State(_)]));
}

#[test]
fn a_publish_without_a_confirmed_join_or_stream_permission_never_starts() {
    let now = Instant::now();
    for access in [
        Access {
            joined: false,
            stream_permission: true,
            slots_available: true,
        },
        Access {
            joined: true,
            stream_permission: false,
            slots_available: true,
        },
        Access {
            joined: true,
            stream_permission: true,
            slots_available: false,
        },
    ] {
        let mut publisher = Publisher::new(1, 2);
        publisher.set_access(access);
        publisher.begin_picker(1);
        let outcomes = publisher.publish(1, request(), false, now);
        assert_eq!(publisher.phase(), Phase::Failed, "{access:?}");
        assert!(
            sends(&outcomes).is_empty(),
            "no stream.start without access: {outcomes:?}"
        );
        assert!(
            !outcomes
                .iter()
                .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
            "no capture without access: {outcomes:?}"
        );
        let Sharing { error, .. } = publisher.sharing();
        assert!(error.is_some(), "the reason reaches the UI");
    }
}

#[test]
fn the_offer_is_answered_before_stream_started_arrives() {
    let now = Instant::now();
    let mut publisher = publisher();
    let outcomes = publishing(&mut publisher, now);
    assert!(matches!(outcomes[0], Outcome::StartEngine { .. }));
    let start = start_signal(&outcomes);
    let start_id = start.request_id.clone().expect("tagged start");
    assert_eq!(start.data["mode"], "server");
    assert_eq!(start.data["codec"], "vp8");
    assert_eq!(start.data["has_audio"], false);
    assert!(start.data.get("renditions").is_none());

    // The backend queues the offer before stream.started.
    let offer = publisher.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp": "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:0\r\na=recvonly\r\n"}),
        now,
    );
    assert!(
        matches!(offer.as_slice(), [Outcome::Offer(sdp)] if sdp.contains("m=video")),
        "{offer:?}"
    );
    let answer = publisher.on_engine(EngineEvent::Answer("v=0 answer".into()), now);
    let sent = sends(&answer);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].kind, "stream.webrtc_answer");
    assert_eq!(sent[0].data["sdp"], "v=0 answer");
    assert!(
        sent[0]
            .request_id
            .as_deref()
            .is_some_and(|id| id.starts_with("native-screen-")),
        "the answer is tagged for the error mapping"
    );

    // The started event confirms our connection and leaves the attempt alive.
    let started = publisher.on_server_event(
        "stream.started",
        start_id.as_str(),
        &json!({
            "server_id": 1,
            "channel_id": 2,
            "publisher_connection_id": "me",
            "codec": "vp8",
            "mode": "server",
        }),
        now,
    );
    assert!(matches!(started.as_slice(), [Outcome::State(_)]));
    assert_eq!(publisher.phase(), Phase::Publishing);
}

#[test]
fn stream_start_carries_the_exact_codec() {
    let now = Instant::now();
    for codec in Codec::ALL {
        let mut publisher = publisher();
        publisher.begin_picker(1);
        let started = publisher.publish(1, request_with_codec(codec), false, now);
        assert!(
            matches!(started.first(), Some(Outcome::StartEngine { .. })),
            "the capture must start for {codec:?}: {started:?}"
        );
        let outcomes = publisher.engine_ready(now, false, None);
        let sent = sends(&outcomes);
        assert_eq!(sent.len(), 1, "one stream.start for {codec:?}");
        assert_eq!(sent[0].kind, "stream.start");
        assert_eq!(
            sent[0].data["codec"],
            codec.wire(),
            "the wire value must be exact for {codec:?}"
        );
        assert_eq!(publisher.phase(), Phase::Publishing);
    }
}

#[test]
fn live_needs_both_the_connection_and_a_sent_frame() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    let connected = publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    assert!(connected.is_empty());
    assert_eq!(publisher.phase(), Phase::Publishing);
    let frame = publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Live);
    assert!(publisher.sharing().live);
    assert!(matches!(
        frame.as_slice(),
        [Outcome::State(Sharing { live: true, .. })]
    ));
    let stats = publisher.on_engine(
        EngineEvent::Stats(PublishStats {
            fps: 29.5,
            dropped: 3,
            width: 1280,
            height: 720,
            sources: 1,
            limitation: crate::publisher::engine::QualityLimitation::None,
            ..PublishStats::default()
        }),
        now,
    );
    assert!(matches!(
        stats.as_slice(),
        [Outcome::State(Sharing { stats: Some(_), .. })]
    ));
}

#[test]
fn a_viewer_is_left_before_the_capture_starts() {
    let now = Instant::now();
    let mut publisher = publisher();
    publisher.begin_picker(1);
    let waiting = publisher.publish(1, request(), true, now);
    assert!(matches!(waiting[0], Outcome::LeaveViewer));
    assert!(
        !waiting
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "the capture waits for the viewer"
    );

    // `stream.left` arrives well before the five-second contract.
    let started = publisher.viewer_left(now + Duration::from_millis(200));
    assert!(matches!(started[0], Outcome::StartEngine { .. }));
    let ready = publisher.engine_ready(now + Duration::from_millis(200), false, None);
    assert_eq!(start_signal(&ready).kind, "stream.start");
}

#[test]
fn a_missing_stream_left_still_starts_after_the_five_second_contract() {
    let now = Instant::now();
    let mut publisher = publisher();
    publisher.begin_picker(1);
    assert!(matches!(
        publisher.publish(1, request(), true, now)[0],
        Outcome::LeaveViewer
    ));
    assert!(
        publisher
            .tick(now + LEAVE_TIMEOUT - Duration::from_millis(1))
            .is_empty(),
        "the state machine must not start early"
    );
    let started = publisher.tick(now + LEAVE_TIMEOUT);
    assert!(matches!(started[0], Outcome::StartEngine { .. }));
    let ready = publisher.engine_ready(now + LEAVE_TIMEOUT, false, None);
    assert_eq!(start_signal(&ready).kind, "stream.start");
}

#[test]
fn a_late_picker_result_after_leave_never_starts_a_capture() {
    let now = Instant::now();
    let mut publisher = publisher();
    publisher.begin_picker(4);
    // The user leaves the call before the picker returns.
    let abandoned = publisher.abandon();
    assert!(matches!(abandoned.as_slice(), [Outcome::State(_)]));
    assert_eq!(publisher.phase(), Phase::Idle);

    let late = publisher.publish(4, request(), false, now);
    assert!(
        late.is_empty(),
        "a result of an ended session must not start: {late:?}"
    );
    assert!(
        publisher.begin_picker(4).is_empty(),
        "the old picker id is stale"
    );
    assert!(
        publisher.begin_picker(5).len() == 1,
        "a new picker is allowed"
    );
}

#[test]
fn leaving_while_publishing_releases_the_capture() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    let abandoned = publisher.abandon();
    assert!(
        abandoned.contains(&Outcome::StopEngine),
        "the capture is released immediately: {abandoned:?}"
    );
    assert_eq!(publisher.phase(), Phase::Idle);
    assert!(
        sends(&abandoned).is_empty(),
        "an abandoned session sends no start or answer"
    );
}

#[test]
fn a_foreign_stream_stopped_does_not_end_our_publication() {
    let now = Instant::now();
    let mut publisher = publisher();
    let outcomes = publishing(&mut publisher, now);
    let start_id = start_signal(&outcomes).request_id.clone().unwrap();
    publisher.on_server_event(
        "stream.started",
        &start_id,
        &json!({
            "server_id": 1,
            "channel_id": 2,
            "publisher_connection_id": "me",
        }),
        now,
    );
    let foreign = publisher.on_server_event(
        "stream.stopped",
        "",
        &json!({"server_id": 1, "channel_id": 2, "publisher_connection_id": "other"}),
        now,
    );
    assert!(foreign.is_empty(), "another publisher's stop is not ours");
    assert_eq!(publisher.phase(), Phase::Publishing);

    let ours = publisher.on_server_event(
        "stream.stopped",
        "",
        &json!({
            "server_id": 1,
            "channel_id": 2,
            "publisher_connection_id": "me",
            "reason": "stream permission was revoked",
        }),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(ours.contains(&Outcome::StopEngine));
    assert!(
        publisher
            .sharing()
            .error
            .is_some_and(|error| error.contains("Доступ"))
    );
}

#[test]
fn one_stop_intent_sends_exactly_one_stream_stop_and_ignores_late_signaling() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    let stop = publisher.stop(now);
    let first = sends(&stop);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, "stream.stop");
    assert!(stop.contains(&Outcome::StopEngine));
    assert_eq!(publisher.phase(), Phase::Stopping);

    // A repeated click must not send a second stop.
    let repeated = publisher.stop(now);
    assert!(sends(&repeated).is_empty());

    // A late answer or offer after the stop is ignored.
    assert!(
        publisher
            .on_engine(EngineEvent::Answer("late answer".into()), now)
            .is_empty()
    );
    assert!(
        publisher
            .on_server_event(
                "stream.webrtc_offer",
                "",
                &json!({"sdp": "late offer"}),
                now,
            )
            .is_empty()
    );

    let stopped = publisher.on_server_event(
        "stream.stopped",
        "",
        &json!({"server_id": 1, "channel_id": 2, "publisher_connection_id": "me"}),
        now + Duration::from_millis(120),
    );
    assert_eq!(publisher.phase(), Phase::Idle);
    // `stop` already emitted StopEngine; the acknowledgement only finishes.
    assert!(matches!(stopped.as_slice(), [Outcome::State(_)]));
}

#[test]
fn a_stop_ack_that_never_arrives_finishes_after_the_bounded_timeout() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    publisher.stop(now);
    assert!(!publisher.tick(now + STOP_TIMEOUT).is_empty());
    assert_eq!(publisher.phase(), Phase::Idle);
}

#[test]
fn a_stop_before_stream_start_sends_nothing_to_the_server() {
    let now = Instant::now();
    let mut publisher = publisher();
    publisher.begin_picker(1);
    // The publish is waiting for a viewer when the user changes their mind.
    publisher.publish(1, request(), true, now);
    let stop = publisher.stop(now);
    assert!(
        sends(&stop).is_empty(),
        "stream.start was never sent, so stream.stop must not be: {stop:?}"
    );
    assert_eq!(publisher.phase(), Phase::Idle);
}

#[test]
fn server_errors_become_terminal_messages_with_teardown() {
    let now = Instant::now();
    let cases = [
        (
            json!({"code": "invalid_state", "message": "join the same voice channel first"}),
            "голосовому",
        ),
        (
            json!({"code": "invalid_state", "message": "channel stream publisher limit reached"}),
            "8 трансляций",
        ),
        (
            json!({"code": "invalid_state", "message": "stream is already active"}),
            "уже запущена",
        ),
        (json!({"code": "forbidden"}), "Нет права"),
        (json!({"code": "internal_error"}), "не смог"),
    ];
    for (data, needle) in cases {
        let mut publisher = publisher();
        let outcomes = publishing(&mut publisher, now);
        let start_id = start_signal(&outcomes).request_id.clone().unwrap();
        let failed = publisher.on_server_event("error", &start_id, &data, now);
        assert_eq!(publisher.phase(), Phase::Failed, "{data:?}");
        assert!(
            failed.contains(&Outcome::StopEngine),
            "the capture is released: {failed:?}"
        );
        let Sharing { error, .. } = publisher.sharing();
        let error = error.expect("terminal message");
        assert!(
            error.contains(needle),
            "{error:?} does not contain {needle:?}"
        );
    }
}

#[test]
fn an_untagged_error_of_another_request_cannot_end_the_attempt() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    let foreign = publisher.on_server_event(
        "error",
        "native-stream-99",
        &json!({"code": "forbidden"}),
        now,
    );
    assert!(foreign.is_empty(), "not our request id");
    assert_eq!(publisher.phase(), Phase::Publishing);
}

#[test]
fn a_webrtc_closed_event_is_terminal_and_releases_the_capture() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    let closed = publisher.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"reason": "stream bitrate limit exceeded"}),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(closed.contains(&Outcome::StopEngine));
    assert!(
        publisher
            .sharing()
            .error
            .is_some_and(|error| error.contains("битрейт"))
    );

    // The engine can no longer change the state.
    assert!(
        publisher
            .on_engine(
                EngineEvent::State(crate::publisher::Connection::Connected),
                now
            )
            .is_empty()
    );
}

#[test]
fn the_answer_deadline_fails_a_silent_server() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    assert!(publisher.tick(now + ANSWER_TIMEOUT).len() >= 2);
    assert_eq!(publisher.phase(), Phase::Failed);
}

#[test]
fn ice_from_the_server_reaches_the_engine_only_while_publishing() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    let outcomes = publisher.on_server_event(
        "stream.ice_candidate",
        "",
        &json!({
            "candidate": "candidate:1 1 udp 2130706431 192.0.2.1 5000 typ host",
            "sdp_mid": "0",
            "sdp_mline_index": 0,
            "username_fragment": "abcd",
        }),
        now,
    );
    assert!(matches!(
        outcomes.as_slice(),
        [Outcome::Ice(candidate)] if candidate.sdp_mid.as_deref() == Some("0")
    ));
    // An empty candidate is a no-op, not an error.
    assert!(
        publisher
            .on_server_event("stream.ice_candidate", "", &json!({"candidate": ""}), now)
            .is_empty()
    );
}

mod answer;
mod bitrate;
mod quality;

#[test]
fn the_ui_snapshot_follows_every_phase() {
    let now = Instant::now();
    let mut publisher = publisher();
    assert_eq!(publisher.sharing().phase, Phase::Idle);
    publisher.begin_picker(1);
    assert_eq!(publisher.sharing().phase, Phase::Picking);
    publisher.publish(1, request(), false, now);
    publisher.engine_ready(now, false, None);
    assert_eq!(publisher.sharing().phase, Phase::Publishing);
    assert_eq!(
        publisher.sharing().quality.map(Quality::describe),
        Some(Quality::default().describe())
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.sharing().phase, Phase::Live);
    publisher.stop(now);
    assert_eq!(publisher.sharing().phase, Phase::Stopping);
    publisher.tick(now + STOP_TIMEOUT);
    assert_eq!(publisher.sharing().phase, Phase::Idle);
}

#[test]
fn a_new_picker_after_a_failure_sends_its_own_stream_stop() {
    let now = Instant::now();
    let mut publisher = publisher();
    let first = publishing(&mut publisher, now);
    let start_id = start_signal(&first).request_id.clone().expect("start id");
    // The server rejects the first publication after `stream.start`; the
    // failed attempt already sent its own stop.
    let failed = publisher.on_server_event("error", &start_id, &json!({"code": "forbidden"}), now);
    assert_eq!(publisher.phase(), Phase::Failed);
    assert_eq!(
        sends(&failed)
            .iter()
            .filter(|signal| signal.kind == "stream.stop")
            .count(),
        1,
        "the failed attempt stops its own server publication"
    );

    // A new picker after the failure is a clean attempt: it must not inherit
    // the `stop_sent` latch of the previous one, or its stop is never sent.
    let second_now = now + Duration::from_secs(1);
    let opened = publisher.begin_picker(2);
    assert_eq!(publisher.phase(), Phase::Picking);
    assert!(matches!(opened.as_slice(), [Outcome::State(_)]));
    let mut publish = publisher.publish(2, request(), false, second_now);
    publish.extend(publisher.engine_ready(second_now, false, None));
    assert!(
        sends(&publish)
            .iter()
            .all(|signal| signal.kind != "stream.stop"),
        "a fresh publish sends no stop: {publish:?}"
    );
    assert_eq!(
        sends(&publish)
            .iter()
            .filter(|signal| signal.kind == "stream.start")
            .count(),
        1
    );

    let stop = publisher.stop(second_now);
    assert_eq!(
        sends(&stop)
            .iter()
            .filter(|signal| signal.kind == "stream.stop")
            .count(),
        1,
        "the second attempt stops its own publication: {stop:?}"
    );
    assert_eq!(publisher.phase(), Phase::Stopping);
}

#[test]
fn a_new_attempt_after_live_needs_its_own_connection_and_frame() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Live);

    // The server closes the live attempt; a new attempt follows.
    publisher.on_server_event("stream.webrtc_closed", "", &json!({"reason": "test"}), now);
    assert_eq!(publisher.phase(), Phase::Failed);

    let second_now = now + Duration::from_secs(1);
    publisher.begin_picker(2);
    let mut publish = publisher.publish(2, request(), false, second_now);
    publish.extend(publisher.engine_ready(second_now, false, None));
    assert_eq!(
        publisher.phase(),
        Phase::Publishing,
        "a new attempt starts unpublished"
    );
    let connected = publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        second_now,
    );
    assert_eq!(
        publisher.phase(),
        Phase::Publishing,
        "the stale Connected of the previous attempt must not turn it Live"
    );
    assert!(!publisher.sharing().live);
    assert!(connected.is_empty());
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        second_now,
    );
    assert_eq!(
        publisher.phase(),
        Phase::Live,
        "both events of this attempt"
    );
}

#[test]
fn stream_start_waits_for_the_engine_and_a_failed_start_sends_nothing() {
    let now = Instant::now();
    let mut publisher = publisher();
    publisher.begin_picker(1);
    let publish = publisher.publish(1, request(), false, now);
    assert!(
        matches!(publish.as_slice(), [Outcome::StartEngine { .. }]),
        "the start decision only asks for the engine: {publish:?}"
    );
    assert!(
        sends(&publish).is_empty(),
        "no stream.start before the engine exists: {publish:?}"
    );

    // The engine could not be created: the attempt is cancelled whole.
    let failed = publisher.on_start_failed("Медиадвижок не создан.", now);
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(
        sends(&failed).is_empty(),
        "no phantom stream.start and no stream.stop: {failed:?}"
    );
    assert!(
        !failed.contains(&Outcome::StopEngine),
        "there is no engine to stop: {failed:?}"
    );
    assert!(
        publisher.sharing().error.is_some(),
        "the UI gets the reason"
    );
    assert!(
        publisher.tick(now + ANSWER_TIMEOUT).is_empty(),
        "the failed start leaves no deadline"
    );

    // Late signaling of the cancelled attempt is ignored.
    assert!(
        publisher
            .on_server_event(
                "stream.started",
                "",
                &json!({"channel_id": 2, "publisher_connection_id": "me"}),
                now,
            )
            .is_empty()
    );
    assert!(
        publisher
            .on_server_event("stream.webrtc_offer", "", &json!({"sdp": "late"}), now)
            .is_empty()
    );
    assert!(
        publisher
            .on_engine(EngineEvent::Answer("late answer".into()), now)
            .is_empty()
    );
}

#[test]
fn stream_left_with_our_stop_request_finishes_without_the_timeout() {
    for kind in ["stream.stopped", "stream.left"] {
        let now = Instant::now();
        let mut publisher = publisher();
        publishing(&mut publisher, now);
        let stop = publisher.stop(now);
        let stop_id = sends(&stop)
            .iter()
            .find(|signal| signal.kind == "stream.stop")
            .and_then(|signal| signal.request_id.clone())
            .expect("tagged stop");
        let done = publisher.on_server_event(kind, &stop_id, &json!({}), now);
        assert_eq!(publisher.phase(), Phase::Idle, "{kind}");
        assert!(matches!(done.as_slice(), [Outcome::State(_)]), "{kind}");
    }
}

#[test]
fn an_untagged_stream_left_does_not_end_our_stop_wait() {
    let now = Instant::now();
    let mut publisher = publisher();
    publishing(&mut publisher, now);
    publisher.stop(now);
    // This is the viewer's leave, not an acknowledgement of our stop.
    assert!(
        publisher
            .on_server_event("stream.left", "", &json!({}), now)
            .is_empty()
    );
    assert_eq!(publisher.phase(), Phase::Stopping);
    assert!(!publisher.tick(now + STOP_TIMEOUT).is_empty());
    assert_eq!(publisher.phase(), Phase::Idle);
}

#[test]
fn begin_picker_resets_the_previous_attempt_snapshot_immediately() {
    let now = Instant::now();
    let mut publisher = publisher();
    let outcomes = publishing(&mut publisher, now);
    let start_id = start_signal(&outcomes)
        .request_id
        .clone()
        .expect("start id");

    // A live attempt with measured counters, then a terminal server error.
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    let stats = PublishStats {
        fps: 30.0,
        dropped: 2,
        // Audio fields are not part of this snapshot test.
        width: 1280,
        height: 720,
        sources: 1,
        limitation: crate::publisher::engine::QualityLimitation::None,
        ..PublishStats::default()
    };
    publisher.on_engine(EngineEvent::Stats(stats), now);
    assert_eq!(publisher.sharing().stats, Some(stats));
    let failed = publisher.on_server_event("error", &start_id, &json!({"code": "forbidden"}), now);
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(
        sends(&failed)
            .iter()
            .any(|signal| signal.kind == "stream.stop"),
        "the failed attempt stops its own publication"
    );
    // The previous attempt leaves the confirmation latches, the error and (as
    // a late engine sample could) the stats behind. Seeding the stats directly
    // also proves that `begin_picker` resets the snapshot itself instead of
    // relying on the duplicate cleanup in `start()`.
    assert!(publisher.start_sent && publisher.stop_sent);
    assert!(publisher.sharing().error.is_some());
    publisher.stats = Some(stats);

    let opened = publisher.begin_picker(2);
    assert!(
        matches!(opened.as_slice(), [Outcome::State(_)]),
        "{opened:?}"
    );

    // This is the state the panel reads right after the picker was opened. It
    // must not inherit anything from the failed attempt.
    let snapshot = publisher.sharing();
    assert_eq!(snapshot.phase, Phase::Picking);
    assert_eq!(snapshot.attempt, 2);
    assert!(!snapshot.live);
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.stats, None);
    assert_eq!(snapshot.quality, None);
    assert_eq!(snapshot.codec, None);
    assert!(!publisher.start_sent, "start_sent must be reset");
    assert!(!publisher.stop_sent, "stop_sent must be reset");
    assert!(publisher.request.is_none(), "the old request must be gone");
    assert!(
        publisher.started.is_none(),
        "the old publisher id must be gone"
    );
    assert!(
        publisher.deadline.is_none() && publisher.stop_deadline.is_none(),
        "the old deadlines must be gone"
    );
    assert_eq!(publisher.picker, Some(2), "the new picker owns the attempt");
}

#[test]
fn a_cancelled_start_never_opens_a_capture() {
    use crate::publisher::engine::PublisherEngine;
    use crate::screen_capture::PreviewStore;
    use std::sync::{Arc, atomic::AtomicBool};

    // The preparation was cancelled between StartEngine and the first stage.
    // The engine returns before touching the capture, the loopback or a peer,
    // so this runs without a desktop.
    let cancelled = AtomicBool::new(true);
    let error = match PublisherEngine::start(
        request(),
        Vec::new(),
        "all".into(),
        Arc::new(PreviewStore::default()),
        Arc::new(crate::screen_capture::PreviewDemand::default()),
        &cancelled,
    ) {
        Ok(_) => panic!("a cancelled start must not open a device"),
        Err(error) => error,
    };
    assert!(error.contains("отменена"), "{error}");
}
