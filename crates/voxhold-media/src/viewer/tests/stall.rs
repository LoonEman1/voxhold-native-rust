//! Frozen-picture detection. No sockets, WebRTC or timers: the state machine
//! only sees cumulative stats reports and an explicit clock.

use super::{publication, snapshot};
use crate::viewer::stats::{MAX_KEYFRAME_REQUESTS, MISSING_WINDOWS, STALL_WINDOW};
use crate::viewer::*;
use serde_json::json;
use std::time::Instant;

fn watching(mode: &str) -> (Viewer, u64, Instant) {
    let now = Instant::now();
    let mut stream = publication("a", 1);
    stream.mode = mode.into();
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[stream.clone()], now);
    viewer.set_joined(true, now);
    let started = viewer.select(&stream, 1, now);
    assert!(
        started
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Start { selection: 1, .. })),
        "the attempt must start: {started:?}"
    );
    let attempt = viewer.watching().expect("watching").attempt;
    (viewer, attempt, now)
}

fn report(viewer: &mut Viewer, attempt: u64, bytes: u64, frames: u32, at: Instant) {
    assert!(
        viewer.on_video_stats(attempt, bytes, frames, at).is_empty(),
        "a stats report never changes the UI by itself"
    );
}

fn count(outcomes: &[Outcome], kind: &str) -> usize {
    outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Outcome::Send(signal) if signal.kind == kind))
        .count()
}

fn recovery_actions(outcomes: &[Outcome]) -> Vec<String> {
    outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Send(signal) if signal.kind == "stream.recovery_request" => signal
                .data
                .get("action")
                .and_then(|action| action.as_str())
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

#[test]
fn decoded_frames_are_healthy_and_reset_the_budget() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    viewer.retries = 2;
    report(&mut viewer, attempt, 1_000, 10, start);
    let mut at = start;
    for step in 1..=6 {
        at += STALL_WINDOW;
        report(
            &mut viewer,
            attempt,
            1_000 + step * 500,
            10 + step as u32,
            at,
        );
        let outcomes = viewer.tick(at);
        assert!(
            recovery_actions(&outcomes).is_empty() && count(&outcomes, "stream.leave") == 0,
            "a moving picture must not start recovery: {outcomes:?}"
        );
    }
    assert_eq!(
        viewer.retries, 0,
        "a full healthy stats window restores the reconnect budget"
    );
    assert!(viewer.watching().unwrap().connected);
}

#[test]
fn rtp_without_decoded_frames_requests_a_keyframe_not_an_ice_restart() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    report(&mut viewer, attempt, 1_000, 10, start);
    let mut outcomes = Vec::new();
    for step in 1..=2 {
        let at = start + STALL_WINDOW * step;
        report(&mut viewer, attempt, 1_000 + step as u64 * 100, 10, at);
        outcomes.extend(viewer.tick(at));
    }
    assert_eq!(
        recovery_actions(&outcomes),
        vec!["keyframe".to_string()],
        "RTP is arriving but no frame decodes: {outcomes:?}"
    );
    assert!(
        !outcomes.iter().any(|outcome| matches!(
            outcome,
            Outcome::Connected {
                connected: false,
                ..
            }
        )),
        "a keyframe request must keep the picture and its transport: {outcomes:?}"
    );
    assert!(!viewer.watching().unwrap().recovering);
}

#[test]
fn identical_counters_on_a_connected_transport_keep_the_static_picture() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    report(&mut viewer, attempt, 1_000, 10, start);
    let mut outcomes = Vec::new();
    // A static screen may legally stop producing RTP while the peer stays
    // connected. The browser reference keeps the last decoded picture; so does
    // the native viewer, for far longer than one recovery window.
    for step in 1..=8 {
        let at = start + STALL_WINDOW * step;
        report(&mut viewer, attempt, 1_000, 10, at);
        outcomes.extend(viewer.tick(at));
    }
    assert!(
        recovery_actions(&outcomes).is_empty() && count(&outcomes, "stream.leave") == 0,
        "a static connected picture must not be recovered: {outcomes:?}"
    );
    let watching = viewer.watching().unwrap();
    assert!(watching.connected, "the last picture stays valid");
    assert!(!watching.recovering, "no recovery was armed");
    assert_eq!(viewer.retries, 0, "a static screen spends no budget");
    let stats = &viewer.active.as_ref().unwrap().stats;
    assert_eq!(stats.idle_windows, 0, "the idle window is reset");
    assert_eq!(stats.missing_windows, 0, "the reports did arrive");
}

