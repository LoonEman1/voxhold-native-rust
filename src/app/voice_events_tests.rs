//! Event-driven wake regressions for the voice panel (O09).
//!
//! The panel used to poll unconditionally every 100 ms. These tests drive the
//! real tasks and the real media wake (`Updates`, `StreamFrame`, `PreviewStore`)
//! with a fake GPUI clock, so a missing poll and an extra poll are both
//! observable.

use super::voice_tests::{draw, fixture_with};
use crate::voice_panel::VoicePanel;
use gpui::{BackgroundExecutor, Entity, TestAppContext, VisualTestContext};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;
use voxhold_media::controller::Publication;
use voxhold_media::publisher::{Phase, Sharing};
use voxhold_media::runtime::VoiceUpdate;

fn publication(channel_id: i64, publisher: &str, user: i64) -> Publication {
    Publication {
        server_id: 1,
        channel_id,
        publisher_user_id: user,
        publisher_connection_id: publisher.into(),
        mode: "server".into(),
        codec: "vp8".into(),
        has_audio: false,
        viewer_count: 1,
        started_at_ms: 1,
    }
}

fn executor(visual: &mut VisualTestContext) -> BackgroundExecutor {
    visual.update(|_window, app| app.background_executor().clone())
}

/// Advance the fake clock, then run everything it released.
fn advance(visual: &mut VisualTestContext, executor: &BackgroundExecutor, millis: u64) {
    executor.advance_clock(Duration::from_millis(millis));
    visual.run_until_parked();
}

fn reset_polls(visual: &mut VisualTestContext, voice: &Entity<VoicePanel>) {
    visual.update(|_window, app| {
        voice.update(app, |panel, _| panel.test_reset_poll_count());
    });
    visual.run_until_parked();
}

fn poll_count(visual: &VisualTestContext, voice: &Entity<VoicePanel>) -> u32 {
    voice.read_with(visual, |panel, _| panel.test_poll_count())
}

fn attach(visual: &mut VisualTestContext, voice: &Entity<VoicePanel>) {
    visual.update(|_window, app| {
        voice.update(app, |panel, _| panel.test_attach_detached_handle());
    });
    visual.run_until_parked();
}

/// Open the player through the production `watch`: it starts the 100 ms frame
/// ticker and signals the event task.
fn watch(visual: &mut VisualTestContext, voice: &Entity<VoicePanel>) {
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.watch(publication(7, "alice", 1), window, cx);
        });
    });
    visual.run_until_parked();
}

fn push_frame(visual: &mut VisualTestContext, voice: &Entity<VoicePanel>, selection: u64) {
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_push_player_frame(selection, 64, 36);
        });
    });
    visual.run_until_parked();
}

/// Without a call and without a handle the panel has no reason to run: half a
/// second of fake time must not produce a single poll. The old unconditional
/// 100 ms loop failed this with five polls.
#[gpui::test]
async fn an_idle_panel_without_a_call_never_polls(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture_with(cx, None, false);
    draw(&mut visual);
    let executor = executor(&mut visual);
    reset_polls(&mut visual, &voice);
    advance(&mut visual, &executor, 500);
    assert_eq!(
        poll_count(&visual, &voice),
        0,
        "an idle panel must not poll without a wake or a deadline"
    );
}

/// A call without a stream keeps only the one-second clock. Half a second has
/// no poll; the next one runs the clock and the fallback tick.
#[gpui::test]
async fn a_call_without_a_stream_has_no_100ms_poll_but_ticks_the_clock(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture_with(cx, None, true);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_streams(vec![publication(7, "alice", 1)]);
        });
    });
    draw(&mut visual);
    let executor = executor(&mut visual);
    reset_polls(&mut visual, &voice);
    advance(&mut visual, &executor, 500);
    assert_eq!(
        poll_count(&visual, &voice),
        0,
        "no 100 ms frame deadline runs without a player"
    );
    advance(&mut visual, &executor, 600);
    assert_eq!(
        poll_count(&visual, &voice),
        1,
        "the one-second clock is the only tick of a call without a stream"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_live_clock_second().is_some(),
            "the clock tick recorded the on-air second"
        );
    });
}

