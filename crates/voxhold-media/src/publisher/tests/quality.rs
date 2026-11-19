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