#[test]
fn an_engine_disconnected_state_uses_the_ice_recovery_path() {
    let (mut viewer, _attempt, start) = watching("server");
    viewer.on_frame();
    // Transport loss is reported by the PeerConnection state, never inferred
    // from RTP silence.
    let outcomes = viewer.on_engine_state("disconnected", start);
    assert_eq!(
        recovery_actions(&outcomes),
        vec!["ice_restart".to_string()],
        "the engine state owns the ICE path: {outcomes:?}"
    );
    let watching = viewer.watching().unwrap();
    assert!(watching.recovering);
    assert!(!watching.connected);
    assert_eq!(viewer.retries, 1);
}

#[test]
fn a_static_screen_with_rare_real_frames_is_not_frozen() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    report(&mut viewer, attempt, 1_000, 10, start);
    let mut at = start;
    // An unchanged window is followed by one real frame, repeatedly. Static
    // content may legally skip a window, so the guard must not fire.
    for step in 1..=8 {
        at += STALL_WINDOW;
        let frames = if step % 2 == 0 { 11 } else { 10 };
        report(&mut viewer, attempt, 1_000 + step * 50, frames, at);
        let outcomes = viewer.tick(at);
        assert!(
            recovery_actions(&outcomes).is_empty() && count(&outcomes, "stream.leave") == 0,
            "rare but real frames are healthy: {outcomes:?}"
        );
    }
    assert_eq!(viewer.retries, 0);
    assert!(recovery_actions(&[]).is_empty());
}

#[test]
fn recovery_stops_and_the_budget_resets_when_a_frame_arrives_again() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    report(&mut viewer, attempt, 1_000, 10, start);
    // A real engine disconnect moves the viewer into ICE recovery.
    viewer.on_engine_state("disconnected", start);
    let watching = viewer.watching().unwrap();
    assert!(watching.recovering);
    assert_eq!(viewer.retries, 1);

    // One decoded frame ends recovery immediately; the deadline is gone.
    let resumed = viewer.on_frame();
    assert!(resumed.iter().any(|outcome| matches!(
        outcome,
        Outcome::Connected {
            connected: true,
            ..
        }
    )));
    let watching = viewer.watching().unwrap();
    assert!(!watching.recovering && watching.connected);
    assert_eq!(viewer.retries, 1, "one frame is not a healthy window");

    // A full window of decoded frames is healthy and restores the budget.
    let mut at = start + STALL_WINDOW * 3;
    report(&mut viewer, attempt, 2_000, 20, at);
    at += STALL_WINDOW;
    report(&mut viewer, attempt, 2_100, 21, at);
    let outcomes = viewer.tick(at);
    assert!(
        recovery_actions(&outcomes).is_empty(),
        "recovery must stop: {outcomes:?}"
    );
    assert_eq!(viewer.retries, 0);
}

#[test]
fn a_missing_report_is_counted_apart_from_identical_counters() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    // A healthy window first, so idle starts clean.
    report(&mut viewer, attempt, 1_000, 10, start);
    let healthy = start + STALL_WINDOW;
    report(&mut viewer, attempt, 1_100, 11, healthy);
    viewer.tick(healthy);

    let mut at = healthy;
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        at += STALL_WINDOW;
        outcomes.extend(viewer.tick(at));
    }
    assert!(
        recovery_actions(&outcomes).is_empty() && count(&outcomes, "stream.leave") == 0,
        "no report says nothing about the encoder: {outcomes:?}"
    );
    let stats = &viewer.active.as_ref().unwrap().stats;
    assert_eq!(stats.missing_windows, 3, "missing windows are counted");
    assert_eq!(stats.idle_windows, 0, "missing is not an identical report");
}

