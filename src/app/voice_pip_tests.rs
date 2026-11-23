//! Regressions for the in-client PiP of an own publication or a watched
//! stream while the user navigates. They drive the real shell and read the
//! painted bounds plus the commands the UI hands to the media layer.

use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use voxhold_media::controller::Publication;
use voxhold_media::publisher::{Phase, Sharing};
use voxhold_media::runtime::VoiceUpdate;
use voxhold_media::screen_capture::ScreenSource;

pub(super) fn publication(channel_id: i64, publisher: &str, user: i64) -> Publication {
    Publication {
        server_id: 1,
        channel_id,
        publisher_user_id: user,
        publisher_connection_id: publisher.into(),
        mode: "server".into(),
        codec: "vp8".into(),
        has_audio: true,
        viewer_count: 1,
        started_at_ms: 1,
    }
}

pub(super) fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

pub(super) fn live(attempt: u64) -> Sharing {
    Sharing {
        phase: Phase::Live,
        attempt,
        live: true,
        ..Sharing::default()
    }
}

pub(super) fn source() -> ScreenSource {
    super::voice_screen_tests::monitor(1, "Экран 1", 1920, 1080)
}

pub(super) fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"));
    visual.simulate_click(bounds.center(), Modifiers::default());
}

pub(super) fn center(rect: crate::voice_room::Rect) -> (f32, f32) {
    (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0)
}

/// Start one own publication through the production intent and then install
/// the snapshot the media layer would report for it.
pub(super) fn publish_live(
    panel: &mut voice_panel::VoicePanel,
    window: &mut Window,
    cx: &mut Context<voice_panel::VoicePanel>,
    attempt: u64,
) {
    panel.test_attach_detached_handle();
    panel.set_stream_permission(true);
    panel.test_set_share_gate(Ok(()));
    panel.test_prepare_screen_publish(window, source());
    panel.test_start_screen_share(window, cx);
    panel.test_set_screen_sharing(live(attempt));
    panel.test_set_screen_confirmed_source(source());
}

pub(super) fn watch_publication(
    panel: &mut voice_panel::VoicePanel,
    window: &mut Window,
    cx: &mut Context<voice_panel::VoicePanel>,
) -> u64 {
    panel.watch(publication(7, "bob-screen", 2), window, cx);
    panel
        .test_watching_selection()
        .expect("the viewer owns a selection")
}

pub(super) fn open_room(visual: &mut VisualTestContext, view: &Entity<Voxhold>, channel: i64) {
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channel = channel;
            if v.channels
                .iter()
                .any(|item| item.id == channel && item.kind == "voice")
            {
                v.open_voice_room(channel, window, cx);
            }
            cx.notify();
        });
    });
    draw(visual);
}

/// M01: the own card belongs to A. Text, another voice room and the personal
/// surface show only the PiP, and navigation changes no media intent.
#[gpui::test]
async fn m01_the_own_publication_stays_in_its_room(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| publish_live(panel, window, cx, 3));
    });
    draw(&mut visual);
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands());
    let attempt = voice.read_with(&*visual, |panel, _| panel.test_handle_attempt());
    assert!(
        visual.debug_bounds("screen-on-air").is_some(),
        "A paints the on-air card"
    );
    assert_eq!(
        visual.debug_bounds("voice-pip"),
        None,
        "inside A the card owns the slot instead of a PiP"
    );

    // A text channel keeps the publication but leaves the room grid.
    open_room(&mut visual, &view, 9);
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.test_screen_pip_active());
        assert!(
            !panel.test_screen_card_in_room(),
            "a text channel has no «Ваш экран» card"
        );
        assert_eq!(panel.test_screen_phase(), Phase::Live);
    });
    assert!(
        visual.debug_bounds("voice-pip").is_some(),
        "the publication is visible as a PiP"
    );

    // Another voice room: no extra grid slot, only the PiP.
    open_room(&mut visual, &view, 8);
    voice.read_with(&*visual, |panel, _| {
        assert!(
            !panel.test_screen_card_in_room(),
            "room B must not show a card of room A"
        );
        assert!(panel.test_screen_pip_active());
    });
    assert!(visual.debug_bounds("voice-pip").is_some());

    // The personal surface (Messages/DM).
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(visual.debug_bounds("voice-pip").is_some());

    // Back to A: the card returns, the same attempt and worker stay.
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = false;
            cx.notify();
        });
    });
    open_room(&mut visual, &view, 7);
    assert!(
        visual.debug_bounds("screen-on-air").is_some(),
        "the owner room gets its card back"
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_phase(), Phase::Live);
        assert_eq!(panel.test_handle_attempt(), attempt, "the same session");
    });
    let commands = voice.update(&mut *visual, |panel, _| {
        (
            panel.test_take_screen_commands(),
            panel.test_take_stream_commands(),
        )
    });
    assert_eq!(commands.0, Vec::new(), "no stop/start from navigation");
    assert_eq!(commands.1, Vec::new(), "no watch/leave from navigation");
}

