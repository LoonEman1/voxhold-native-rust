//! Regressions for the ownership of an own publication across navigation,
//! reconnect and terminal phases: the gate that reaches the worker, the one
//! stop latch, the fullscreen layer of the player and the labels of the PiP.

use super::voice_pip_tests::{
    click, draw, live, open_room, publication, publish_live, source, watch_publication,
};
use super::voice_tests::participant;
use super::*;
use gpui::{TestAppContext, VisualTestContext};
use std::collections::HashMap;
use voxhold_media::publisher::{Phase, Resolution, Sharing};
use voxhold_media::runtime::VoiceUpdate;

/// Install a confirmed permission map and keep the legacy single flag of the
/// displayed room in step. The fixture channel list grants no `Stream` bit, so
/// every test that needs a confirmed grant states it here.
fn confirmed_permissions(
    panel: &mut voice_panel::VoicePanel,
    permissions: [(i64, bool); 2],
    displayed: i64,
) {
    let map: HashMap<i64, bool> = permissions.into_iter().collect();
    let allowed = map.get(&displayed).copied().unwrap_or(false);
    panel.test_set_stream_permissions(map);
    panel.set_stream_permission(allowed);
}

/// P1: a `Stream` revocation after the start must reach the worker and block
/// the live settings; the start snapshot must not freeze the permission.
#[gpui::test]
async fn m04_a_stream_revocation_reaches_the_worker_and_blocks_the_settings(
    cx: &mut TestAppContext,
) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            publish_live(panel, window, cx, 3);
            let _ = panel.test_take_screen_commands();

            // The server revokes exactly the Stream bit of A while the attempt
            // is live: the current confirmation, not the start snapshot, is
            // what the worker has to see.
            panel.test_set_stream_permissions(HashMap::from([(7, false), (8, true)]));
            assert_eq!(
                panel.test_take_screen_access(),
                Some((false, true)),
                "a Stream revocation must reach the worker"
            );

            // The live dialog of the owner cannot restart into a room that no
            // longer grants Stream, and it says why.
            panel.test_open_live_screen_settings(window, cx);
            assert!(panel.screen_share_open());
            panel.test_clear_share_gate();
            panel.test_set_screen_resolution(Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a revoked Stream blocks the restart"
            );
            assert!(
                panel.test_screen_error().contains("права"),
                "the refusal is visible: {:?}",
                panel.test_screen_error()
            );
            // Closing the refused dialog cancels nothing that was sent.
            panel.test_close_screen_share(window, cx);
            assert!(panel.test_take_screen_commands().is_empty());
            assert_eq!(
                panel.test_screen_phase(),
                Phase::Live,
                "the stream is still owned"
            );

            // Granting Stream again restores the gate, and a real quality
            // change may preflight. A revocation that lands during that
            // preflight must reach the worker exactly like the start one: the
            // worker cancels the preparation, the UI sends no stop.
            panel.test_set_stream_permissions(HashMap::from([(7, true), (8, false)]));
            assert_eq!(panel.test_take_screen_access(), Some((true, true)));
            panel.test_open_live_screen_settings(window, cx);
            panel.test_clear_share_gate();
            panel.test_set_screen_resolution(Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let restart = panel.test_take_screen_commands();
            assert_eq!(restart.len(), 1, "{restart:?}");
            assert_eq!(restart[0].0, "restart");
            assert!(panel.screen_share_busy(), "the preflight is pending");
            panel.test_set_stream_permissions(HashMap::from([(7, false), (8, false)]));
            assert_eq!(
                panel.test_take_screen_access(),
                Some((false, true)),
                "the revocation reaches the pending preflight"
            );
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a cancelled preflight is not a stop"
            );
            panel.test_close_screen_share(window, cx);
            let _ = panel.test_take_screen_commands();

            // The user may still end the stream: exactly one stop.
            panel.test_stop_screen_share(window, cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("stop", 0)]);
        });
    });
}