#[test]
fn stats_of_another_attempt_are_ignored() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    assert!(
        viewer
            .on_video_stats(attempt.wrapping_add(1), 9_000, 99, start)
            .is_empty()
    );
    let stats = &viewer.active.as_ref().unwrap().stats;
    assert!(stats.latest.is_none(), "the stale attempt was not recorded");
    assert_eq!(stats.reports, 0);
}

#[test]
fn keyframe_attempts_are_bounded_and_then_the_same_publication_is_rewatched() {
    let (mut viewer, mut attempt, start) = watching("server");
    viewer.on_frame();
    let mut bytes = 1_000_u64;
    report(&mut viewer, attempt, bytes, 10, start);
    let mut at = start;
    let mut keyframes = 0;
    let mut leaves = 0;
    let mut failed = None;

    for _ in 0..80 {
        at += STALL_WINDOW;
        bytes += 100;
        // RTP keeps arriving; the decoder never produces a frame.
        report(&mut viewer, attempt, bytes, 10, at);
        for outcome in viewer.tick(at) {
            match outcome {
                Outcome::Send(signal) if signal.kind == "stream.recovery_request" => {
                    assert_eq!(
                        signal.data["action"].as_str(),
                        Some("keyframe"),
                        "decode stalls never restart ICE"
                    );
                    keyframes += 1;
                }
                Outcome::Send(signal) if signal.kind == "stream.leave" => {
                    // `fail` also sends a leave, but clears the intent before
                    // the outcomes are applied. Only a re-watch keeps it.
                    if viewer.intent().is_some() {
                        leaves += 1;
                        let id = signal.request_id.clone().unwrap_or_default();
                        let ack = viewer.on_server_event("stream.left", &id, &json!({}), at);
                        assert!(
                            ack.iter().any(|outcome| matches!(
                                outcome,
                                Outcome::Start { selection: 1, .. }
                            )),
                            "the same publication starts again: {ack:?}"
                        );
                        // The replacement engine is a new viewing attempt:
                        // reports stamped with the closed engine are ignored.
                        let next = viewer.watching().expect("the re-watch is active").attempt;
                        assert_ne!(next, attempt, "a reconnect gets a fresh attempt id");
                        attempt = next;
                        assert!(
                            viewer
                                .on_video_stats(attempt.wrapping_add(1), bytes, 10, at)
                                .is_empty()
                        );
                        // The new engine confirms its first picture; this must
                        // not reset the reconnect budget by itself.
                        viewer.on_frame();
                    }
                }
                Outcome::Failed { message, .. } => failed = Some(message),
                _ => {}
            }
        }
        if failed.is_some() {
            break;
        }
    }

    let failed = failed.expect("the budget must end the viewer");
    assert!(
        failed.contains("изображение"),
        "the error speaks about the picture: {failed}"
    );
    assert_eq!(
        leaves, MAX_RETRIES as usize,
        "only the bounded reconnects are attempted"
    );
    assert_eq!(
        keyframes,
        MAX_KEYFRAME_REQUESTS as usize * (MAX_RETRIES as usize + 1),
        "each engine gets its bounded keyframe requests"
    );
    assert!(viewer.watching().is_none());

    // After the error no further stats can restart anything.
    let outcomes = viewer.tick(at + STALL_WINDOW);
    assert!(outcomes.is_empty());
}

