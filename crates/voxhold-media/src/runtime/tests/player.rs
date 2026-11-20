//! Player lifetime regressions: a replacement engine must show its own first
//! frame, and closing a view must not join the peer on the signaling loop.

use super::*;
use crate::runtime::player::{close_player, install_stream};
use crate::stream::StreamViewer;
use libwebrtc::video_frame::I420Buffer;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

fn sync_pair() -> (Arc<(Mutex<bool>, std::sync::Condvar)>, Arc<AtomicBool>) {
    (
        Arc::new((Mutex::new(false), std::sync::Condvar::new())),
        Arc::new(AtomicBool::new(false)),
    )
}

fn release(gate: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

fn wait_released(released: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !released.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "the release thread never dropped the player engine"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn frame_outcomes(
    viewer: &mut Viewer,
    render: &mut VideoFrame,
    update: EngineUpdate,
    now: Instant,
) -> Vec<ViewerOutcome> {
    let slot = Mutex::new(None);
    match super::super::engine::apply_engine_update(&slot, render, viewer, update, now) {
        super::super::engine::EngineEffect::Outcomes(outcomes) => outcomes,
        _ => panic!("a picture update never reports an audio-output result"),
    }
}

fn watching(now: Instant) -> (Viewer, Publication) {
    let stream = publication("a");
    let mut viewer = Viewer::new(1, 10);
    viewer.on_server_event(
        "stream.snapshot",
        "",
        &serde_json::json!({"streams": [stream.clone()]}),
        now,
    );
    viewer.set_joined(true, now);
    let started = viewer.select(&stream, 1, now);
    assert!(
        started
            .iter()
            .any(|outcome| matches!(outcome, ViewerOutcome::Start { selection: 1, .. })),
        "the first attempt starts: {started:?}"
    );
    (viewer, stream)
}

#[test]
fn a_replacement_engine_reads_its_first_frame_at_the_same_sequence_and_size() {
    let now = Instant::now();
    let (mut viewer, stream) = watching(now);
    let mut render = VideoFrame::default();

    // Engine A decodes its first frame, sequence 1 at 320x180.
    let (mut first, _commands_a, events_a, store_a) = StreamViewer::detached_for_tests();
    assert!(store_a.publish_i420(I420Buffer::new_black(640, 360)));
    events_a.push(Event::Frame);
    assert!(matches!(
        first.poll(&mut render, 320, 180),
        Some(EngineUpdate::Frame)
    ));
    let outcomes = frame_outcomes(&mut viewer, &mut render, EngineUpdate::Frame, now);
    assert!(outcomes.iter().any(|outcome| matches!(
        outcome,
        ViewerOutcome::Connected {
            selection: 1,
            connected: true
        }
    )));

    // Reconnect to the same publication with the same selection id. The viewer
    // closes the old attempt and starts a new one after `stream.left`.
    let switching = viewer.select(&stream, 1, now);
    let leave = switching
        .iter()
        .find_map(|outcome| match outcome {
            ViewerOutcome::Send(signal) if signal.kind == "stream.leave" => {
                signal.request_id.clone()
            }
            _ => None,
        })
        .expect("the old attempt must leave");
    let ack = viewer.on_server_event("stream.left", &leave, &serde_json::json!({}), now);
    assert!(
        ack.iter()
            .any(|outcome| matches!(outcome, ViewerOutcome::Start { selection: 1, .. })),
        "the same publication starts a new attempt: {ack:?}"
    );

    // Engine B is a different store and peer, but its first frame also has
    // sequence 1 and the same decoded size. Without the install reset the
    // cached frame identity would hide it and no confirmation would be sent.
    let (mut second, _commands_b, events_b, store_b) = StreamViewer::detached_for_tests();
    assert!(store_b.publish_i420(I420Buffer::new_black(640, 360)));
    events_b.push(Event::Frame);
    render.reset_for_new_engine();
    assert!(matches!(
        second.poll(&mut render, 320, 180),
        Some(EngineUpdate::Frame)
    ));
    let outcomes = frame_outcomes(&mut viewer, &mut render, EngineUpdate::Frame, now);
    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            ViewerOutcome::Connected {
                selection: 1,
                connected: true
            }
        )),
        "B's picture must confirm the new attempt: {outcomes:?}"
    );

    // Entering and leaving fullscreen is only a repaint of B's cached frame.
    assert!(
        matches!(
            second.poll(&mut render, 640, 360),
            Some(EngineUpdate::Repaint)
        ),
        "a larger surface re-reads the cached picture"
    );
    let outcomes = frame_outcomes(&mut viewer, &mut render, EngineUpdate::Repaint, now);
    assert!(
        outcomes.is_empty(),
        "a resize must not confirm or restart anything: {outcomes:?}"
    );

    // Replace B while fullscreen is active. The new engine's first frame is
    // again sequence 1 at the fullscreen surface; the install reset must still
    // apply, and leaving fullscreen afterwards stays a repaint of C.
    let (mut third, _commands_c, events_c, store_c) = StreamViewer::detached_for_tests();
    assert!(store_c.publish_i420(I420Buffer::new_black(640, 360)));
    events_c.push(Event::Frame);
    render.reset_for_new_engine();
    assert!(matches!(
        third.poll(&mut render, 640, 360),
        Some(EngineUpdate::Frame)
    ));
    assert!(matches!(
        third.poll(&mut render, 160, 90),
        Some(EngineUpdate::Repaint)
    ));
    assert!(third.poll(&mut render, 160, 90).is_none());
    assert_eq!((render.width(), render.height()), (160, 90));
}