/// M02: one viewer selection survives the same route and fullscreen changes
/// the picture surface only.
#[gpui::test]
async fn m02_the_viewer_survives_navigation_and_fullscreen(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    let selection = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            watch_publication(panel, window, cx)
        })
    });
    draw(&mut visual);
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands());
    assert!(
        visual.debug_bounds("voice-player").is_some(),
        "the standard player is used in the owner room"
    );

    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.player_toggle_fullscreen(window));
    });
    draw(&mut visual);
    assert!(voice.read_with(&*visual, |panel, _| panel.player_fullscreen()));
    // Leaving fullscreen first keeps the same viewer selection.
    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.exit_fullscreen(window));
    });
    draw(&mut visual);
    assert!(
        !voice.read_with(&*visual, |panel, _| panel.player_fullscreen()),
        "leaving fullscreen keeps the viewer"
    );

    open_room(&mut visual, &view, 9);
    assert!(
        visual.debug_bounds("voice-pip").is_some(),
        "outside A the same viewer is a PiP"
    );
    assert!(voice.read_with(&*visual, |panel, _| {
        panel.test_watching_selection() == Some(selection)
    }));
    assert!(voice.read_with(&*visual, |panel, _| panel.test_pip_active()));

    open_room(&mut visual, &view, 7);
    assert!(
        visual.debug_bounds("voice-player").is_some(),
        "returning to A restores the standard player"
    );
    let commands = voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands());
    assert_eq!(commands, Vec::new(), "route and fullscreen add no intents");
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_watching_selection()),
        Some(selection)
    );
}

/// M03: both roles, tabs, and one close command per role.
#[gpui::test]
async fn m03_each_role_closes_with_one_command(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    let selection = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            publish_live(panel, window, cx, 4);
            watch_publication(panel, window, cx)
        })
    });
    open_room(&mut visual, &view, 8);
    let setup_stream = voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands());
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands());
    assert_eq!(
        setup_stream,
        vec![("watch", selection)],
        "the setup queued exactly one watch"
    );
    assert!(voice.read_with(&*visual, |panel, _| {
        panel.test_screen_pip_active() && panel.player_open()
    }));
    assert!(
        visual.debug_bounds("pip-tab-viewer").is_some()
            && visual.debug_bounds("pip-tab-publisher").is_some(),
        "both roles offer their tab"
    );
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_pip_role()),
        crate::voice_panel::PipRole::Viewer,
        "the open viewer is the default surface"
    );

    // Close the viewer: exactly one leave, no stop.
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_handle_attempt().is_some(),
            "the detached worker is still installed"
        );
        assert_eq!(
            panel.test_watching_selection(),
            Some(selection),
            "the setup owns a viewer selection"
        );
        assert_eq!(panel.test_pip_role(), crate::voice_panel::PipRole::Viewer);
    });
    click(&mut visual, "pip-close");
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_watching_selection(), None);
        assert!(!panel.player_open());
    });
    let (screen, stream) = voice.update(&mut *visual, |panel, _| {
        (
            panel.test_take_screen_commands(),
            panel.test_take_stream_commands(),
        )
    });
    assert_eq!(
        screen,
        Vec::new(),
        "closing the viewer never stops the share"
    );
    assert_eq!(stream, vec![("close", selection)]);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.player_open(), "the viewer surface is gone");
        assert!(panel.test_screen_pip_active(), "the own share remains");
        assert!(panel.call_active(), "voice stays connected");
    });
    draw(&mut visual);
    assert!(
        !voice.read_with(&*visual, |panel, _| panel.test_pip_tabs()),
        "a single remaining role has no tab strip"
    );

    // A repeated close of the viewer sends nothing more.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.leave_stream(window, cx));
    });
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands()),
        Vec::new()
    );
}

