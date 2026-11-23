//! PiP and navigation regressions (M01–M07 of the streaming plan).
//!
//! The tests drive the real shell paint, the same panel methods the buttons
//! call and the real event routing seams. Assertions are about final commands,
//! identifiers, surfaces and bounds, not the mere presence of an element.

use super::voice_screen_tests::{draw, monitor};
use super::voice_tests::fixture;
use crate::voice_panel::PipTab;
use crate::voice_room::{PIP_MARGIN, PIP_WIDTH};
use gpui::{px, size, TestAppContext};
use voxhold_media::controller::Publication;
use voxhold_media::publisher::{
    Codec, Confirmed, Phase, Quality, QualityOutcome, QualityStatus, Resolution, Sharing,
};
use voxhold_media::runtime::VoiceUpdate;
use voxhold_media::system_audio::AudioScope;

fn publication(channel: i64, publisher: &str, user: i64, has_audio: bool) -> Publication {
    Publication {
        server_id: 1,
        channel_id: channel,
        publisher_user_id: user,
        publisher_connection_id: publisher.into(),
        mode: "server".into(),
        codec: "vp8".into(),
        has_audio,
        viewer_count: 1,
        started_at_ms: 1,
    }
}

fn source() -> voxhold_media::screen_capture::ScreenSource {
    monitor(3, "Экран 1", 1920, 1080)
}

fn confirmed() -> Confirmed {
    Confirmed {
        attempt: 1,
        source: source(),
        quality: Quality::default(),
        codec: Codec::Vp8,
        audio: true,
        audio_scope: AudioScope::Device,
    }
}

fn live_sharing() -> Sharing {
    Sharing {
        phase: Phase::Live,
        attempt: 1,
        live: true,
        quality: Some(Quality::default()),
        codec: Some(Codec::Vp8),
        confirmed: Some(confirmed()),
        has_audio: true,
        ..Sharing::default()
    }
}

/// Start a real publication intent in room A and put it Live. Returns the one
/// publish command the test drained.
fn start_own(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
) {
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_prepare_screen_publish(source());
            panel.test_start_screen_share(cx);
        });
    });
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(commands, vec![("publish", 7)], "one start in A");
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_sharing(live_sharing());
        });
    });
}

/// Watch a publication from the call channel A through the production path.
fn start_view(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
    has_audio: bool,
) {
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.watch(publication(7, "publisher-1", 2, has_audio), cx);
        });
    });
    let intents =
        visual.update(|_window, app| voice.update(app, |panel, _| panel.test_take_watch_intents()));
    assert_eq!(intents.len(), 1, "one watch intent");
}

fn show_channel(
    visual: &mut gpui::VisualTestContext,
    view: &gpui::Entity<super::Voxhold>,
    channel: i64,
) {
    visual.update(|window, app| {
        view.update(app, |view, cx| {
            // A channel click leaves the personal surface the same way
            // `open_server` does; the helper must not keep `personal_mode`.
            view.personal_mode = false;
            view.personal_return = None;
            view.pending_return_channel = None;
            view.channel = channel;
            if channel == 7 || channel == 8 {
                view.open_voice_room(channel, window, cx);
            } else {
                cx.notify();
            }
        });
    });
    draw(visual);
}

fn commands(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
) -> Vec<(&'static str, u64)> {
    visual.update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()))
}

fn leaves(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
) -> Vec<u64> {
    visual.update(|_window, app| voice.update(app, |panel, _| panel.test_take_leave_intents()))
}