/// P1: the gate of an intent that was sent but not confirmed yet belongs to
/// the room that started it. Opening another room must not swap it.
#[gpui::test]
async fn m04_an_unconfirmed_start_keeps_its_owner_gate(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            confirmed_permissions(panel, [(7, true), (8, false)], 7);
            panel.test_set_share_gate(Ok(()));
            panel.test_prepare_screen_publish(window, source());
            panel.test_start_screen_share(window, cx);
            let _ = panel.test_take_screen_commands();
            assert!(panel.screen_share_busy(), "the intent is open");
            assert_eq!(
                panel.test_screen_phase(),
                Phase::Idle,
                "the worker has not confirmed the attempt yet"
            );
            let _ = panel.test_take_screen_access();

            // The user opens B while the capture preparation is still running.
            panel.set_room_state(8, Vec::new(), Vec::new());
            panel.test_set_stream_permissions(HashMap::from([(7, true), (8, false)]));
            panel.set_stream_permission(false);
            assert_eq!(
                panel.test_take_screen_access(),
                Some((true, true)),
                "the sent intent keeps the confirmed gate of A"
            );
            assert_eq!(panel.test_publisher_room(), 7);

            // The owner's permission is then revoked: the same owner branch
            // must cancel the pending preparation.
            panel.test_set_stream_permissions(HashMap::from([(7, false), (8, false)]));
            assert_eq!(
                panel.test_take_screen_access(),
                Some((false, true)),
                "the revocation follows the owner, not the displayed room"
            );

            // A stop of the unconfirmed intent is still exactly one command.
            panel.test_stop_screen_share(window, cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("stop", 0)]);
        });
    });
}

/// P3: the stop latch is keyed by the publish session, so the confirmation of
/// the same intent does not unlock a second stop.
#[gpui::test]
async fn m03_the_stop_latch_survives_the_attempt_confirmation(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            confirmed_permissions(panel, [(7, true), (8, false)], 7);
            panel.test_set_share_gate(Ok(()));
            panel.test_prepare_screen_publish(window, source());
            panel.test_start_screen_share(window, cx);
            let _ = panel.test_take_screen_commands();
            assert_eq!(panel.test_screen_phase(), Phase::Idle, "attempt 0");

            panel.test_stop_screen_share(window, cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("stop", 0)]);

            // The media layer confirms the very attempt that was stopped.
            assert!(panel.test_apply_screen_sharing(window, live(21)));
            assert_eq!(
                panel.test_publish_access().map(|access| access.2),
                Some(21),
                "the confirmed attempt replaces the zero of the intent"
            );
            panel.test_stop_screen_share(window, cx);
            assert_eq!(
                panel.test_take_screen_commands(),
                Vec::new(),
                "the confirmation of the same intent must not unlock a second stop"
            );
        });
    });
}

