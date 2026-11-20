//! Quality ranges, web presets and the confirmed stop+start of a live change.
//! Pure state-machine tests: no WebRTC, no device, no socket.

use super::*;
use serde_json::json;
use std::time::Instant;

#[test]
fn quality_values_are_clamped_to_the_web_ranges() {
    assert_eq!(
        Quality {
            resolution: Resolution::P720,
            fps: 144,
            bitrate_kbps: 999,
            audio_bitrate_kbps: 32,
        }
        .clamped()
        .fps,
        60
    );
    assert_eq!(
        Quality {
            resolution: Resolution::P720,
            fps: 15,
            bitrate_kbps: 99,
            audio_bitrate_kbps: 32,
        }
        .clamped(),
        Quality {
            resolution: Resolution::P720,
            fps: 30,
            bitrate_kbps: 500,
            audio_bitrate_kbps: 64,
        }
    );
    assert_eq!(
        Quality {
            resolution: Resolution::P1080,
            fps: 30,
            bitrate_kbps: 20_001,
            audio_bitrate_kbps: 510,
        }
        .clamped()
        .bitrate_kbps,
        16_000
    );
    assert_eq!(
        Quality {
            resolution: Resolution::P1080,
            fps: 30,
            bitrate_kbps: 6000,
            audio_bitrate_kbps: 510,
        }
        .clamped()
        .audio_bitrate_kbps,
        320,
        "the client never exceeds the backend's 320 kbit/s"
    );
    for requested in [0, 32, 64, 80, 128, 255, 320, 510, 900] {
        let applied = Quality {
            audio_bitrate_kbps: requested,
            ..Quality::default()
        }
        .clamped()
        .audio_bitrate_kbps;
        assert!(
            (32..=510).contains(&applied),
            "the backend accepts 32–510 kbit/s, got {applied}"
        );
        assert!(
            AUDIO_BITRATES.contains(&applied),
            "the UI offers only its six values, got {applied}"
        );
    }
    assert_eq!(
        Quality::default().describe(),
        "1920 × 1080 · 30 FPS · 6,0 Мбит/с"
    );
    assert_eq!(Quality::default().audio_bitrate_kbps, 128);
}

#[test]
fn the_web_presets_change_only_the_frame_rate_and_video_bitrate() {
    let base = Quality {
        resolution: Resolution::P720,
        audio_bitrate_kbps: 256,
        ..Quality::default()
    };
    let desktop = Preset::Desktop.apply(base);
    assert_eq!((desktop.fps, desktop.bitrate_kbps), (30, 6000));
    assert_eq!(desktop.resolution, Resolution::P720, "resolution stays");
    assert_eq!(desktop.audio_bitrate_kbps, 256, "audio stays");
    let games = Preset::Games.apply(base);
    assert_eq!((games.fps, games.bitrate_kbps), (60, 12_000));
    let films = Preset::Films.apply(base);
    assert_eq!((films.fps, films.bitrate_kbps), (30, 8000));
    assert_eq!(Preset::detect(desktop), Preset::Desktop);
    assert_eq!(Preset::detect(games), Preset::Games);
    assert_eq!(Preset::detect(films), Preset::Films);
    assert_eq!(
        Preset::detect(Quality {
            fps: 30,
            bitrate_kbps: 4300,
            ..base
        }),
        Preset::Custom
    );
    assert_eq!(
        Preset::Custom.apply(base),
        base,
        "the custom tile never changes a value"
    );
}