/// M03 second half: finishing the own share sends one stop and keeps the
/// viewed stream.
#[gpui::test]
async fn m03_finishing_the_share_keeps_the_viewer(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    let selection = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            publish_live(panel, window, cx, 5);
            watch_publication(panel, window, cx)
        })
    });
    open_room(&mut visual, &view, 8);
    let _ = voice.update(&mut *visual, |panel, _| {
        (
            panel.test_take_screen_commands(),
            panel.test_take_stream_commands(),
        )
    });
    voice.update(&mut *visual, |panel, _| {
        panel.test_select_pip_role(crate::voice_panel::PipRole::Publisher);
    });
    draw(&mut visual);
    click(&mut visual, "pip-close");
    let (screen, stream) = voice.update(&mut *visual, |panel, _| {
        (
            panel.test_take_screen_commands(),
            panel.test_take_stream_commands(),
        )
    });
    assert_eq!(screen, vec![("stop", 0)], "one stop for the own share");
    assert_eq!(stream, Vec::new(), "the viewer is not left");
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.player_open(), "the viewer remains");
        assert_eq!(
            panel.test_watching_selection(),
            Some(selection),
            "the same viewer selection survives"
        );
        assert!(panel.call_active());
    });
    // The repeated close of an already stopping attempt sends no duplicate.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.test_stop_screen_share(window, cx))
    });
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands()),
        Vec::new()
    );
}