/// P2: while the player owns the window, the Tab ring describes the fullscreen
/// layer that is really painted, and closing the role gives the window back.
#[gpui::test]
async fn m02_fullscreen_ring_and_close_follow_the_painted_layer(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    let selection = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            watch_publication(panel, window, cx)
        })
    });
    open_room(&mut visual, &view, 8);
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands());

    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.player_toggle_fullscreen(window));
    });
    draw(&mut visual);
    assert!(voice.read_with(&*visual, |panel, _| panel.player_fullscreen()));
    assert!(
        visual.debug_bounds("stream-exit-full").is_some(),
        "the exit control of the fullscreen layer is painted"
    );
    assert!(
        visual.debug_bounds("stream-audio").is_some(),
        "the viewer audio action is painted"
    );
    let ring = voice.read_with(&*visual, |panel, _| panel.test_focus_ring_ids());
    let player_ring: Vec<&str> = ring
        .iter()
        .copied()
        .filter(|id| id.starts_with("stream-") || id.starts_with("pip-"))
        .collect();
    assert_eq!(
        player_ring,
        vec!["stream-audio", "stream-exit-full"],
        "the player entries describe the fullscreen layer: {ring:?}"
    );
    assert!(
        !ring
            .iter()
            .any(|id| matches!(*id, "pip-close" | "pip-settings" | "pip-tab-viewer")),
        "the unmounted PiP buttons are not focusable: {ring:?}"
    );
    let focused = |visual: &mut VisualTestContext| {
        visual.update(|window, app| voice.read(app).focused_action(window))
    };
    assert_eq!(focused(&mut visual), Some("stream-exit-full"));

    // Tab reaches the audio action, Enter changes only this viewer's sound.
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("stream-audio"));
    let muted = voice.read_with(&*visual, |panel, _| panel.stream_muted());
    visual.simulate_keystrokes("enter");
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_stream_mute()),
        Some((selection, !muted))
    );

    // Enter on the exit control restores the PiP and its own ring.
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("stream-exit-full"));
    visual.simulate_keystrokes("enter");
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.player_fullscreen(), "the window is given back");
        assert!(panel.player_open(), "the viewer survives");
        assert!(panel.test_pip_active());
        assert!(
            !panel.test_focus_ring_ids().contains(&"stream-exit-full"),
            "the fullscreen ring is gone"
        );
    });
    assert_eq!(
        focused(&mut visual),
        Some("pip-settings"),
        "focus lands on an action the PiP really paints"
    );
    assert!(
        visual.debug_bounds("pip-settings").is_some(),
        "that action is painted"
    );

    // The same fullscreen is released when the role is closed from it.
    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.player_toggle_fullscreen(window));
    });
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_pip_close_role(window, cx);
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.player_open(), "closing the role closes the player");
        assert!(!panel.player_fullscreen());
        assert!(
            !panel.test_owns_fullscreen(),
            "the window is not left owned"
        );
        assert!(!panel.test_pip_active());
    });
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands()),
        vec![("close", selection)]
    );
}

/// P4: a failed attempt owns no stream. Closing its PiP dismisses the terminal
/// surface with no command, and a stopping attempt paints a status instead of a
/// dead button.
#[gpui::test]
async fn m07_a_terminal_attempt_closes_without_a_command(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| publish_live(panel, window, cx, 5));
    });
    open_room(&mut visual, &view, 8);
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands());
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_apply_screen_sharing(
                window,
                Sharing {
                    phase: Phase::Stopping,
                    attempt: 5,
                    live: true,
                    ..Sharing::default()
                },
            );
        });
    });
    // The in-room card paints the same status and its handle leaves the ring,
    // so Tab never lands on an action that is no longer painted. (The card is
    // in room A, the PiP below is shown in B.)
    open_room(&mut visual, &view, 7);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_screen_card_stop_painted());
        assert!(!panel.test_focus_ring_ids().contains(&"screen-stop"));
    });
    open_room(&mut visual, &view, 8);
    draw(&mut visual);
    assert!(visual.debug_bounds("voice-pip").is_some());
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_pip_close_enabled());
        assert!(!panel.test_pip_settings_enabled());
        assert!(
            !panel.test_focus_ring_ids().contains(&"pip-close"),
            "a status is not a Tab stop"
        );
    });
    assert!(
        visual.debug_bounds("pip-close").is_some(),
        "the stopping status is still painted"
    );
    click(&mut visual, "pip-close");
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands()),
        Vec::new(),
        "an already stopping attempt has nothing to send"
    );

    // A failed attempt is terminal: the close control dismisses it locally.
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_apply_screen_sharing(
                window,
                Sharing {
                    phase: Phase::Failed,
                    attempt: 5,
                    error: Some("Кодировщик недоступен".into()),
                    ..Sharing::default()
                },
            );
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.test_pip_close_enabled());
        assert!(
            panel.test_pip_status().contains("Кодировщик"),
            "the reason is painted: {:?}",
            panel.test_pip_status()
        );
    });
    click(&mut visual, "pip-close");
    draw(&mut visual);
    assert_eq!(
        voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands()),
        Vec::new(),
        "a failed attempt must not send a stop"
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_phase(), Phase::Idle);
        assert!(!panel.test_pip_active(), "the terminal PiP is gone");
        assert_eq!(panel.test_publish_access(), None);
    });
}