#[test]
fn a_live_quality_change_is_one_confirmed_stop_and_one_new_start() {
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
    let attempt = publisher.attempt;
    assert_eq!(publisher.phase(), Phase::Live);

    let changed = Quality {
        resolution: Resolution::P1080,
        fps: 60,
        bitrate_kbps: 9000,
        audio_bitrate_kbps: 256,
    };
    let restart = publisher.restart(
        PublishRequest {
            quality: changed,
            audio: true,
            ..request()
        },
        now,
    );
    assert_eq!(publisher.phase(), Phase::Stopping);
    assert!(
        restart
            .iter()
            .any(|outcome| *outcome == Outcome::StopEngine),
        "the old capture and peer are released: {restart:?}"
    );
    assert_eq!(
        sends(&restart)
            .iter()
            .filter(|signal| signal.kind == "stream.stop")
            .count(),
        1,
        "exactly one stop: {restart:?}"
    );
    assert!(
        sends(&restart)
            .iter()
            .all(|signal| signal.kind != "stream.start"),
        "no second start before the stop ack: {restart:?}"
    );
    assert!(publisher.sharing().restarting);
    assert!(publisher.sharing().status.contains("Перезапуск"));
    // A restart outside Live has no meaning and must not queue anything.
    let mut idle = Publisher::new(1, 2);
    assert!(
        idle.restart(
            PublishRequest {
                quality: changed,
                ..request()
            },
            now
        )
        .is_empty()
    );

    // The backend acknowledges the stop; only now does the new attempt begin.
    let ack = publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let starts: Vec<_> = ack
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::StartEngine { attempt, request } => Some((*attempt, request.quality)),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec![(attempt + 1, changed)]);
    assert_eq!(publisher.phase(), Phase::Publishing);
    assert!(!publisher.sharing().restarting);
    assert!(
        sends(&ack).is_empty(),
        "stream.start waits for the new engine: {ack:?}"
    );

    // The pinned backend broadcasts `stream.stopped` and also answers the
    // request; both copies belong to the closed attempt. The duplicate must
    // not fail the fresh one.
    let echo = publisher.on_server_event(
        "stream.stopped",
        "",
        &json!({
            "channel_id": 2,
            "publisher_connection_id": "me",
            "reason": "stream stopped by user",
        }),
        now,
    );
    assert!(
        echo.is_empty(),
        "a stale stop echo kills the restart: {echo:?}"
    );
    assert_eq!(publisher.phase(), Phase::Publishing);

    // Closing the old media session produces `stream.webrtc_closed` after the
    // stop acknowledgement too (observed on the live stand). It is not a
    // failure of the new attempt.
    let old_peer = publisher.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"reason": "stream WebRTC connection closed"}),
        now,
    );
    assert!(
        old_peer.is_empty(),
        "a stale closed event kills the restart: {old_peer:?}"
    );
    assert_eq!(publisher.phase(), Phase::Publishing);

    let ready = publisher.engine_ready(now, true, Some("Только звук demo.exe (PID 42)".into()));
    let start = start_signal(&ready);
    assert_eq!(start.data["has_audio"], json!(true));
    assert_eq!(start.data["codec"], json!("vp8"));
    assert_eq!(
        sends(&ready)
            .iter()
            .filter(|signal| signal.kind == "stream.start")
            .count(),
        1,
        "exactly one new start: {ready:?}"
    );
    assert!(publisher.sharing().has_audio);
    assert_eq!(
        publisher.sharing().audio_scope.as_deref(),
        Some("Только звук demo.exe (PID 42)")
    );
    assert!(
        start.data.get("audio_scope").is_none(),
        "the backend contract has no scope field; it stays local: {:?}",
        start.data
    );
    assert_eq!(publisher.sharing().attempt, attempt + 1);

    // The stale-close filter is narrow: a real revocation still ends the
    // publication with an explicit message.
    let revoked = publisher.on_server_event(
        "stream.webrtc_closed",
        "",
        &json!({"reason": "stream permission was revoked"}),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(
        publisher
            .sharing()
            .error
            .as_deref()
            .is_some_and(|error| error.contains("отозван")),
        "the revocation reason must reach the UI: {revoked:?}"
    );
}