/// M04: the settings of publication A use A's permission while B is shown;
/// a new start in B is blocked by B's rules.
#[gpui::test]
async fn m04_live_settings_use_the_owner_room_and_b_blocks_its_own_start(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            let mut permissions = std::collections::HashMap::new();
            permissions.insert(7, true);
            permissions.insert(8, false);
            panel.test_set_stream_permissions(permissions);
            publish_live(panel, window, cx, 6);
        });
    });
    draw(&mut visual);
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands());

    // The confirmed channel list grants Stream in A only. The shell hands that
    // map to the panel on navigation, so it must agree with the confirmed
    // permission the publication started with.
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            for item in &mut v.channels {
                if item.id == 7 {
                    item.permissions |= crate::settings::PERMISSION_STREAM;
                }
            }
            cx.notify();
        });
    });

    // Navigate to B, which has no Stream permission. The panel pushes A's
    // confirmed access to the worker, not B's.
    open_room(&mut visual, &view, 8);
    let access = voice.update(&mut *visual, |panel, _| panel.test_take_screen_access());
    assert_eq!(
        access,
        Some((true, true)),
        "the live attempt keeps the owner permission and slot"
    );
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_publisher_room()),
        7
    );

    // The PiP settings open the existing live dialog and a bitrate change
    // reaches the live sender without a stop or a new start.
    click(&mut visual, "pip-settings");
    assert!(voice.read_with(&*visual, |panel, _| panel.screen_share_open()));
    let op_commands = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_change_screen_bitrate(100, cx);
            panel.test_set_share_gate(Ok(()));
            panel.test_start_screen_share(window, cx);
            panel.test_take_screen_commands()
        })
    });
    assert_eq!(op_commands.len(), 1, "{op_commands:?}");
    assert_eq!(op_commands[0].0, "bitrate", "bitrate applies live");
    let op = op_commands[0].1;
    // The sender confirms the bitrate. The dialog closes and the busy state is
    // released exactly as the runtime would report it.
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            let mut accepted = panel.test_screen_sharing();
            accepted.live = true;
            accepted.phase = Phase::Live;
            accepted.quality_status = Some(voxhold_media::publisher::QualityStatus::new(
                op,
                voxhold_media::publisher::QualityOutcome::Applied,
            ));
            assert!(panel.test_apply_screen_sharing(window, accepted));
        });
    });
    draw(&mut visual);
    assert!(!voice.read_with(&*visual, |panel, _| panel.screen_share_open()));

    // A resolution change is a confirmed restart of the owner attempt.
    click(&mut visual, "pip-settings");
    let restart = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_set_share_gate(Ok(()));
            panel.test_start_screen_share(window, cx);
            panel.test_take_screen_commands()
        })
    });
    assert_eq!(restart.len(), 1, "{restart:?}");
    assert_eq!(restart[0].0, "restart");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            // Cancel the restart dialog before the next, unrelated start.
            panel.test_close_screen_share(window, cx);
            let _ = panel.test_take_screen_commands();
        });
    });

    // A fresh publication in B is refused by B's own permission. The owner
    // call is moved to B and the displayed room is B; only the permission
    // differs.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_screen_sharing(Sharing::default());
            panel.test_set_call(
                8,
                crate::voice_room::CallPhase::Connected,
                Vec::new(),
                Vec::new(),
            );
            panel.set_room_state(8, Vec::new(), Vec::new());
            panel.test_set_stream_permissions(std::collections::HashMap::from([(8, false)]));
            panel.set_stream_permission(false);
            panel.test_clear_share_gate();
            assert!(panel.share_gate().is_err(), "B has no Stream permission");
            panel.test_open_screen_share(window, cx);
            assert!(!panel.screen_share_open(), "the picker never opens in B");
            assert!(panel.test_take_screen_commands().is_empty());
        });
    });
}

/// M05: keyboard and mouse of the PiP, viewer-only sound, one leave command.
#[gpui::test]
async fn m05_pip_focus_settings_and_viewer_audio(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    let selection = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            watch_publication(panel, window, cx)
        })
    });
    open_room(&mut visual, &view, 8);
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands());
    assert!(
        visual.debug_bounds("pip-settings").is_some() && visual.debug_bounds("pip-close").is_some(),
        "settings and close are painted"
    );

    // A mouse click opens the compact panel and focuses its first row.
    click(&mut visual, "pip-settings");
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.test_pip_settings_open());
    });
    assert!(
        visual.debug_bounds("pip-settings-panel").is_some(),
        "the compact panel is painted inside the PiP"
    );
    let focused = |visual: &mut VisualTestContext| {
        visual.update(|window, app| voice.read(app).focused_action(window))
    };
    assert_eq!(focused(&mut visual), Some("pip-settings-audio"));

    // Tab/Shift+Tab stay inside the open panel.
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("pip-settings-leave"));
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("pip-settings"));
    visual.simulate_keystrokes("shift-tab");
    assert_eq!(focused(&mut visual), Some("pip-settings-leave"));

    // The audio toggle changes only this viewer's output.
    visual.simulate_keystrokes("shift-tab");
    assert_eq!(focused(&mut visual), Some("pip-settings-audio"));
    let muted_before = voice.read_with(&*visual, |panel, _| panel.stream_muted());
    visual.simulate_keystrokes("enter");
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_stream_mute()),
        Some((selection, !muted_before))
    );
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands()),
        Vec::new(),
        "viewer audio never touches the publication"
    );

    // Escape closes the top panel and returns focus to its button.
    visual.simulate_keystrokes("escape");
    voice.read_with(
        &*visual,
        |panel, _| assert!(!panel.test_pip_settings_open()),
    );
    assert_eq!(focused(&mut visual), Some("pip-settings"));

    // Enter on the focused settings button reopens, then leaving sends one
    // command and closes the surface.
    visual.simulate_keystrokes("enter");
    assert!(voice.read_with(&*visual, |panel, _| panel.test_pip_settings_open()));
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("pip-settings-leave"));
    visual.simulate_keystrokes("enter");
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands()),
        vec![("close", selection)]
    );
    assert!(!voice.read_with(&*visual, |panel, _| panel.player_open()));
}