/// P5: the recovering label belongs to an unconfirmed connection, so a
/// recovered attempt returns to the normal title.
#[gpui::test]
async fn m07_a_recovered_reconnect_clears_the_recovering_label(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            assert!(panel.test_apply_update(
                VoiceUpdate::SelfConnection(Some("c1".into())),
                window,
                cx
            ));
            confirmed_permissions(panel, [(7, true), (8, false)], 7);
            panel.test_set_share_gate(Ok(()));
            panel.test_prepare_screen_publish(window, source());
            panel.test_start_screen_share(window, cx);
            assert!(panel.test_apply_screen_sharing(window, live(6)));
            assert_eq!(panel.test_pip_title(), "Ваш экран");

            // A new connection id belongs to a session the media layer has not
            // confirmed again: the PiP says so.
            assert!(panel.test_apply_update(
                VoiceUpdate::SelfConnection(Some("c2".into())),
                window,
                cx
            ));
            assert_eq!(panel.test_pip_title(), "Ваш экран · восстановление");
            assert_eq!(
                panel.test_publish_access().map(|access| access.2),
                Some(6),
                "the confirmed attempt is kept"
            );

            // The media layer recovers the attempt on the new connection. The
            // label must not stay on "recovering" forever.
            assert!(panel.test_apply_screen_sharing(
                window,
                Sharing {
                    phase: Phase::Publishing,
                    attempt: 6,
                    live: true,
                    ..Sharing::default()
                }
            ));
            assert_eq!(panel.test_pip_title(), "Ваш экран");
            assert_eq!(
                panel.test_publish_access().and_then(|access| access.3),
                Some("c2".into())
            );
        });
    });
}

/// P6: a reconnect replays the session facts without a duplicate stop or
/// leave, and each role still owns exactly one closing command.
#[gpui::test]
async fn m07_a_reconnect_duplicates_no_stop_and_no_leave(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    let selection = visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_apply_update(VoiceUpdate::SelfConnection(Some("c1".into())), window, cx);
            confirmed_permissions(panel, [(7, true), (8, false)], 7);
            panel.test_set_share_gate(Ok(()));
            panel.test_prepare_screen_publish(window, source());
            panel.test_start_screen_share(window, cx);
            assert!(panel.test_apply_screen_sharing(window, live(9)));
            panel.test_set_screen_confirmed_source(source());
            let selection = watch_publication(panel, window, cx);
            let _ = panel.test_take_screen_commands();
            let _ = panel.test_take_stream_commands();

            for update in [
                VoiceUpdate::Reconnecting,
                VoiceUpdate::SelfConnection(Some("c2".into())),
                VoiceUpdate::Connected,
                VoiceUpdate::Streams(vec![publication(7, "c2", 1)]),
                VoiceUpdate::Participants(vec![participant("c2", 1)]),
                VoiceUpdate::Tracks(std::collections::HashSet::from(["c2".to_owned()])),
            ] {
                panel.test_apply_update(update, window, cx);
            }
            assert_eq!(
                panel.test_take_screen_commands(),
                Vec::new(),
                "a reconnect stops nothing"
            );
            assert_eq!(
                panel.test_take_stream_commands(),
                Vec::new(),
                "a reconnect leaves nothing"
            );

            // Both intents are still owned and close exactly once.
            panel.test_stop_screen_share(window, cx);
            panel.leave_stream(window, cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("stop", 0)]);
            assert_eq!(
                panel.test_take_stream_commands(),
                vec![("close", selection)]
            );
            selection
        })
    });
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_watching_selection()),
        None,
        "the leave closed the one selection ({selection})"
    );
}