#[gpui::test]
async fn m01_own_broadcast_takes_the_source_slot_and_nothing_else(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    start_own(&mut visual, &voice);
    draw(&mut visual);
    assert!(
        visual.debug_bounds("screen-on-air").is_some(),
        "the source room grid owns the card"
    );
    assert!(
        visual.debug_bounds("stream-pip").is_none(),
        "no PiP while the source room is shown"
    );
    let attempt = visual.update(|_w, app| voice.read(app).test_handle_attempt());
    let phase = visual.update(|_w, app| voice.read(app).test_screen_phase());

    for channel in [9, 8] {
        show_channel(&mut visual, &view, channel);
        assert!(
            visual.debug_bounds("stream-pip").is_some(),
            "channel {channel} shows the compact PiP"
        );
        // `debug_bounds` keeps the last paint of a selector, so the slot
        // decision itself is asserted on the panel.
        assert!(
            !visual.update(|_w, app| voice.read(app).screen_card_active()),
            "a foreign grid never takes the own card"
        );
    }

    // A personal surface (a direct conversation) is not the source room.
    visual.update(|_window, app| {
        view.update(app, |view, cx| {
            view.personal_mode = true;
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(visual.debug_bounds("stream-pip").is_some());
    assert!(!visual.update(|_w, app| voice.read(app).screen_card_active()));

    show_channel(&mut visual, &view, 7);
    assert!(
        visual.update(|_w, app| voice.read(app).screen_card_active()),
        "returning to A restores the source card"
    );
    assert!(
        !visual.update(|_w, app| voice.read(app).test_pip_visible()),
        "the PiP disappears in the source room"
    );
    assert!(
        commands(&mut visual, &voice).is_empty(),
        "navigation never starts or stops the publication"
    );
    assert!(leaves(&mut visual, &voice).is_empty());
    visual.update(|_w, app| {
        let panel = voice.read(app);
        assert_eq!(
            panel.test_handle_attempt(),
            attempt,
            "the worker is unchanged"
        );
        assert_eq!(panel.test_screen_phase(), phase);
        assert_eq!(panel.test_publish_room(), Some(7), "the owner stays A");
    });
}

#[gpui::test]
async fn m02_viewer_keeps_one_selection_and_survives_fullscreen(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    start_view(&mut visual, &voice, true);
    draw(&mut visual);
    assert!(
        visual.debug_bounds("voice-player").is_some(),
        "the source room uses the standard player"
    );
    assert!(visual.debug_bounds("stream-pip").is_none());
    let selection = visual.update(|_w, app| voice.read(app).watching().map(|_| ()));

    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.player_toggle_fullscreen(window));
    });
    draw(&mut visual);
    assert!(visual.debug_bounds("stream-fullscreen-player").is_some());
    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.exit_fullscreen(window));
    });
    draw(&mut visual);
    visual.update(|_w, app| {
        let panel = voice.read(app);
        assert!(!panel.player_fullscreen(), "fullscreen is released");
        assert!(panel.player_open(), "the watch survives the exit");
    });

    for channel in [9, 8] {
        show_channel(&mut visual, &view, channel);
        assert!(visual.debug_bounds("stream-pip").is_some());
        assert!(visual.update(|_w, app| voice.read(app).test_pip_visible()));
        assert!(visual.update(|_w, app| voice.read(app).player_open()));
    }
    show_channel(&mut visual, &view, 7);
    assert!(visual.update(|_w, app| {
        !voice.read(app).test_pip_visible() && voice.read(app).player_open()
    }));
    visual.update(|_w, app| {
        let panel = voice.read(app);
        assert!(panel.watching().is_some());
        assert_eq!(panel.call_channel(), 7, "the voice call never moved");
        let _ = selection;
    });
    assert!(
        commands(&mut visual, &voice).is_empty(),
        "no start/stop from viewing"
    );
    assert!(
        leaves(&mut visual, &voice).is_empty(),
        "navigation sent no leave"
    );
}