/// M05 without a sound track: the panel offers the fullscreen action instead
/// of a dead volume row.
#[gpui::test]
async fn m05_a_silent_stream_offers_fullscreen(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            let mut silent = publication(7, "bob-screen", 2);
            silent.has_audio = false;
            panel.watch(silent, window, cx);
        });
    });
    open_room(&mut visual, &view, 8);
    click(&mut visual, "pip-settings");
    assert!(
        visual.debug_bounds("pip-settings-fullscreen").is_some(),
        "a silent stream points at the fullscreen picture"
    );
    assert!(
        visual.debug_bounds("pip-settings-audio").is_none(),
        "no volume row without an audio track"
    );
    let focused = visual.update(|window, app| voice.read(app).focused_action(window));
    assert_eq!(focused, Some("pip-settings-fullscreen"));
    visual.simulate_keystrokes("enter");
    assert!(voice.read_with(&*visual, |panel, _| panel.player_fullscreen()));
}

/// M06: dragging and resizing stay inside the safe area, buttons do not start
/// a drag, and the position survives the route.
#[gpui::test]
async fn m06_pip_drag_resize_and_position(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| publish_live(panel, window, cx, 7));
    });
    open_room(&mut visual, &view, 8);
    let (area, boxed) = voice.read_with(&*visual, |panel, _| {
        (
            panel.test_player_area(),
            panel.player_box().expect("the PiP has a box"),
        )
    });
    assert!(boxed.x >= area.x + 16.0 - 0.5 && boxed.y >= area.y + 16.0 - 0.5);
    assert!(
        boxed.right() <= area.right() - 16.0 + 0.5 && boxed.bottom() <= area.bottom() - 16.0 + 0.5
    );
    assert!(
        (boxed.w - crate::voice_room::PIP_DEFAULT_WIDTH).abs() < 1.0,
        "the compact surface starts at 320 logical px"
    );
    assert!(
        (boxed.right() - (area.right() - 16.0)).abs() < 0.5
            && (boxed.bottom() - (area.bottom() - 16.0)).abs() < 0.5,
        "a fresh PiP starts in the bottom-right safe corner"
    );

    // A click on settings or close is not a drag.
    let geometry = crate::voice_room::pip_geometry(boxed, false);
    let settings = center(geometry.settings);
    let close = center(geometry.close);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            assert!(
                !panel.player_pointer_down(settings.0, settings.1),
                "settings is a button"
            );
            assert!(!panel.player_dragging());
            assert!(
                !panel.player_pointer_down(close.0, close.1),
                "close is a button"
            );
            assert!(!panel.player_dragging());
        });
    });

    // Drag beyond the top-left edge: the box is clamped inside the safe area.
    let start = center(geometry.header);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.player_pointer_down(start.0, start.1));
            panel.player_pointer_moved(area.x - 900.0, area.y - 900.0, cx);
            let dragged = panel.player_box().expect("still painted");
            assert!(dragged.x >= area.x + 16.0 - 0.5);
            assert!(dragged.y >= area.y + 16.0 - 0.5);
            panel.player_pointer_up();
            assert!(!panel.player_dragging(), "release ends the drag");
        });
    });

    // A lost mouse-up outside the window is reported as a move without the
    // left button: the drag ends instead of following the cursor.
    let (boxed, geometry) = voice.read_with(&*visual, |panel, _| {
        let boxed = panel.player_box().unwrap();
        (boxed, crate::voice_room::pip_geometry(boxed, false))
    });
    let start = center(geometry.header);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            assert!(panel.player_pointer_down(start.0, start.1));
            assert!(panel.player_dragging());
        });
    });
    visual.simulate_mouse_move(
        gpui::point(gpui::px(boxed.x + 40.0), gpui::px(boxed.y + 10.0)),
        None,
        Modifiers::default(),
    );
    assert!(
        !voice.read_with(&*visual, |panel, _| panel.player_dragging()),
        "a move without the held button ends the drag"
    );

    // Resize far beyond the right edge: the width is clamped in the area.
    let geometry = voice.read_with(&*visual, |panel, _| {
        let boxed = panel.player_box().unwrap();
        crate::voice_room::pip_geometry(boxed, false)
    });
    let corner = center(geometry.corner);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.player_pointer_down(corner.0, corner.1));
            panel.player_pointer_moved(area.right() + 800.0, corner.1, cx);
            let resized = panel.player_box().unwrap();
            assert!(resized.right() <= area.right() - 16.0 + 0.5);
            panel.player_pointer_up();
        });
    });

    // The local position survives A ↔ B.
    let saved = voice.read_with(&*visual, |panel, _| panel.player_box().unwrap());
    open_room(&mut visual, &view, 7);
    assert!(!voice.read_with(&*visual, |panel, _| panel.test_pip_active()));
    open_room(&mut visual, &view, 8);
    let restored = voice.read_with(&*visual, |panel, _| panel.player_box().unwrap());
    assert!(
        (restored.x - saved.x).abs() < 1.0 && (restored.y - saved.y).abs() < 1.0,
        "the local position survives the route"
    );

    // A narrow safe area shrinks the width instead of leaving it. This is the
    // last step: the shell resets the area to the real viewport next frame.
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.set_player_area_and_place(crate::voice_room::Rect::new(0., 0., 300., 600.));
            let narrow = panel.player_box().unwrap();
            assert!(narrow.w <= 300.0 - 32.0 + 0.5, "{narrow:?}");
            assert!(narrow.x >= 16.0 - 0.5 && narrow.right() <= 300.0 - 16.0 + 0.5);
            assert!(narrow.y >= 16.0 - 0.5 && narrow.bottom() <= 600.0 - 16.0 + 0.5);
        });
    });
}