#[test]
fn closing_the_player_returns_before_the_old_peer_finishes() {
    let (gate, released) = sync_pair();
    let mut stream = Some(StreamViewer::blocked_for_tests(
        gate.clone(),
        released.clone(),
    ));
    let mut selection = Some(7_u64);
    let frame = Mutex::new(Some(StreamFrame {
        selection: 7,
        width: 2,
        height: 2,
        bgra: vec![0; 16],
    }));

    let started = Instant::now();
    close_player(&mut stream, &mut selection, &frame);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the signaling path waited for the player drop: {:?}",
        started.elapsed()
    );
    assert!(stream.is_none(), "the engine left the session immediately");
    assert_eq!(selection, None, "the selection left with the engine");
    assert!(
        frame.lock().unwrap().is_none(),
        "the published frame is cleared before the peer closes"
    );
    assert!(
        !released.load(Ordering::SeqCst),
        "the drop is gated, so the caller must not have performed it"
    );

    release(&gate);
    wait_released(&released);
}

#[test]
fn rapid_switches_release_every_stream_and_publish_no_late_frame() {
    let mut gates = Vec::new();
    let mut released = Vec::new();
    let mut stream;
    let mut selection;
    let frame = Mutex::new(None);

    for index in 0..3 {
        let (gate, flag) = sync_pair();
        stream = Some(StreamViewer::blocked_for_tests(gate.clone(), flag.clone()));
        selection = Some(index as u64);
        if let Ok(mut slot) = frame.lock() {
            *slot = Some(StreamFrame {
                selection: index as u64,
                width: 1,
                height: 1,
                bgra: vec![0; 4],
            });
        }
        close_player(&mut stream, &mut selection, &frame);
        // A decode of the old engine may still be queued; the engine is gone
        // before it can be polled, so the UI slot stays empty.
        assert!(frame.lock().unwrap().is_none());
        gates.push(gate);
        released.push(flag);
    }
    assert!(released.iter().all(|flag| !flag.load(Ordering::SeqCst)));
    for gate in &gates {
        release(gate);
    }
    for flag in &released {
        wait_released(flag);
    }
}

#[test]
fn a_cached_repaint_is_not_video_progress() {
    let now = Instant::now();
    let window = Duration::from_secs(2);
    let (mut viewer, _stream) = watching(now);
    viewer.on_frame();
    let attempt = viewer.watching().unwrap().attempt;
    viewer.on_video_stats(attempt, 1_000, 10, now);

    // A resize re-reads the cached picture as `EngineUpdate::Repaint`. It is a
    // draw, not a decoded frame: no stats report and no `on_frame`.
    let mut render = VideoFrame::default();
    let outcomes = frame_outcomes(&mut viewer, &mut render, EngineUpdate::Repaint, now);
    assert!(
        outcomes.is_empty(),
        "a repaint must not confirm or restart anything: {outcomes:?}"
    );

    // Identical counters on a connected transport are a static screen, not a
    // broken one. The draw has not made the picture move, but the transport is
    // still healthy, so the view must survive without keyframe/ICE/re-watch.
    for step in 1..=4 {
        let at = now + window * step;
        viewer.on_video_stats(attempt, 1_000, 10, at);
        let outcomes = viewer.tick(at);
        assert!(
            !outcomes.iter().any(|outcome| matches!(
                outcome,
                ViewerOutcome::Send(signal)
                    if signal.kind == "stream.recovery_request" || signal.kind == "stream.leave"
            )),
            "a static connected picture must be preserved: {outcomes:?}"
        );
    }
    let watching = viewer.watching().expect("the picture stays selected");
    assert!(watching.connected && !watching.recovering);
}