/// A watcher who is not in the call still sees a foreign stream card with an
/// `M:SS` label. The painted room list with a running stream is the third
/// reason for the one-second clock, after a call and an open handle, and it
/// must not turn into a 100 ms deadline.
#[gpui::test]
async fn a_room_stream_outside_a_call_ticks_the_live_clock_once_per_second(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = fixture_with(cx, None, false);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.set_room_state(7, Vec::new(), vec![publication(7, "alice", 1)]);
        });
    });
    draw(&mut visual);
    let executor = executor(&mut visual);
    reset_polls(&mut visual, &voice);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_live_clock_second(),
            None,
            "the setup has not polled yet"
        );
    });
    advance(&mut visual, &executor, 1_000);
    assert_eq!(
        poll_count(&visual, &voice),
        1,
        "one second of a painted foreign stream is exactly one clock poll"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_live_clock_second().is_some(),
            "that poll recorded the on-air second"
        );
    });
}

/// The same painted foreign stream in a minimized window draws no `M:SS`, so
/// the one-second clock must stay asleep; activating the window brings the
/// label and its tick back.
#[gpui::test]
async fn a_minimized_idle_room_stream_does_not_tick_until_the_window_is_activated(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = fixture_with(cx, None, false);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.set_room_state(7, Vec::new(), vec![publication(7, "alice", 1)]);
            panel.test_set_minimized(Some(true));
        });
    });
    draw(&mut visual);
    let executor = executor(&mut visual);
    reset_polls(&mut visual, &voice);
    advance(&mut visual, &executor, 1_000);
    assert_eq!(
        poll_count(&visual, &voice),
        0,
        "a minimized window hides the stream label and must not poll"
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_live_clock_second(),
            None,
            "the hidden label never recorded a second"
        );
    });

    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_minimized(Some(false));
            panel.test_window_activated();
        });
    });
    visual.run_until_parked();
    assert!(
        poll_count(&visual, &voice) >= 1,
        "window activation wakes the painted stream label"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_live_clock_second().is_some(),
            "the activation poll recorded the on-air second"
        );
    });
}

/// The same idle room without a running stream has nothing to repaint, so the
/// clock stays asleep even though the room pane is painted.
#[gpui::test]
async fn an_idle_room_without_a_stream_does_not_tick(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture_with(cx, None, false);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.set_room_state(7, Vec::new(), Vec::new());
        });
    });
    draw(&mut visual);
    let executor = executor(&mut visual);
    reset_polls(&mut visual, &voice);
    advance(&mut visual, &executor, 1_000);
    assert_eq!(
        poll_count(&visual, &voice),
        0,
        "a painted room without a stream or a call has no reason to poll"
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_live_clock_second(), None);
    });
}

/// A queue transition from empty to non-empty wakes the panel without any
/// timer: the speaking state lands at once, exactly like an error or a
/// reconnect would.
#[gpui::test]
async fn a_pushed_update_wakes_the_panel_and_is_applied(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture_with(cx, None, true);
    draw(&mut visual);
    attach(&mut visual, &voice);
    reset_polls(&mut visual, &voice);
    let mut speaking = HashMap::new();
    speaking.insert("alice".to_string(), true);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_push_update(VoiceUpdate::Speaking(speaking));
        });
    });
    visual.run_until_parked();
    assert!(
        poll_count(&visual, &voice) >= 1,
        "the queue wake must poll without a timer"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.speaking("alice"),
            "the update was applied by that poll"
        );
    });
}