/// M06 DPI: a viewed PiP asks the decoder for the physical picture size.
#[gpui::test]
async fn m06_dpi_changes_the_requested_picture(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.set_scale(1.0);
            watch_publication(panel, window, cx);
        });
    });
    open_room(&mut visual, &view, 8);
    let first = voice.update(&mut *visual, |panel, _| {
        panel.set_scale(1.0);
        panel.test_take_viewport()
    });
    assert!(first.is_some(), "the PiP reports its picture");
    visual.update(|_window, app| {
        voice.update(app, |panel, _| panel.set_scale(2.0));
    });
    let second = voice.update(&mut *visual, |panel, _| panel.test_take_viewport());
    let first = first.unwrap();
    let second = second.expect("a DPI change is a new physical surface");
    assert_eq!(second.0, first.0 * 2);
    assert_eq!(second.1, first.1 * 2);
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_viewport()),
        None,
        "the same surface is not requested twice"
    );
}

/// M07: reconnect keeps the surface and its last valid frame; late events of
/// another stream or attempt never close it; revoke and logout clear it.
#[gpui::test]
async fn m07_reconnect_late_events_and_revoke(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    let selection = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            watch_publication(panel, window, cx)
        })
    });
    open_room(&mut visual, &view, 8);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                VoiceUpdate::StreamStatus {
                    selection,
                    status: "Восстанавливаем соединение…".into(),
                },
                window,
                cx,
            );
        });
    });
    draw(&mut visual);
    assert!(voice.read_with(&*visual, |panel, _| panel.player_open()));
    assert!(
        voice
            .read_with(&*visual, |panel, _| panel.test_pip_status())
            .contains("Восстанавливаем"),
        "the current attempt status is shown"
    );

    // A late stop of another selection must not close this player.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                VoiceUpdate::StreamStatus {
                    selection: selection + 99,
                    status: String::new(),
                },
                window,
                cx,
            );
        });
    });
    assert!(voice.read_with(&*visual, |panel, _| panel.player_open()));

    // A late snapshot of an older publication attempt is ignored too.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            publish_live(panel, window, cx, 9);
            panel.test_apply_update(
                VoiceUpdate::ScreenShare(Sharing {
                    phase: Phase::Idle,
                    attempt: 8,
                    ..Sharing::default()
                }),
                window,
                cx,
            );
        });
    });
    draw(&mut visual);
    assert!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_pip_active()),
        "attempt 8 cannot end the on-air attempt 9"
    );

    // Revoke ends both intents and removes every PiP surface.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                VoiceUpdate::Ended(Some("Доступ отозван".into())),
                window,
                cx,
            );
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_pip_active(), "revoke leaves no PiP surface");
        assert!(!panel.player_open());
        assert!(!panel.test_screen_pip_active());
        assert!(!panel.call_active());
    });

    // Logout uses the same teardown.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.abandon_voice(window, cx));
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_pip_active());
        assert!(!panel.call_active());
    });
}