/// P6/M01: the own card takes exactly one slot of its own grid and none of
/// another room's grid.
#[gpui::test]
async fn m01_the_owner_card_takes_one_slot_only_in_its_own_grid(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_apply_update(VoiceUpdate::SelfConnection(Some("me".into())), window, cx);
            confirmed_permissions(panel, [(7, true), (8, true)], 7);
            panel.test_set_share_gate(Ok(()));
            panel.test_prepare_screen_publish(window, source());
            panel.test_start_screen_share(window, cx);
            assert!(panel.test_apply_screen_sharing(window, live(4)));
            panel.test_set_screen_confirmed_source(source());
            panel.test_set_streams(vec![
                publication(7, "me", 1),
                publication(7, "bob-screen", 2),
                publication(7, "carol-screen", 3),
            ]);
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_visible_streams_len(),
            2,
            "the own publication is the card, not a second tile"
        );
        assert!(panel.test_screen_card_in_room());
        assert!(!panel.test_screen_pip_active());
    });

    // Another room's grid reserves nothing for A's publication.
    open_room(&mut visual, &view, 8);
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_visible_streams_len(), 0);
        assert!(
            !panel.test_screen_card_in_room(),
            "B has no slot for the stream of A"
        );
        assert!(panel.test_screen_pip_active());
    });
}

/// P6: a late settings dialog of an attempt that just ended closes with it and
/// cannot restart the stream.
#[gpui::test]
async fn m04_a_late_settings_dialog_closes_with_the_attempt(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            publish_live(panel, window, cx, 7);
            let _ = panel.test_take_screen_commands();
            panel.test_open_live_screen_settings(window, cx);
            assert!(panel.screen_share_open());
            // The attempt ends while the dialog still waits for its answer.
            assert!(panel.test_apply_screen_sharing(window, Sharing::default()));
            assert!(
                !panel.screen_share_open(),
                "an ended attempt takes its dialog with it"
            );
            assert_eq!(panel.test_publish_access(), None);
            // A delayed snapshot of that ended attempt cannot open it again.
            let _ = panel.test_apply_screen_sharing(
                window,
                Sharing {
                    phase: Phase::Publishing,
                    attempt: 6,
                    live: true,
                    ..Sharing::default()
                },
            );
            assert!(!panel.screen_share_open());
            assert!(panel.test_take_screen_commands().is_empty());
            assert_eq!(panel.test_screen_phase(), Phase::Idle);
        });
    });
}

/// P6: another call starts without the intents of the previous one, and its
/// gate follows the new room.
#[gpui::test]
async fn m07_another_call_starts_without_the_old_intents(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            publish_live(panel, window, cx, 5);
            watch_publication(panel, window, cx);
            let _ = panel.test_take_screen_commands();
            let _ = panel.test_take_stream_commands();

            // Leaving the call ends the viewer and the publication as one
            // session: the queued `stream.leave` is subsumed by `leave`, so no
            // separate command may survive the teardown.
            panel.leave_in_window(window, cx);
            assert_eq!(panel.test_take_screen_commands(), Vec::new());
            assert_eq!(panel.test_take_stream_commands(), Vec::new());
            assert_eq!(panel.test_publish_access(), None);
            assert!(!panel.test_pip_active());

            // Joining another room starts clean: no card, no owner, no intent.
            panel.set_room_state(8, Vec::new(), Vec::new());
            panel.test_join_detached(window, cx);
            assert!(panel.call_active());
            assert_eq!(panel.test_publisher_room(), 8);
            assert_eq!(panel.test_publish_access(), None);
            assert!(!panel.test_pip_active());
            assert!(!panel.test_screen_card_in_room());
            assert_eq!(panel.test_take_screen_commands(), Vec::new());
            assert_eq!(panel.test_take_stream_commands(), Vec::new());
        });
    });
}