#[gpui::test]
async fn m03_two_roles_share_one_pip_and_close_their_own_command(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    start_own(&mut visual, &voice);
    start_view(&mut visual, &voice, true);
    show_channel(&mut visual, &view, 9);

    assert!(visual.debug_bounds("stream-pip").is_some());
    assert!(visual.debug_bounds("pip-tab-view").is_some());
    assert!(visual.debug_bounds("pip-tab-own").is_some());
    visual.update(|_w, app| {
        assert_eq!(voice.read(app).test_pip_tab(), PipTab::View);
    });
    visual.update(|_w, app| {
        voice.update(app, |panel, cx| panel.test_switch_pip_tab(PipTab::Own, cx));
    });
    draw(&mut visual);
    visual.update(|_w, app| {
        assert_eq!(voice.read(app).test_pip_tab(), PipTab::Own);
    });

    // Close the own role: exactly one stop, no leave, the viewer stays.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.test_close_pip_role(window, cx));
    });
    assert_eq!(commands(&mut visual, &voice), vec![("stop", 0)]);
    assert!(leaves(&mut visual, &voice).is_empty());
    // A repeated command is inert until a new attempt starts.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.test_close_pip_role(window, cx));
    });
    assert!(
        commands(&mut visual, &voice).is_empty(),
        "one stop per attempt"
    );
    visual.update(|_w, app| {
        voice.update(app, |panel, _| {
            panel.test_apply_screen_sharing(Sharing::default())
        });
    });
    draw(&mut visual);
    visual.update(|_w, app| {
        let panel = voice.read(app);
        assert!(panel.call_active(), "voice is still connected");
        assert!(panel.player_open(), "the remaining view stays");
        assert!(panel.test_pip_visible());
    });

    // Close the viewer: exactly one leave, no stop.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.test_close_pip_role(window, cx));
    });
    assert_eq!(leaves(&mut visual, &voice).len(), 1);
    assert!(commands(&mut visual, &voice).is_empty());
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.test_close_pip_role(window, cx));
    });
    assert!(
        leaves(&mut visual, &voice).is_empty(),
        "one leave per selection"
    );
}

#[gpui::test]
async fn m04_live_settings_belong_to_the_owner_channel(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    // A grants Stream, B does not. The owner's access must survive the
    // navigation to B.
    visual.update(|window, app| {
        view.update(app, |view, cx| {
            view.channels[0].permissions |= crate::settings::PERMISSION_STREAM;
            view.open_voice_room(7, window, cx);
        });
    });
    draw(&mut visual);
    start_own(&mut visual, &voice);
    show_channel(&mut visual, &view, 8);
    visual.update(|_w, app| {
        voice.update(app, |panel, cx| {
            // B has no Stream permission in this scenario.
            panel.set_stream_permission(false);
            panel.test_switch_pip_tab(PipTab::Own, cx);
        });
    });
    draw(&mut visual);

    // Starting a broadcast in B obeys B and is blocked.
    visual.update(|_w, app| {
        voice.update(app, |panel, _| panel.test_clear_share_gate_override());
        let panel = voice.read(app);
        let error = panel.share_gate().expect_err("B blocks the primary start");
        assert!(error.contains("подключитесь"), "{error}");
    });

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_pip_settings(window, cx);
            assert!(panel.test_screen_restart(), "the owner dialog opened");
            panel.test_change_screen_bitrate(1000, cx);
            panel.test_start_screen_share(cx);
        });
    });
    let (first, op) = visual.update(|_w, app| {
        voice.update(app, |panel, _| {
            (
                panel.test_take_screen_commands(),
                panel.test_pending_quality_op(),
            )
        })
    });
    assert_eq!(
        first,
        vec![("bitrate", op.expect("bitrate operation"))],
        "a bitrate change never restarts"
    );
    let op = op.unwrap();
    visual.update(|_w, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_screen_sharing(Sharing {
                quality_status: Some(QualityStatus {
                    op,
                    outcome: QualityOutcome::Applied,
                }),
                ..live_sharing()
            });
            cx.notify();
        });
    });
    draw(&mut visual);

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_pip_settings(window, cx);
            panel.test_set_screen_resolution(Resolution::P720, cx);
            panel.test_start_screen_share(cx);
        });
    });
    let (second, op) = visual.update(|_w, app| {
        voice.update(app, |panel, _| {
            (
                panel.test_take_screen_commands(),
                panel.test_pending_quality_op(),
            )
        })
    });
    assert_eq!(
        second,
        vec![("restart", op.expect("codec/resolution operation"))],
        "a profile change goes through preflight/restart"
    );
}