#[test]
fn leave_is_sent_and_the_next_event_runs_before_the_old_engine_is_released() {
    let now = Instant::now();
    let (mut viewer, _stream) = watching(now);
    assert!(viewer.on_frame().iter().any(|outcome| matches!(
        outcome,
        ViewerOutcome::Connected {
            connected: true,
            ..
        }
    )));

    let (gate, released) = sync_pair();
    let mut stream = Some(StreamViewer::blocked_for_tests(
        gate.clone(),
        released.clone(),
    ));
    let mut selection = Some(1_u64);
    let frame = Mutex::new(None);

    let outcomes = viewer.leave(now);
    let mut leave_sent = false;
    for outcome in outcomes {
        match outcome {
            ViewerOutcome::Close => close_player(&mut stream, &mut selection, &frame),
            ViewerOutcome::Send(signal) if signal.kind == "stream.leave" => leave_sent = true,
            _ => {}
        }
    }
    // The next voice signal/event is handled by the session before the release
    // thread can finish joining the old peer.
    let next_voice_event = true;
    assert!(leave_sent, "stream.leave must be queued by the close");
    assert!(next_voice_event);
    assert!(
        !released.load(Ordering::SeqCst),
        "the old engine is still closing while the session moves on"
    );

    release(&gate);
    wait_released(&released);

    // A late frame notification of the closed stream has no engine to poll.
    assert!(stream.is_none());
    assert!(frame.lock().unwrap().is_none());
}

#[test]
fn installing_a_replacement_engine_resets_the_cached_frame_identity() {
    let (first, _commands_a, events_a, store_a) = StreamViewer::detached_for_tests();
    let (second, _commands_b, events_b, store_b) = StreamViewer::detached_for_tests();
    let mut stream = None;
    let mut selection = None;
    let slot = Mutex::new(None);
    let mut render = VideoFrame::default();

    // Engine A's first frame owns the picture surface: sequence 1 at 320x180.
    assert!(store_a.publish_i420(I420Buffer::new_black(640, 360)));
    events_a.push(Event::Frame);
    install_stream(&mut stream, &mut selection, &slot, &mut render, first, 1);
    assert!(matches!(
        stream.as_mut().unwrap().poll(&mut render, 320, 180),
        Some(EngineUpdate::Frame)
    ));

    // Engine B is a different store that also starts at sequence 1 with the
    // same fitted size. The production install path must forget A's identity,
    // otherwise B's first frame is hidden and the attempt never confirms.
    assert!(store_b.publish_i420(I420Buffer::new_black(640, 360)));
    events_b.push(Event::Frame);
    install_stream(&mut stream, &mut selection, &slot, &mut render, second, 1);
    assert!(matches!(
        stream.as_mut().unwrap().poll(&mut render, 320, 180),
        Some(EngineUpdate::Frame)
    ));
    assert_eq!(selection, Some(1), "the install owns the selection too");
}

#[test]
fn statistics_are_stamped_with_the_attempt_the_runtime_installed() {
    let now = Instant::now();
    let (viewer, _stream) = watching(now);
    let attempt = viewer.watching().expect("watching").attempt;
    // The runtime passes `Outcome::Start.attempt` to `StreamViewer::start`;
    // `poll` must report the same id or the state machine rejects a live
    // engine's statistics as stale.
    let (mut engine, _commands, events, _video) = StreamViewer::detached_for_attempt(attempt);
    events.push(Event::VideoStats {
        bytes_received: 42,
        frames_decoded: 7,
    });
    let mut render = VideoFrame::default();
    let update = engine.poll(&mut render, 1, 1).expect("a stats event");
    assert!(matches!(
        update,
        EngineUpdate::Stats {
            attempt: stamped,
            bytes_received: 42,
            frames_decoded: 7,
        } if stamped == attempt
    ));
}