/// P3: a terminal answer for an intent that never went Live must not replace
/// the owner gate with the displayed room. The publisher PiP has no settings
/// before its attempt is Live, so leaving fullscreen has to focus a painted
/// tab or the close instead of a dead button.
#[gpui::test]
async fn a09_exit_fullscreen_focuses_a_painted_publisher_action(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            publish_live(panel, window, cx, 3);
            watch_publication(panel, window, cx);
            panel.test_select_pip_role(crate::voice_panel::PipRole::Publisher);
        });
    });
    open_room(&mut visual, &view, 9);
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_stream_commands());
    let _ = voice.update(&mut *visual, |panel, _| panel.test_take_screen_commands());

    // The player owns the window while the publisher role is on screen.
    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.player_toggle_fullscreen(window));
    });
    draw(&mut visual);
    assert!(voice.read_with(&*visual, |panel, _| panel.player_fullscreen()));

    // The attempt starts stopping: the publisher settings button is no longer
    // painted, and neither is its close (a status takes their place).
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_apply_screen_sharing(
                window,
                Sharing {
                    phase: Phase::Stopping,
                    attempt: 3,
                    live: true,
                    ..Sharing::default()
                },
            );
        });
    });
    draw(&mut visual);
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_selected_pip_role()),
        crate::voice_panel::PipRole::Publisher
    );

    // Leaving fullscreen must put the keyboard on a control the restored card
    // really paints: its publisher tab. Focusing its settings would target a
    // button that this phase does not draw.
    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.exit_fullscreen(window));
    });
    draw(&mut visual);
    let focused = |visual: &mut VisualTestContext| {
        visual.update(|window, app| voice.read(app).focused_action(window))
    };
    assert_eq!(
        focused(&mut visual),
        Some("pip-tab-publisher"),
        "focus lands on the painted publisher action"
    );
    assert!(
        visual.debug_bounds("pip-tab-publisher").is_some(),
        "the focused control is painted"
    );
    // The stopping status paints its settings and close as text, not as live
    // actions: neither is a Tab stop of the restored card.
    let ring = voice.read_with(&*visual, |panel, _| panel.test_focus_ring_ids());
    assert!(
        !ring
            .iter()
            .any(|id| matches!(*id, "pip-settings" | "pip-close")),
        "the stopping publisher paints no dead action: {ring:?}"
    );
}

/// P3: `Failed` before `Live` keeps the sent intent as the gate owner. Joining
/// B afterwards must hand the worker A's confirmed permission, not B's.
#[gpui::test]
async fn a09_a_terminal_snapshot_keeps_the_owner_gate(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            confirmed_permissions(panel, [(7, true), (8, false)], 7);
            panel.test_set_share_gate(Ok(()));
            panel.test_prepare_screen_publish(window, source());
            panel.test_start_screen_share(window, cx);
            let _ = panel.test_take_screen_commands();
            let _ = panel.test_take_screen_access();
            assert_eq!(panel.test_screen_phase(), Phase::Idle, "no Live yet");

            // The worker answers with a terminal failure before the attempt is
            // confirmed.
            assert!(panel.test_apply_screen_sharing(
                window,
                Sharing {
                    phase: Phase::Failed,
                    attempt: 0,
                    error: Some("Захват не начался".into()),
                    ..Sharing::default()
                },
            ));
            assert_eq!(panel.test_screen_phase(), Phase::Failed);
            let access = panel
                .test_publish_access()
                .expect("the unconfirmed intent still owns A");
            assert_eq!(access.0, 7, "the owner room is A");
            assert_eq!(access.2, 0, "the attempt was never confirmed");

            // The user opens B while the call stays in A. B grants no Stream,
            // so leaking its gate would reach the worker as `(false, true)`.
            panel.set_room_state(8, Vec::new(), Vec::new());
            panel.test_set_stream_permissions(HashMap::from([(7, true), (8, false)]));
            panel.set_stream_permission(false);
            assert_eq!(panel.test_publisher_room(), 7);
            assert_eq!(
                panel.test_take_screen_access(),
                Some((true, true)),
                "the terminal intent keeps the confirmed gate of A"
            );

            // The dismissal drops the retained identity: no stale owner may
            // gate the next room.
            panel.test_stop_screen_share(window, cx);
            assert_eq!(panel.test_publish_access(), None);
            assert_eq!(
                panel.test_take_screen_access(),
                None,
                "a dismissed attempt pushes no gate"
            );
        });
    });
}