/// A poster of the live attempt: both confirmations that turn it Live.
fn live(publisher: &mut Publisher, now: Instant) {
    publishing(publisher, now);
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.phase(), Phase::Live);
}

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

    let outcomes = publisher.apply_quality(next.clone(), now);
    assert_eq!(
        outcomes,
        vec![
            Outcome::SetBitrate {
                attempt,
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
    assert_eq!(publisher.sharing().quality, Some(next.quality));
    assert_eq!(publisher.request(), Some(&next));
    assert!(
        publisher.sharing().status.contains("Битрейт"),
        "the card can say the bitrate changed: {:?}",
        publisher.sharing().status
    );
    assert!(
        !outcomes.iter().any(|outcome| matches!(
            outcome,
            Outcome::Send(_) | Outcome::StartEngine { .. } | Outcome::StopEngine
        )),
        "no stop and no start on a bitrate-only change: {outcomes:?}"
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
    let applied = audio_publisher.apply_quality(louder_audio.clone(), now);
    assert!(matches!(
        applied.first(),
        Some(Outcome::SetBitrate { quality, .. }) if *quality == louder_audio.quality
    ));
    assert_eq!(audio_publisher.phase(), Phase::Live);
}

#[test]
fn resolution_frame_rate_or_audio_scope_change_keeps_the_restart_path() {
    let now = Instant::now();
    let live_request = request();
    let variants = [
        PublishRequest {
            quality: Quality {
                resolution: Resolution::P720,
                ..Quality::default()
            },
            ..live_request.clone()
        },
        PublishRequest {
            quality: Quality {
                fps: 60,
                ..Quality::default()
            },
            ..live_request.clone()
        },
        PublishRequest {
            audio: true,
            ..live_request.clone()
        },
        PublishRequest {
            audio_scope: crate::system_audio::AudioScope::EverythingExceptSelf,
            ..live_request.clone()
        },
        PublishRequest {
            source: ScreenSource { id: 9, ..source() },
            ..live_request.clone()
        },
    ];
    for next in variants {
        assert_eq!(
            live_request.change_to(&next),
            QualityChange::Restart,
            "{next:?}"
        );
        let mut publisher = publisher();
        live(&mut publisher, now);
        assert!(
            publisher.apply_quality(next.clone(), now).is_empty(),
            "a live apply must refuse a change that needs a new capture: {next:?}"
        );
        assert_eq!(publisher.phase(), Phase::Live);
        assert_eq!(publisher.request(), Some(&live_request));
    }
    assert_eq!(
        live_request.change_to(&live_request),
        QualityChange::Unchanged
    );
}

#[test]
fn a_failed_or_cancelled_preflight_keeps_the_live_attempt_and_sends_nothing() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let attempt = publisher.attempt;

    let started = publisher.preflight_started();
    assert!(matches!(started.as_slice(), [Outcome::State(_)]));
    assert!(publisher.sharing().preflighting);
    assert_eq!(publisher.phase(), Phase::Live);
    assert!(!publisher.sharing().restarting);

    let failed = publisher.preflight_failed("Новая настройка не открылась.");
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().attempt, attempt);
    assert!(!publisher.sharing().preflighting);
    assert!(publisher.sharing().error.is_some());
    assert!(
        !failed.iter().any(|outcome| matches!(
            outcome,
            Outcome::Send(_) | Outcome::StartEngine { .. } | Outcome::StopEngine
        )),
        "a failed preflight must not touch the live stream: {failed:?}"
    );

    // The user closing the dialog cancels the preflight and clears the error.
    publisher.preflight_started();
    let cancelled = publisher.preflight_cancelled(None);
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.sharing().error, None);
    assert_eq!(publisher.sharing().status, "В эфире");
    assert!(matches!(cancelled.as_slice(), [Outcome::State(_)]));
}

#[test]
fn a_local_failure_after_the_stop_restores_the_previous_request_once() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let previous = publisher.request().cloned().expect("live request");
    let attempt = publisher.attempt;

    let changed = PublishRequest {
        quality: Quality {
            resolution: Resolution::P720,
            fps: 60,
            bitrate_kbps: 9000,
            audio_bitrate_kbps: 128,
        },
        ..request()
    };
    publisher.restart(changed.clone(), now);
    let ack = publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let (new_attempt, new_request) = ack
        .iter()
        .find_map(|outcome| match outcome {
            Outcome::StartEngine { attempt, request } => Some((*attempt, request.clone())),
            _ => None,
        })
        .expect("the new attempt starts after the ack");
    assert_eq!(new_request.quality, changed.quality);
    assert_eq!(new_attempt, attempt + 1);

    // The new capture fails locally after the old stream was stopped.
    let restored = publisher.on_start_failed("Захват не открылся.", now);
    let (attempt, request) = restored
        .iter()
        .find_map(|outcome| match outcome {
            Outcome::StartEngine { attempt, request } => Some((*attempt, request.clone())),
            _ => None,
        })
        .expect("the previous request is restored once");
    assert_eq!(request, previous, "the live settings come back");
    assert_eq!(attempt, new_attempt + 1);
    assert_eq!(publisher.phase(), Phase::Publishing);
    assert_eq!(publisher.sharing().quality, Some(previous.quality));
    assert!(
        !restored
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Send(_))),
        "stream.start was never sent, so the restore sends nothing: {restored:?}"
    );

    // A second local failure is terminal: no second restore, no loop.
    let terminal = publisher.on_start_failed("Захват не открылся снова.", now);
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(
        !terminal
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "only one automatic restore is allowed: {terminal:?}"
    );
    assert!(publisher.sharing().error.is_some());
}