/// A prepared player picture wakes the panel, is taken by the event poll and
/// asks the worker for exactly one next picture. The frame deadline is not
/// touched by unrelated timer ticks.
#[gpui::test]
async fn a_published_frame_wakes_the_panel_and_requests_the_next_one(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture_with(cx, None, true);
    draw(&mut visual);
    attach(&mut visual, &voice);
    watch(&mut visual, &voice);
    reset_polls(&mut visual, &voice);
    let selection = voice
        .read_with(&*visual, |panel, _| panel.test_watching_selection())
        .expect("the player watches the publication");
    push_frame(&mut visual, &voice, selection);
    assert!(
        poll_count(&visual, &voice) >= 1,
        "a published picture wakes the panel"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_player_frame_image_id().is_some(),
            "the event poll took the prepared picture"
        );
        assert!(
            panel.test_take_frame_request(),
            "taking a picture requested exactly one next conversion"
        );
    });
}

/// Player redraw stays at the previous ten frames per second: a picture that
/// lands inside the deadline is not taken before it, and the 100 ms ticker
/// takes one picture per deadline.
#[gpui::test]
async fn the_player_keeps_the_ten_frames_per_second_deadline(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture_with(cx, None, true);
    draw(&mut visual);
    attach(&mut visual, &voice);
    watch(&mut visual, &voice);
    reset_polls(&mut visual, &voice);
    let executor = executor(&mut visual);
    let selection = voice
        .read_with(&*visual, |panel, _| panel.test_watching_selection())
        .expect("the player watches the publication");

    // The first picture has no previous deadline and is taken by the wake.
    push_frame(&mut visual, &voice, selection);
    let mut last = voice
        .read_with(&*visual, |panel, _| panel.test_player_frame_image_id())
        .expect("the first picture is shown");

    let mut shown = 0;
    for _ in 0..10 {
        // A picture published inside the current interval must not be taken
        // early, even though it wakes the panel.
        push_frame(&mut visual, &voice, selection);
        assert_eq!(
            voice.read_with(&*visual, |panel, _| panel.test_player_frame_image_id()),
            Some(last),
            "an event wake must not bypass the 100 ms frame deadline"
        );
        advance(&mut visual, &executor, 100);
        let next = voice
            .read_with(&*visual, |panel, _| panel.test_player_frame_image_id())
            .expect("the deadline takes a picture");
        assert_ne!(next, last, "the next deadline paints the newer picture");
        last = next;
        shown += 1;
    }
    assert_eq!(shown, 10, "one second contains exactly ten frame deadlines");
}

/// A minimized window keeps the player state but takes no picture; the 100 ms
/// ticker must not poll. Activating the window wakes the panel again.
#[gpui::test]
async fn a_minimized_player_takes_no_polls_until_the_window_is_activated(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture_with(cx, None, true);
    draw(&mut visual);
    attach(&mut visual, &voice);
    watch(&mut visual, &voice);
    let executor = executor(&mut visual);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| panel.test_set_minimized(Some(true)));
    });
    reset_polls(&mut visual, &voice);
    advance(&mut visual, &executor, 500);
    assert_eq!(
        poll_count(&visual, &voice),
        0,
        "a minimized player draws nothing and must not poll"
    );

    visual.update(|_window, app| {
        voice.update(app, |panel, _| panel.test_window_activated());
    });
    visual.run_until_parked();
    assert!(
        poll_count(&visual, &voice) >= 1,
        "window activation wakes the paused player"
    );
}

/// Stop/leave ends the fast work: the demand is released, the handle is gone
/// and neither the clock nor the frame ticker polls an idle panel.
#[gpui::test]
async fn stop_and_leave_release_the_preview_demand_and_stop_the_polls(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture_with(cx, None, true);
    attach(&mut visual, &voice);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                ..Sharing::default()
            });
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_screen_preview_demand().expect("a handle").0,
            "the painted live card requests the local preview"
        );
    });

    // The worker answer of a Stop arrives through the queue wake.
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_push_screen_sharing(Sharing::default());
        });
    });
    visual.run_until_parked();
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_phase(), Phase::Idle);
        assert!(
            !panel.test_screen_preview_demand().expect("a handle").0,
            "an Idle snapshot disables the preview demand"
        );
    });

    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.leave_in_window(window, cx));
    });
    visual.run_until_parked();
    let executor = executor(&mut visual);
    reset_polls(&mut visual, &voice);
    advance(&mut visual, &executor, 1_000);
    assert_eq!(
        poll_count(&visual, &voice),
        0,
        "after Leave neither the clock nor the player ticker polls"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.call_active(), "the call ended");
        assert!(
            panel.test_screen_preview_demand().is_none(),
            "no handle means no preview demand"
        );
    });
}