#[gpui::test]
async fn m05_viewer_settings_own_the_focus_and_send_one_command(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    start_view(&mut visual, &voice, true);
    show_channel(&mut visual, &view, 9);
    visual.update(|_w, app| {
        assert!(
            voice.read(app).test_screen_sharing().phase != Phase::Live,
            "the viewer is not a publisher"
        );
    });
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.test_open_pip_settings(window, cx));
    });
    draw(&mut visual);
    visual.update(|_w, app| {
        let panel = voice.read(app);
        assert!(panel.test_pip_settings_open());
        assert!(panel.test_screen_preview_present() == false);
    });
    assert!(
        visual.debug_bounds("pip-settings-audio").is_some(),
        "a track offers the existing mute toggle"
    );
    assert!(
        visual.debug_bounds("pip-settings-fullscreen").is_some(),
        "the fullscreen action is available"
    );

    // The settings panel keeps the focus ring to itself and Escape closes it.
    let focused = |visual: &mut gpui::VisualTestContext| {
        visual.update(|window, app| voice.read(app).test_pip_focused(window))
    };
    visual.update(|window, app| {
        let handle = voice.read(app).pip_settings_focus_for_tests();
        window.focus(&handle);
    });
    let first = focused(&mut visual);
    assert_eq!(first, Some("pip-settings-audio"));
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("pip-settings-fullscreen"));
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("pip-settings-leave"));
    visual.simulate_keystrokes("shift-tab");
    assert_eq!(focused(&mut visual), Some("pip-settings-fullscreen"));
    visual.simulate_keystrokes("escape");
    draw(&mut visual);
    visual.update(|_w, app| {
        assert!(!voice.read(app).test_pip_settings_open());
    });
    assert_eq!(
        focused(&mut visual),
        Some("pip-settings"),
        "focus returns to the gear while the PiP is alive"
    );

    // The audio toggle changes only the viewer track.
    visual.update(|_w, app| {
        voice.update(app, |panel, cx| panel.toggle_stream_mute(cx));
        let panel = voice.read(app);
        assert!(panel.stream_muted());
        assert!(panel.call_active(), "voice is untouched");
        assert_eq!(panel.test_publish_room(), None, "no own publication");
    });

    // Without a track the panel offers the fullscreen action instead.
    let (mut quiet, quiet_view, quiet_voice) = fixture(cx);
    draw(&mut quiet);
    start_view(&mut quiet, &quiet_voice, false);
    show_channel(&mut quiet, &quiet_view, 9);
    quiet.update(|window, app| {
        quiet_voice.update(app, |panel, cx| panel.test_open_pip_settings(window, cx));
    });
    draw(&mut quiet);
    assert!(
        quiet.debug_bounds("pip-settings-audio").is_none(),
        "no audio row without a track"
    );
    assert!(
        quiet.debug_bounds("pip-settings-fullscreen").is_some(),
        "the fullscreen action is offered instead"
    );
    quiet.update(|window, app| {
        quiet_voice.update(app, |panel, cx| panel.test_close_pip_role(window, cx));
    });
    assert_eq!(
        quiet
            .update(|_w, app| quiet_voice.update(app, |panel, _| panel.test_take_leave_intents()))
            .len(),
        1
    );
}