#[test]
fn a_restore_after_a_sent_start_waits_for_the_stop_acknowledgement() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    let previous = publisher.request().cloned().expect("live request");
    let changed = PublishRequest {
        quality: Quality {
            resolution: Resolution::P720,
            ..Quality::default()
        },
        ..request()
    };
    publisher.restart(changed.clone(), now);
    publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let ready = publisher.engine_ready(now, false, None);
    assert_eq!(
        sends(&ready)
            .iter()
            .filter(|signal| signal.kind == "stream.start")
            .count(),
        1
    );

    // The new peer dies locally after `stream.start` reached the server.
    let failed = publisher.on_engine(EngineEvent::Error("Соединение потеряно.".into()), now);
    let kinds: Vec<&str> = sends(&failed).iter().map(|signal| signal.kind).collect();
    assert_eq!(kinds, vec!["stream.stop"], "{failed:?}");
    assert!(failed.contains(&Outcome::StopEngine));
    assert!(
        !failed
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "the previous attempt must not start before the stop ack: {failed:?}"
    );
    assert_eq!(publisher.phase(), Phase::Stopping);

    let ack = publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let restored = ack
        .iter()
        .find_map(|outcome| match outcome {
            Outcome::StartEngine { request, .. } => Some(request.clone()),
            _ => None,
        })
        .expect("the previous request starts after the ack");
    assert_eq!(restored, previous);
    // Exactly one start of the restored attempt, produced only by its engine.
    let start = publisher.engine_ready(now, false, None);
    assert_eq!(
        sends(&start)
            .iter()
            .filter(|signal| signal.kind == "stream.start")
            .count(),
        1
    );
}

#[test]
fn a_server_failure_does_not_restore_the_previous_request() {
    let now = Instant::now();
    let mut publisher = publisher();
    live(&mut publisher, now);
    publisher.restart(
        PublishRequest {
            quality: Quality {
                resolution: Resolution::P720,
                ..Quality::default()
            },
            ..request()
        },
        now,
    );
    publisher.on_server_event("stream.stopped", "", &json!({}), now);
    let ready = publisher.engine_ready(now, false, None);
    let start_id = start_signal(&ready).request_id.clone().expect("start id");

    let failed = publisher.on_server_event("error", &start_id, &json!({"code": "forbidden"}), now);
    assert_eq!(publisher.phase(), Phase::Failed);
    assert!(
        !failed
            .iter()
            .any(|outcome| matches!(outcome, Outcome::StartEngine { .. })),
        "a revoked permission must not start the previous publication: {failed:?}"
    );
}

#[test]
fn a_viewer_sees_one_stop_and_one_start_in_order() {
    let now = Instant::now();
    let mut publisher = publisher();
    let first = publishing(&mut publisher, now);
    assert_eq!(start_signal(&first).kind, "stream.start");
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );

    let mut kinds = Vec::new();
    let stop = publisher.restart(
        PublishRequest {
            quality: Quality {
                resolution: Resolution::P720,
                ..Quality::default()
            },
            ..request()
        },
        now,
    );
    kinds.extend(sends(&stop).iter().map(|signal| signal.kind));
    let ack = publisher.on_server_event("stream.stopped", "", &json!({}), now);
    kinds.extend(sends(&ack).iter().map(|signal| signal.kind));
    let ready = publisher.engine_ready(now, false, None);
    kinds.extend(sends(&ready).iter().map(|signal| signal.kind));

    assert_eq!(kinds, vec!["stream.stop", "stream.start"]);
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "stream.start").count(),
        1,
        "the connected viewer receives exactly one new start: {kinds:?}"
    );
    assert!(
        kinds.iter().position(|kind| *kind == "stream.stop")
            < kinds.iter().position(|kind| *kind == "stream.start"),
        "the stop must precede the start: {kinds:?}"
    );
}