/// A published local preview wakes the panel and is taken by the visible card
/// without any timer: the demand was enabled by the painted card, and the
/// capture publishes at its own throttle.
#[gpui::test]
async fn a_published_preview_wakes_the_panel_and_is_shown(cx: &mut TestAppContext) {
    use voxhold_media::screen_capture::PreviewFrame;

    let (mut visual, _view, voice) = fixture_with(cx, None, true);
    attach(&mut visual, &voice);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                ..Sharing::default()
            });
        });
    });
    draw(&mut visual);
    reset_polls(&mut visual, &voice);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_push_screen_preview(PreviewFrame {
                width: 4,
                height: 4,
                bgra: vec![0x40; 4 * 4 * 4],
            });
        });
    });
    visual.run_until_parked();
    assert!(
        poll_count(&visual, &voice) >= 1,
        "a prepared preview wakes the hidden-from-timers panel"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_screen_preview_image_id().is_some(),
            "the event poll installed the preview picture"
        );
        assert_eq!(
            panel.test_screen_preview_stats().1,
            1,
            "the visible card counted exactly one shown preview"
        );
    });
}

/// An idle panel is released while its event task is parked on the media
/// wake. Release must wake that task once; otherwise it would wait for an
/// update of a dead entity until the app quits.
#[gpui::test]
async fn a_released_idle_panel_ends_its_parked_event_task(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture_with(cx, None, false);
    draw(&mut visual);
    let tasks = voice.read_with(&*visual, |panel, _| panel.test_event_tasks());
    assert_eq!(
        tasks.load(Ordering::Relaxed),
        1,
        "the panel runs exactly one event task"
    );
    // The fixture view keeps a second strong handle to the panel.
    visual.update(|_window, app| {
        view.update(app, |view, _| view.voice = None);
    });
    drop(voice);
    // The next app update flushes the dropped entity and runs its `Drop`.
    visual.update(|_window, _app| {});
    visual.run_until_parked();
    assert_eq!(
        tasks.load(Ordering::Relaxed),
        0,
        "release woke the task and it ended instead of staying parked"
    );
}

/// The O02 minor: a preview frame taken while the card is not visible is
/// discarded by the same predicate as the demand, so no `RenderImage` is
/// created and `shown` does not move.
#[gpui::test]
async fn a_hidden_or_minimized_card_discards_the_preview_frame(cx: &mut TestAppContext) {
    use voxhold_media::screen_capture::PreviewFrame;

    let (mut visual, _view, voice) = fixture_with(cx, None, true);
    attach(&mut visual, &voice);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                ..Sharing::default()
            });
        });
    });
    draw(&mut visual);

    let frame = || PreviewFrame {
        width: 4,
        height: 4,
        bgra: vec![0x40; 4 * 4 * 4],
    };

    // Minimized window: the demand is off, so the prepared picture must be
    // drained without becoming a texture.
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_minimized(Some(true));
            panel.test_push_screen_preview(frame());
            assert!(!panel.test_take_screen_preview(window));
            assert_eq!(panel.test_screen_preview_image_id(), None);
            assert_eq!(
                panel.test_screen_preview_stats().1,
                0,
                "a discarded picture is not counted as shown"
            );
            assert!(!panel.test_screen_frame_pending(), "the slot was drained");
        });
    });

    // Hidden room pane: the card still exists in the geometry but the demand
    // is off for the same reason.
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_minimized(Some(false));
            panel.set_room_painted(false);
            panel.test_push_screen_preview(frame());
            assert!(!panel.test_take_screen_preview(window));
            assert_eq!(panel.test_screen_preview_image_id(), None);
            assert_eq!(panel.test_screen_preview_stats().1, 0);
            assert!(!panel.test_screen_frame_pending());
        });
    });
}