#[test]
fn advancing_stats_without_a_presented_frame_exhaust_the_budget() {
    let (mut viewer, mut attempt, start) = watching("server");
    // The decoder reports progress, but `on_frame` never presents a picture
    // (for example `VideoStore` rejected every oversized frame). Advancing
    // counters alone are not a healthy window and must not restore the
    // reconnect budget, otherwise the deadline path reconnects forever.
    let mut bytes = 1_000_u64;
    let mut frames = 10_u32;
    report(&mut viewer, attempt, bytes, frames, start);
    let mut rewatches = 0_usize;
    let mut failed = None;
    let mut at = start;
    for _ in 0..40 {
        at += STALL_WINDOW;
        bytes += 100;
        frames += 1;
        report(&mut viewer, attempt, bytes, frames, at);
        for outcome in viewer.tick(at) {
            match outcome {
                Outcome::Send(signal) if signal.kind == "stream.leave" => {
                    if viewer.intent().is_none() {
                        continue;
                    }
                    let id = signal.request_id.clone().unwrap_or_default();
                    let ack = viewer.on_server_event("stream.left", &id, &json!({}), at);
                    assert!(
                        ack.iter()
                            .any(|outcome| matches!(outcome, Outcome::Start { .. })),
                        "the bounded re-watch starts: {ack:?}"
                    );
                    rewatches += 1;
                    attempt = viewer.watching().expect("the re-watch is active").attempt;
                }
                Outcome::Failed { message, .. } => failed = Some(message),
                _ => {}
            }
        }
        if failed.is_some() {
            break;
        }
    }
    let failed = failed.expect("the deadline path must end the viewer");
    assert!(
        failed.contains("восстановить"),
        "the deadline reports a failed recovery: {failed}"
    );
    assert_eq!(
        rewatches, MAX_RETRIES as usize,
        "only the bounded reconnect budget is spent"
    );
    assert!(viewer.watching().is_none());
}

