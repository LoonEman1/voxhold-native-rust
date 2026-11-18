//! UI gate of the screen publication: the «Экран» action must be unavailable
//! without a confirmed join, the Stream permission or a free publisher slot.

use crate::voice_room::CallPhase;
use gpui::TestAppContext;

fn source() -> voxhold_media::screen_capture::ScreenSource {
    voxhold_media::screen_capture::ScreenSource {
        id: 3,
        kind: voxhold_media::screen_capture::SourceKind::Monitor,
        title: "Экран 1".into(),
        display_id: -1,
        width: 1920,
        height: 1080,
    }
}

fn draw(visual: &mut gpui::VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

/// Open the picker through the test seam and paint it once.
fn open_picker(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
) {
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker(source());
        });
    });
    // The focus request is deferred to the frame callback; the second draw
    // installs it on the rendered focus path.
    draw(visual);
    draw(visual);
}

fn focused(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
) -> Option<String> {
    visual.update(|window, app| voice.read(app).test_focused_share(window))
}

fn publication(index: usize) -> voxhold_media::controller::Publication {
    voxhold_media::controller::Publication {
        server_id: 1,
        channel_id: 7,
        publisher_user_id: 100 + index as i64,
        publisher_connection_id: format!("publisher-{index}"),
        mode: "server".into(),
        codec: "vp8".into(),
        has_audio: false,
        viewer_count: 0,
        started_at_ms: 0,
    }
}

#[gpui::test]
async fn the_screen_action_needs_a_confirmed_join_permission_and_slot(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            // Permission Stream on the channel is the first gate.
            panel.set_stream_permission(false);
            let error = panel.share_gate().expect_err("no Stream permission");
            assert!(error.contains("права"), "{error}");

            // The join must be confirmed in this room (the fixture starts
            // connected and then loses the call).
            panel.set_stream_permission(true);
            panel.test_set_call(0, CallPhase::Idle, Vec::new(), Vec::new());
            let error = panel.share_gate().expect_err("not in the call");
            assert!(error.contains("подключитесь"), "{error}");
            panel.test_set_call(7, CallPhase::Connected, Vec::new(), Vec::new());

            // A full channel has no publisher slot, independent of the host
            // capture stack.
            panel.test_set_streams((0..8).map(publication).collect());
            let error = panel.share_gate().expect_err("channel full");
            assert!(error.contains("8 трансляций"), "{error}");
            panel.test_set_streams(Vec::new());

            // With every server-side gate open the answer depends only on the
            // local capture/codec capability; the test states which it got
            // instead of pretending capture always exists.
            let capability = voxhold_media::screen_capture::capability_cached();
            let codec = voxhold_media::publisher::Codec::available();
            match panel.share_gate() {
                Ok(()) => {
                    assert!(capability.is_ok(), "gate passed without a capturer");
                    assert!(!codec.is_empty(), "gate passed without a codec");
                }
                Err(error) => {
                    assert!(
                        capability.is_err() || codec.is_empty(),
                        "gate failed for an unexpected reason: {error}"
                    );
                }
            }
        });
    });
}

#[gpui::test]
async fn an_unconfirmed_acl_disables_the_screen_action(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.set_stream_permission(true);
            panel.set_access_confirmed(false);
            let error = panel.share_gate().expect_err("ACL is stale");
            assert!(error.contains("подтверждены"), "{error}");
            panel.set_access_confirmed(true);
        });
    });
}

#[gpui::test]
async fn the_picker_takes_focus_and_the_keyboard_selects_and_starts(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.screen_share_open(), "the picker opened");
        assert!(
            panel.test_picker_focus_is_separate(),
            "the picker handles must not overlap the toolbar or the on-air card"
        );
    });
    assert_eq!(
        focused(&mut visual, &voice),
        Some("Monitors".to_owned()),
        "the modal owns the keyboard from the first frame"
    );
    assert!(
        visual.debug_bounds("screen-share-card").is_some(),
        "the picker must be painted"
    );

    // Tab to the only source row and select it with Enter.
    visual.simulate_keystrokes("tab");
    visual.simulate_keystrokes("tab");
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual, &voice), Some("Source(0)".to_owned()));
    visual.simulate_keystrokes("enter");

    // Walk to Start and run it.
    for _ in 0..24 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(focused(&mut visual, &voice), Some("Start".to_owned()));
    visual.simulate_keystrokes("enter");

    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(
        commands,
        vec![("publish", 7)],
        "Enter on Start publishes the selected source"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.screen_share_open(), "the dialog waits for the worker");
    });
}

#[gpui::test]
async fn escape_before_start_only_cancels_the_picker(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    visual.simulate_keystrokes("escape");
    draw(&mut visual);
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(
        commands,
        vec![("cancel", 7)],
        "closing before Start never touches the publication"
    );
    voice.read_with(&*visual, |panel, _| assert!(!panel.screen_share_open()));
}

#[gpui::test]
async fn escape_after_start_stops_the_committed_publication(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    // Select the source and start it with the keyboard.
    for _ in 0..3 {
        visual.simulate_keystrokes("tab");
    }
    visual.simulate_keystrokes("enter");
    for _ in 0..24 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    visual.simulate_keystrokes("enter");
    let published = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(published, vec![("publish", 7)]);

    // Escape while the attempt is committed is a stop, not a stale cancel.
    visual.simulate_keystrokes("escape");
    draw(&mut visual);
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(
        commands,
        vec![("stop", 0)],
        "Escape sends exactly one stop and no cancel: {commands:?}"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.screen_share_open());
        assert!(!panel.screen_share_busy());
    });
}

#[gpui::test]
async fn the_disabled_reason_is_hidden_while_a_share_is_active(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Sharing};
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_share_gate(Err("Трансляция уже идёт."));
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                ..Sharing::default()
            });
            assert!(panel.share_gate().is_err(), "the action is really blocked");
            assert!(
                panel.test_screen_reason().is_none(),
                "an active share shows its card, not the permanent reason"
            );

            // A non-active failure of the gate still explains itself.
            panel.test_set_screen_sharing(Sharing::default());
            panel.test_set_share_gate(Err("Нет доступа к голосовому каналу."));
            assert_eq!(
                panel.test_screen_reason(),
                Some("Нет доступа к голосовому каналу.")
            );
        });
    });
}
