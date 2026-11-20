//! Frozen-picture detection. No sockets, WebRTC or timers: the state machine
//! only sees cumulative stats reports and an explicit clock.

use super::{publication, snapshot};
use crate::viewer::stats::{MAX_KEYFRAME_REQUESTS, STALL_WINDOW};
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
fn stopped_rtp_while_connected_uses_the_ice_recovery_path() {
    let (mut viewer, attempt, start) = watching("server");
    viewer.on_frame();
    report(&mut viewer, attempt, 1_000, 10, start);
    let mut outcomes = Vec::new();
    for step in 1..=2 {
        let at = start + STALL_WINDOW * step;
        report(&mut viewer, attempt, 1_000, 10, at);
        outcomes.extend(viewer.tick(at));
    }
    assert_eq!(
        recovery_actions(&outcomes),
        vec!["ice_restart".to_string()],
        "no RTP at all while connected is not a keyframe problem: {outcomes:?}"
    );
    let watching = viewer.watching().unwrap();
    assert!(watching.recovering, "the existing ICE path is engaged");
    assert!(!watching.connected);
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
    // Two idle windows move the viewer into ICE recovery.
    for step in 1..=2 {
        let at = start + STALL_WINDOW * step;
        report(&mut viewer, attempt, 1_000, 10, at);
        viewer.tick(at);
    }
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
    let (mut viewer, attempt, start) = watching("server");
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