#[gpui::test]
async fn m06_compact_box_clamps_on_drag_edge_pointer_loss_and_narrow_area(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    start_own(&mut visual, &voice);
    show_channel(&mut visual, &view, 9);
    let boxed = visual.update(|_w, app| voice.read(app).test_pip_box().expect("PiP box"));
    assert!(
        (boxed.w - PIP_WIDTH).abs() < 0.5,
        "compact width: {boxed:?}"
    );
    let area = visual.update(|_w, app| voice.read(app).test_player_area());
    assert!((boxed.right() - (area.right() - PIP_MARGIN)).abs() < 0.5);
    assert!((boxed.bottom() - (area.bottom() - PIP_MARGIN)).abs() < 0.5);

    // A grip drag beyond every edge clamps the box into the safe area.
    let header = visual.update(|_w, app| {
        let panel = voice.read(app);
        let b = panel.test_pip_box().unwrap();
        (b.x + 20.0, b.y + 10.0)
    });
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.player_pointer_down(header.0, header.1), "header drag");
            panel.player_pointer_moved(-4000.0, -4000.0, true, cx);
        });
    });
    let moved = visual.update(|_w, app| voice.read(app).test_pip_box().unwrap());
    assert!(moved.x >= area.x + PIP_MARGIN - 0.5);
    assert!(moved.y >= area.y + PIP_MARGIN - 0.5);
    // Releasing outside the window arrives as an unpressed move and ends it.
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.player_pointer_moved(5000.0, 5000.0, false, cx);
            assert!(!panel.player_dragging(), "release outside ends the drag");
        });
    });

    // Buttons never start a drag.
    let (gear, close) = visual.update(|_w, app| {
        let panel = voice.read(app);
        let b = panel.test_pip_box().unwrap();
        (
            (b.right() - 84.0, b.y + 19.0),
            (b.right() - 20.0, b.y + 19.0),
        )
    });
    visual.update(|_w, app| {
        voice.update(app, |panel, _| {
            assert!(!panel.player_pointer_down(gear.0, gear.1));
            assert!(!panel.player_pointer_down(close.0, close.1));
        });
    });

    // A narrow window shrinks the width inside the safe rect.
    visual.simulate_resize(size(px(240.), px(600.)));
    draw(&mut visual);
    let narrow = visual.update(|_w, app| voice.read(app).test_pip_box().unwrap());
    let narrow_area = visual.update(|_w, app| voice.read(app).test_player_area());
    assert!(
        narrow.right() <= narrow_area.right() - PIP_MARGIN + 0.5,
        "narrow box stays inside: {narrow:?}"
    );
    assert!(narrow.x >= narrow_area.x + PIP_MARGIN - 0.5);
    assert!(narrow.w <= 240.0 - PIP_MARGIN * 2.0 + 0.5);

    // The remembered position survives a room round trip at the wide size.
    visual.simulate_resize(size(px(1200.), px(760.)));
    draw(&mut visual);
    visual.update(|_w, app| {
        voice.update(app, |panel, _| {
            panel.test_set_pip_position((120.0, 90.0));
        });
    });
    show_channel(&mut visual, &view, 7);
    assert!(visual.debug_bounds("screen-on-air").is_some());
    show_channel(&mut visual, &view, 9);
    visual.update(|_w, app| {
        assert_eq!(voice.read(app).test_pip_position(), Some((120.0, 90.0)));
    });

    // DPI changes the physical viewport, not the logical safe area.
    visual.update(|_w, app| {
        voice.update(app, |panel, _| {
            panel.set_scale(1.5);
        });
        let panel = voice.read(app);
        assert_eq!(panel.test_pip_position(), Some((120.0, 90.0)));
    });
}

#[gpui::test]
async fn m07_reconnect_stale_events_and_revoke_never_restore_a_foreign_card(
    cx: &mut TestAppContext,
) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    start_own(&mut visual, &voice);
    start_view(&mut visual, &voice, true);
    show_channel(&mut visual, &view, 9);

    // The reconnect status of the current attempt keeps the last frame.
    let selection = visual.update(|_w, app| voice.read(app).test_watching_selection());
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                VoiceUpdate::StreamStatus {
                    selection: selection.expect("watch selection"),
                    status: "Восстанавливаем соединение…".into(),
                },
                window,
                cx,
            );
        });
    });
    draw(&mut visual);
    visual.update(|_w, app| {
        let panel = voice.read(app);
        assert!(panel.player_open(), "the attempt and its frame stay");
        assert!(panel.test_pip_visible());
    });

    // A late close of another selection never closes this one.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                VoiceUpdate::StreamStatus {
                    selection: 9999,
                    status: String::new(),
                },
                window,
                cx,
            );
        });
    });
    visual.update(|_w, app| {
        assert!(voice.read(app).player_open());
    });

    // A revoke closes the owner's live dialog and the PiP image immediately.
    visual.update(|_w, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_pip_settings(_w, cx);
            panel.set_publish_access(false, false, true);
            assert!(!panel.screen_share_open());
        });
    });
    draw(&mut visual);
    visual.update(|_w, app| {
        let panel = voice.read(app);
        assert!(!panel.test_screen_preview_present(), "no stale own frame");
    });

    // Leaving the call and abandoning the session clears every surface.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.abandon(window, cx));
    });
    draw(&mut visual);
    visual.update(|_w, app| {
        let panel = voice.read(app);
        assert!(!panel.test_pip_visible());
        assert!(!panel.player_open());
        assert_eq!(panel.test_publish_room(), None);
        assert!(panel.test_pip_position().is_none());
    });
    // `debug_bounds` keeps the last painted frame, so the surface decision is
    // asserted on the panel and the painted PiP is checked after a fresh draw
    // at a text channel where it was never mounted.
    assert!(!visual.update(|_w, app| voice.read(app).test_pip_visible()));
    assert!(!visual.update(|_w, app| voice.read(app).test_screen_card_active()));
}