#[test]
fn the_start_outcome_names_the_attempt_whose_statistics_are_accepted() {
    let now = Instant::now();
    let stream = publication("a", 1);
    let mut viewer = Viewer::new(1, 10);
    snapshot(&mut viewer, &[stream.clone()], now);
    viewer.set_joined(true, now);
    let started = viewer.select(&stream, 1, now);
    let start_attempt = started
        .iter()
        .find_map(|outcome| match outcome {
            Outcome::Start { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .expect("the attempt starts");
    assert_eq!(
        start_attempt,
        viewer.watching().expect("watching").attempt,
        "the installed engine and the state machine share one attempt id"
    );

    viewer.on_frame();
    report(&mut viewer, start_attempt, 1_000, 10, now);
    assert_eq!(
        viewer.active.as_ref().unwrap().stats.reports,
        1,
        "the start attempt's report is recorded"
    );
    assert!(
        viewer
            .on_video_stats(start_attempt.wrapping_sub(1), 9_000, 99, now + STALL_WINDOW)
            .is_empty()
    );
    assert_eq!(
        viewer.active.as_ref().unwrap().stats.reports,
        1,
        "an older attempt's report is rejected"
    );
}

#[test]
fn a_new_socket_session_restores_the_reconnect_budget() {
    let (mut viewer, _attempt, start) = watching("server");
    let retained = viewer.intent().expect("intent").clone();
    viewer.on_frame();
    assert!(!viewer.on_engine_state("disconnected", start).is_empty());
    assert_eq!(viewer.retries, 1);
    viewer.on_frame();
    assert!(!viewer.on_engine_state("failed", start).is_empty());
    assert_eq!(viewer.retries, MAX_RETRIES);

    // The socket ends; the selection is retained and watched again on the
    // fresh session. Its first stall must get the usual bounded re-watch.
    let ended = viewer.transport_ended(start);
    assert!(ended.contains(&Outcome::Close));
    assert_eq!(
        viewer.retries, 0,
        "a new socket session starts a fresh reconnect budget"
    );
    snapshot(&mut viewer, &[retained], start);
    viewer.set_joined(true, start);
    let attempt = viewer.watching().expect("the re-watch starts").attempt;
    viewer.on_frame();
    report(&mut viewer, attempt, 1_000, 10, start);

    let mut leave = false;
    let mut failed = None;
    for step in 1..=8 {
        let at = start + STALL_WINDOW * step;
        report(&mut viewer, attempt, 1_000 + step as u64 * 100, 10, at);
        for outcome in viewer.tick(at) {
            match outcome {
                Outcome::Send(signal) if signal.kind == "stream.leave" => leave = true,
                Outcome::Failed { message, .. } => failed = Some(message),
                _ => {}
            }
        }
        if leave || failed.is_some() {
            break;
        }
    }
    assert!(
        failed.is_none(),
        "the first stall of the new session must not fail: {failed:?}"
    );
    assert!(leave, "the first stall still gets its bounded re-watch");
    assert_eq!(viewer.retries, 1);
}

#[test]
fn a_repeated_engine_failure_counts_one_retry() {
    let (mut viewer, _attempt, start) = watching("server");
    viewer.on_frame();
    let first = viewer.on_engine_state("disconnected", start);
    assert_eq!(
        recovery_actions(&first),
        vec!["ice_restart".to_string()],
        "one loss sends one ICE restart: {first:?}"
    );
    assert_eq!(viewer.retries, 1);

    // The same ICE failure often surfaces as `disconnected` then `failed`.
    let duplicate = viewer.on_engine_state("failed", start);
    assert!(
        duplicate.is_empty(),
        "a duplicate state of the armed recovery is ignored: {duplicate:?}"
    );
    assert_eq!(viewer.retries, 1, "one failure spends one retry");

    // A decoded frame ends that recovery; the next loss spends the next retry.
    viewer.on_frame();
    let second = viewer.on_engine_state("failed", start);
    assert_eq!(recovery_actions(&second), vec!["ice_restart".to_string()]);
    assert_eq!(viewer.retries, 2);
}

#[test]
fn an_error_for_a_keyframe_request_keeps_the_bounded_recovery() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    report(&mut viewer, attempt, 1_000, 10, start);
    let mut keyframe = None;
    for step in 1..=2 {
        let at = start + STALL_WINDOW * step;
        report(&mut viewer, attempt, 1_000 + step as u64 * 100, 10, at);
        for outcome in viewer.tick(at) {
            if let Outcome::Send(signal) = outcome {
                if signal.kind == "stream.recovery_request"
                    && signal.data["action"].as_str() == Some("keyframe")
                {
                    keyframe = signal.request_id;
                }
            }
        }
    }
    let request = keyframe.expect("a keyframe request is sent");
    let reply = viewer.on_server_event(
        "error",
        &request,
        &json!({"code":"unsupported"}),
        start + STALL_WINDOW * 3,
    );
    assert!(
        reply.is_empty(),
        "a keyframe reply rejects the keyframe, not the view: {reply:?}"
    );
    assert!(
        viewer.watching().is_some() && viewer.intent().is_some(),
        "the bounded recovery keeps the publication"
    );
}

#[test]
fn missing_stats_windows_escalate_to_ice_after_the_browser_threshold() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    report(&mut viewer, attempt, 1_000, 10, start);
    // The first window still has its report and is a static screen, not a
    // missing sample.
    assert!(
        recovery_actions(&viewer.tick(start + STALL_WINDOW)).is_empty(),
        "the reported window is not a missing one"
    );
    let mut outcomes = Vec::new();
    for step in 2..=u32::from(MISSING_WINDOWS) {
        outcomes.extend(viewer.tick(start + STALL_WINDOW * step));
    }
    assert!(
        recovery_actions(&outcomes).is_empty(),
        "the first missing windows are tolerated: {outcomes:?}"
    );
    outcomes = viewer.tick(start + STALL_WINDOW * (u32::from(MISSING_WINDOWS) + 1));
    assert_eq!(
        recovery_actions(&outcomes),
        vec!["ice_restart".to_string()],
        "a dead stats source uses the existing ICE path: {outcomes:?}"
    );
    let watching = viewer.watching().unwrap();
    assert!(watching.recovering && !watching.connected);
    assert_eq!(viewer.active.as_ref().unwrap().stats.missing_windows, 0);
}