/// M05: a settings dialog opened from the publisher PiP returns focus to its
/// button when it closes, if the surface is still alive.
#[gpui::test]
async fn m05_publisher_settings_return_focus_to_the_pip(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| publish_live(panel, window, cx, 11));
        voice.update(app, |panel, cx| {
            watch_publication(panel, window, cx);
            panel.test_select_pip_role(crate::voice_panel::PipRole::Publisher);
        });
    });
    open_room(&mut visual, &view, 8);
    draw(&mut visual);
    click(&mut visual, "pip-settings");
    assert!(voice.read_with(&*visual, |panel, _| panel.screen_share_open()));
    assert!(voice.read_with(&*visual, |panel, _| panel.test_pip_restore_focus()));

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_close_screen_share(window, cx);
            panel.test_sync_pip(window);
        });
    });
    assert_eq!(
        visual.update(|window, app| voice.read(app).focused_action(window)),
        Some("pip-settings"),
        "focus returns to the settings button of the living PiP"
    );
    assert!(voice.read_with(&*visual, |panel, _| panel.test_pip_active()));
}

/// M07: revoking access in A closes the call and the PiP even while B is
/// displayed.
#[gpui::test]
async fn m07_revoke_in_the_owner_room_closes_the_pip(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| publish_live(panel, window, cx, 12));
    });
    open_room(&mut visual, &view, 8);
    assert!(visual.debug_bounds("voice-pip").is_some());
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channels = vec![voxhold_api::Channel {
                id: 8,
                name: "Room B".into(),
                kind: "voice".into(),
                position: 0,
                permissions: 1,
                access_restricted: false,
                e2ee_required: false,
                comments_enabled: false,
                can_publish: false,
                unread_count: 0,
            }];
            v.refresh_voice_call(window, cx);
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.call_active(), "A is no longer accessible");
        assert!(!panel.test_pip_active(), "no stale PiP survives the revoke");
        assert!(!panel.test_screen_pip_active());
    });
}

/// M07: the personal card must not reappear outside its room even after an
/// error snapshot.
#[gpui::test]
async fn m07_an_error_outside_the_room_does_not_restore_the_card(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| publish_live(panel, window, cx, 10));
    });
    open_room(&mut visual, &view, 8);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Failed,
                attempt: 10,
                error: Some("Кодировщик недоступен".into()),
                ..Sharing::default()
            });
        });
    });
    draw(&mut visual);
    assert!(
        !voice.read_with(&*visual, |panel, _| panel.test_screen_card_in_room()),
        "no grid card outside the owner room, not even on failure"
    );
    assert!(
        visual.debug_bounds("voice-pip").is_some(),
        "the failed attempt is reported in the PiP with its error"
    );
    assert!(voice.read_with(&*visual, |panel, _| {
        panel.test_pip_status().contains("Кодировщик")
    }));
}
