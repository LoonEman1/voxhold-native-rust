//! GPUI regressions for the voice room, the dock and the stream player. They
//! drive the real window event path (keys, focus, layout) and read the actual
//! element bounds, not a hand-computed rectangle.

use super::*;
use crate::voice_room::CallPhase;
use gpui::{Keystroke, TestAppContext, VisualTestContext};
use voxhold_media::controller::{Participant, Publication};

fn client() -> voxhold_api::Client {
    let mut client = voxhold_api::Client::new("http://127.0.0.1:1").unwrap();
    client.set_token("token".into());
    client
}

fn channel(id: i64, name: &str, kind: &str) -> voxhold_api::Channel {
    voxhold_api::Channel {
        id,
        name: name.into(),
        kind: kind.into(),
        position: 0,
        permissions: 1,
        access_restricted: false,
        e2ee_required: false,
        comments_enabled: false,
        can_publish: false,
        unread_count: 0,
    }
}

fn participant(connection: &str, user: i64) -> Participant {
    Participant {
        connection_id: connection.into(),
        user_id: user,
        server_id: 1,
        channel_id: 7,
        self_mute: false,
        self_deaf: false,
        server_mute: false,
        reconnecting: false,
    }
}

fn publication(channel_id: i64, publisher: &str, user: i64) -> Publication {
    Publication {
        server_id: 1,
        channel_id,
        publisher_user_id: user,
        publisher_connection_id: publisher.into(),
        mode: "server".into(),
        codec: "vp8".into(),
        has_audio: true,
        viewer_count: 2,
        started_at_ms: 1,
    }
}

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

/// Focus the room after it has been mounted. A focus handle set before its
/// element is painted does not own the keyboard in the test window.
fn focus_room(visual: &mut VisualTestContext, voice: &Entity<voice_panel::VoicePanel>) {
    for _ in 0..2 {
        visual.update(|window, app| {
            let focus = voice.read(app).focus_handle();
            window.focus(&focus);
            window.draw(app).clear();
        });
        visual.run_until_parked();
    }
}

/// A window with two voice rooms and one text channel, an active call in room
/// 7 and the matching panel entity.
fn fixture(
    cx: &mut TestAppContext,
) -> (
    &'static mut VisualTestContext,
    Entity<Voxhold>,
    Entity<voice_panel::VoicePanel>,
) {
    cx.update(|cx| gpui_component::init(cx));
    let client = client();
    let (window, view) = super::settings_tests::make_app(cx, client.clone());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let voice = visual.update(|window, cx| {
        let panel = cx.new(|cx| voice_panel::VoicePanel::new(client, 1, 7, false, window, cx));
        let mut rooms = std::collections::HashMap::new();
        rooms.insert(
            7,
            crate::voice_panel::RoomAccess {
                name: "Room A".into(),
                readable: true,
                e2ee: false,
            },
        );
        rooms.insert(
            8,
            crate::voice_panel::RoomAccess {
                name: "Room B".into(),
                readable: true,
                e2ee: false,
            },
        );
        panel.update(cx, |panel, _| {
            panel.set_rooms(rooms);
            panel.set_self_user(1);
            panel.set_room_state(7, Vec::new(), Vec::new());
            panel.test_set_call(
                7,
                CallPhase::Connected,
                vec![participant("alice", 1), participant("bob", 2)],
                vec!["alice".into()],
            );
        });
        view.update(cx, |v, cx| {
            cx.notify();
            v.channels = vec![
                channel(7, "Room A", "voice"),
                channel(8, "Room B", "voice"),
                channel(9, "Text", "text"),
            ];
            v.members = vec![
                voxhold_api::Member {
                    user_id: 1,
                    username: "alice".into(),
                    role: "member".into(),
                    ..Default::default()
                },
                voxhold_api::Member {
                    user_id: 2,
                    username: "bob".into(),
                    role: "member".into(),
                    ..Default::default()
                },
            ];
            v.server = 1;
            v.voice = Some(panel.clone());
            v.voice_room = 7;
            v.voice_channel = 7;
            v.channel = 7;
            v.access_pending = false;
            cx.notify();
        });
        let focus = panel.read(cx).focus_handle();
        window.focus(&focus);
        panel
    });
    (visual, view, voice)
}

#[gpui::test]
async fn a_call_survives_navigation_and_moves_to_the_dock(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    view.read_with(&*visual, |v, app| {
        assert!(v.voice_room_visible(), "the call room is shown");
        assert!(!v.voice_dock_visible(app), "no dock while the room is open");
    });

    // Browse another voice room: the call stays in room 7 and gets a dock.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channel = 8;
            v.open_voice_room(8, window, cx);
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, app| {
        assert!(v.voice_room_visible());
        assert!(v.voice_dock_visible(app));
    });
    assert!(
        visual.debug_bounds("voice-dock").is_some(),
        "the dock is painted under the channel list"
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.call_channel(), 7, "navigation must not end the call");
        assert!(panel.call_active());
    });

    // A text channel keeps the call in the dock too.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channel = 9;
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, app| {
        assert!(!v.voice_room_visible());
        assert!(v.voice_dock_visible(app));
    });

    // Returning to the call room hides the dock and keeps the same attempt.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channel = 7;
            v.open_voice_room(7, window, cx);
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, app| {
        assert!(v.voice_room_visible());
        assert!(!v.voice_dock_visible(app));
    });
}

#[gpui::test]
async fn tab_and_shift_tab_walk_the_room_actions(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture(cx);
    draw(&mut visual);
    focus_room(&mut visual, &voice);
    let focused = |visual: &mut VisualTestContext| {
        visual.update(|window, app| voice.read(app).focused_action(window))
    };
    visual.simulate_keystrokes("tab");
    assert_eq!(
        focused(&mut visual),
        Some("voice-mic"),
        "Tab starts at the first action"
    );
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("voice-deaf"));
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(&mut visual), Some("voice-records"));
    visual.simulate_keystrokes("shift-tab");
    assert_eq!(
        focused(&mut visual),
        Some("voice-deaf"),
        "Shift+Tab walks backwards"
    );
    visual.simulate_keystrokes("shift-tab");
    assert_eq!(focused(&mut visual), Some("voice-mic"));
}

#[gpui::test]
async fn escape_and_f_leave_fullscreen_without_leaving_the_call(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    focus_room(&mut visual, &voice);
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_open_player(publication(7, "alice", 1));
            panel.player_toggle_fullscreen(window);
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.player_fullscreen());
    });
    visual.simulate_keystrokes("escape");
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.player_fullscreen(), "Esc leaves fullscreen");
        assert!(panel.call_active(), "the voice call stays");
        assert!(panel.player_open(), "the floating player returns");
    });
    view.read_with(&*visual, |v, _| assert_eq!(v.voice_channel, 7));

    // Fullscreen and F leave it again.
    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.player_toggle_fullscreen(window));
    });
    draw(&mut visual);
    visual.simulate_keystrokes("f");
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.player_fullscreen());
        assert!(panel.call_active());
    });
}

#[gpui::test]
async fn push_to_talk_press_and_release_reach_the_panel(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    focus_room(&mut visual, &voice);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice_settings.prefs.input_mode = "push_to_talk".into();
            v.voice_settings.prefs.push_to_talk = "Space".into();
            voice.update(cx, |panel, cx| {
                panel.set_push_to_talk(true);
                cx.notify();
            });
        });
        let _ = window;
    });
    draw(&mut visual);
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: Keystroke::parse("space").unwrap(),
        is_held: false,
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.test_ptt_held(), "the configured key opens the hold");
    });
    visual.simulate_event(gpui::KeyUpEvent {
        keystroke: Keystroke::parse("space").unwrap(),
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_ptt_held(), "the release closes the hold");
    });

    // Losing focus releases the hold too.
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: Keystroke::parse("space").unwrap(),
        is_held: false,
    });
    visual.update(|window, app| {
        voice.update(app, |panel, _| panel.release_push_to_talk());
        let _ = window;
    });
    voice.read_with(&*visual, |panel, _| assert!(!panel.test_ptt_held()));

    // Opening a modal blocks a new press.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.show_search = true;
            v.sync_push_to_talk_block(cx);
        });
        let _ = window;
    });
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: Keystroke::parse("space").unwrap(),
        is_held: false,
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_ptt_held(), "a modal owns the keyboard");
    });
}

#[gpui::test]
async fn room_snapshots_follow_the_server_and_open_with_their_streams(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice_rooms.insert((1, 8), vec![participant("carol", 3)]);
            v.voice_streams
                .insert((1, 8), vec![publication(8, "carol-screen", 3)]);
            assert_eq!(v.voice_count(8), 1, "the cached room is counted");
            assert!(v.voice_has_stream(8));
            v.channel = 8;
            v.open_voice_room(8, window, cx);
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.room(), 8);
        assert_eq!(
            panel.room_streams().len(),
            1,
            "room and streams move together"
        );
        assert_eq!(
            panel.room_streams()[0].publisher_connection_id,
            "carol-screen"
        );
    });

    // Another server keeps its own (server, channel) snapshot, so switching
    // does not need a new media event.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 2;
            v.channels = vec![channel(30, "Server B voice", "voice")];
            v.channel = 30;
            v.voice_rooms.insert((2, 30), vec![participant("dave", 4)]);
            v.voice_streams
                .insert((2, 30), vec![publication(30, "dave-screen", 4)]);
            v.open_voice_room(30, window, cx);
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.room(), 30);
        assert_eq!(panel.room_streams().len(), 1);
        assert_eq!(
            panel.room_streams()[0].publisher_connection_id,
            "dave-screen"
        );
    });
}

#[gpui::test]
async fn push_to_talk_closes_when_a_required_modifier_is_released(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    focus_room(&mut visual, &voice);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice_settings.prefs.input_mode = "push_to_talk".into();
            v.voice_settings.prefs.push_to_talk = "Ctrl+Space".into();
            voice.update(cx, |panel, cx| {
                panel.set_push_to_talk(true);
                cx.notify();
            });
        });
        let _ = window;
    });
    draw(&mut visual);

    // A fresh combination opens the hold.
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: Keystroke::parse("ctrl-space").unwrap(),
        is_held: false,
    });
    voice.read_with(&*visual, |panel, _| assert!(panel.test_ptt_held()));

    // Releasing only Ctrl breaks Ctrl+Space even though Space is still down.
    visual.simulate_event(gpui::KeyUpEvent {
        keystroke: Keystroke::parse("control").unwrap(),
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(
            !panel.test_ptt_held(),
            "the released modifier closes the hold"
        )
    });

    // An auto-repeat of the main key must not reopen it.
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: Keystroke::parse("ctrl-space").unwrap(),
        is_held: true,
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_ptt_held(), "auto-repeat never opens the hold")
    });

    // A modifier transition reported on its own also closes the hold.
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: Keystroke::parse("ctrl-space").unwrap(),
        is_held: false,
    });
    visual.simulate_event(gpui::ModifiersChangedEvent {
        modifiers: gpui::Modifiers {
            control: false,
            ..Default::default()
        },
        capslock: Default::default(),
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_ptt_held(), "ModifiersChanged releases the hold")
    });
}

#[gpui::test]
async fn the_room_and_player_use_the_real_layout_coordinates(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    let chat = view.read_with(&*visual, |v, _| {
        layout::compute_layout(v.viewport.0, v.viewport.1, v.shell, true, v.show_members).chat
    });
    let viewport = visual
        .debug_bounds("voice-room-viewport")
        .expect("the card viewport is painted");
    assert!(
        (f32::from(viewport.size.width) - (chat.w - 48.0)).abs() < 2.0,
        "Go keeps a 24 px inset on both sides: {viewport:?}"
    );
    let toolbar = visual
        .debug_bounds("voice-action-0")
        .expect("toolbar buttons are painted");
    assert!(f32::from(toolbar.size.width) > 60.0);
    voice.read_with(&*visual, |panel, _| {
        let (_, height) = panel.test_room_size();
        assert!(height > 120.0);
    });

    // The floating player starts at 360 px and stays inside the player area.
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_open_player(publication(7, "alice", 1));
        });
        let _ = window;
    });
    draw(&mut visual);
    let player = visual
        .debug_bounds("voice-player")
        .expect("the player is painted");
    assert!((f32::from(player.size.width) - 360.0).abs() < 1.0);
    assert!((f32::from(player.size.height) - (360.0 * 9.0 / 16.0 + 76.0)).abs() < 1.0);
    let area = voice.read_with(&*visual, |panel, _| panel.test_player_area());
    assert!(f32::from(player.origin.x) >= area.x + 8.0 - 0.5);
    assert!(f32::from(player.origin.y) >= area.y + 8.0 - 0.5);
}

#[gpui::test]
async fn unconfirmed_access_hides_the_active_room_and_blocks_join(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    visual.update(|_, app| {
        voice.update(app, |panel, _| {
            panel.test_set_streams(vec![publication(7, "alice", 1)]);
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_visible_roster_len(), 2);
        assert_eq!(panel.test_visible_streams_len(), 1);
        assert!(panel.test_audio_for("alice"));
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice_unconfirmed.insert(1);
            v.sync_voice_panel(cx);
        });
        let _ = window;
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_visible_roster_len(),
            0,
            "the active call roster must follow the ACL gate"
        );
        assert_eq!(panel.test_visible_streams_len(), 0);
        assert!(!panel.test_audio_for("alice"));
    });
    visual.update(|_, app| {
        voice.update(app, |panel, cx| {
            panel.join(cx);
            assert!(panel.error().contains("не подтверждены"));
        });
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.apply_confirmed_channels(1, vec![channel(7, "Room A", "voice")], window, cx);
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(!v.voice_unconfirmed.contains(&1));
    });
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_visible_roster_len(), 2);
        assert_eq!(panel.test_visible_streams_len(), 1);
        assert!(panel.test_audio_for("alice"));
    });
}

#[gpui::test]
async fn ended_in_fullscreen_restores_the_window_and_closes_the_player(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_player(publication(7, "alice", 1));
            panel.player_toggle_fullscreen(window);
            assert!(panel.player_fullscreen());
            panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Ended(Some(
                    "Голосовое соединение завершено сервером".into(),
                )),
                window,
                cx,
            );
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.player_fullscreen(), "revoke must leave fullscreen");
        assert!(!panel.player_open(), "the player is closed");
        assert!(!panel.call_active(), "the call is released");
    });
}

#[gpui::test]
async fn voice_participants_are_listed_under_their_channel(cx: &mut TestAppContext) {
    let (mut visual, view, _voice) = fixture(cx);
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            let mut bob = participant("bob", 2);
            bob.self_mute = true;
            v.voice_rooms
                .insert((1, 7), vec![participant("alice", 1), bob]);
            let mut dave = participant("dave", 4);
            dave.reconnecting = true;
            v.voice_rooms
                .insert((1, 8), vec![participant("carol", 3), dave]);
            cx.notify();
        });
    });
    draw(&mut visual);
    let channel7 = visual.debug_bounds("channel-7").expect("channel 7 row");
    let channel8 = visual.debug_bounds("channel-8").expect("channel 8 row");
    let alice = visual
        .debug_bounds("voice-member-alice")
        .expect("alice row");
    let bob = visual.debug_bounds("voice-member-bob").expect("bob row");
    let carol = visual
        .debug_bounds("voice-member-carol")
        .expect("carol row");
    let dave = visual
        .debug_bounds("voice-member-dave")
        .expect("a reconnecting member still has a row");
    // Rows belong to the channel above them, not just to the list.
    assert!(channel7.origin.y < alice.origin.y);
    assert!(alice.origin.y < bob.origin.y);
    assert!(bob.origin.y < channel8.origin.y);
    assert!(channel8.origin.y < carol.origin.y);
    assert!(carol.origin.y < dave.origin.y);
    // Muted and reconnecting members show the card's state icon; a healthy
    // member shows only the dot.
    assert!(visual.debug_bounds("voice-state-bob").is_some());
    assert!(visual.debug_bounds("voice-state-dave").is_some());
    assert!(visual.debug_bounds("voice-state-alice").is_none());
    assert!(visual.debug_bounds("voice-state-carol").is_none());
}

#[gpui::test]
async fn the_room_keeps_the_current_palette(cx: &mut TestAppContext) {
    let (visual, view, voice) = fixture(cx);
    // A real theme switch reaches an already running panel.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.theme_path = None;
            v.apply_theme(crate::theme::ThemeMode::Light, cx);
        });
    });
    assert!(view.read_with(&*visual, |v, _| v.palette.light));
    voice.read_with(&*visual, |panel, _| assert!(panel.test_palette().light));

    // A room opened later starts with the same palette.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice = None;
            v.open_voice_room(7, window, cx);
        });
    });
    let panel = view
        .read_with(&*visual, |v, _| v.voice.clone())
        .expect("a new panel");
    panel.read_with(&*visual, |panel, _| assert!(panel.test_palette().light));
}

#[gpui::test]
async fn many_voice_members_keep_the_account_footer_visible(cx: &mut TestAppContext) {
    let (mut visual, view, _voice) = fixture(cx);
    let roster: Vec<_> = (0..80)
        .map(|index| participant(&format!("member-{index}"), 100 + index))
        .collect();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.voice_rooms.insert((1, 7), roster);
            cx.notify();
        });
    });
    draw(&mut visual);
    let list = visual.debug_bounds("channel-list").expect("channel list");
    let footer = visual
        .debug_bounds("account-footer")
        .expect("account footer");
    let (_, viewport_h) = visual.update(|window, _| {
        let size = window.viewport_size();
        (f32::from(size.width), f32::from(size.height))
    });
    let list_bottom: f32 = (list.origin.y + list.size.height).into();
    let footer_bottom: f32 = (footer.origin.y + footer.size.height).into();
    assert!(
        list_bottom <= f32::from(footer.origin.y) + 0.5,
        "the member rows scroll instead of covering the footer"
    );
    assert!(
        footer_bottom <= viewport_h + 0.5,
        "the footer stays inside the window"
    );
}

#[gpui::test]
async fn speaking_changes_wake_the_parent_once(cx: &mut TestAppContext) {
    let (visual, _view, voice) = fixture(cx);
    let calls = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let counter = calls.clone();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.set_speaking_handler(Box::new(move |_cx| {
                counter.set(counter.get() + 1);
            }));
            let mut speaking = std::collections::HashMap::new();
            speaking.insert("alice".to_string(), true);
            let first = panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Speaking(speaking.clone()),
                window,
                cx,
            );
            // The runtime resends the same set while it is unchanged.
            let again = panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Speaking(speaking),
                window,
                cx,
            );
            assert!(first, "a real change repaints the panel");
            assert!(!again, "an unchanged speech set must not repaint");
        });
    });
    assert_eq!(
        calls.get(),
        1,
        "only an actual speech change repaints the sidebar"
    );
}

#[gpui::test]
async fn an_app_opened_room_wakes_the_sidebar_on_speech(cx: &mut TestAppContext) {
    let (mut visual, view, _voice) = fixture(cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice = None;
            v.open_voice_room(7, window, cx);
        });
    });
    let panel = view
        .read_with(&*visual, |v, _| v.voice.clone())
        .expect("panel");
    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            let mut speaking = std::collections::HashMap::new();
            speaking.insert("alice".to_string(), true);
            panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Speaking(speaking),
                window,
                cx,
            );
        });
    });
    // The production handler defers a parent update; drawing must drain it
    // without a re-entrant entity update.
    draw(&mut visual);
    panel.read_with(&*visual, |panel, _| assert!(panel.speaking("alice")));
}

#[gpui::test]
async fn dsp_controls_accept_the_web_ranges(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = fixture(cx);
    draw(&mut visual);
    voice.update(&mut *visual, |panel, _| {
        panel.set_dsp("threshold", 250, 999, false, false);
    });
    voice.read_with(&*visual, |panel, _| {
        let controls = panel.test_controls();
        assert_eq!(
            controls.noise_mode,
            voxhold_media::dsp::NoiseMode::Threshold
        );
        assert_eq!(controls.noise_gate_percent, 100);
        assert!((controls.input_gain - 2.0).abs() < f32::EPSILON);
        assert!(!controls.echo_cancellation);
        assert!(!controls.auto_gain_control);
    });
    voice.update(&mut *visual, |panel, _| {
        panel.set_dsp("auto", 15, 0, true, true);
    });
    voice.read_with(&*visual, |panel, _| {
        let controls = panel.test_controls();
        assert_eq!(controls.noise_mode, voxhold_media::dsp::NoiseMode::Auto);
        assert_eq!(controls.noise_gate_percent, 15);
        assert_eq!(controls.input_gain, 0.0);
    });
}

#[test]
fn voice_room_colors_follow_the_theme() {
    let dark_palette = crate::theme::dark_palette();
    let light_palette = crate::theme::light_palette();
    let dark = crate::voice_panel::VoiceColors::resolve(dark_palette);
    let light = crate::voice_panel::VoiceColors::resolve(light_palette);
    // The dark room keeps its fixed surfaces; the light room uses the light
    // workspace surfaces and text instead of the black card.
    assert_eq!(dark.surface, 0x1e1e1b);
    assert_eq!(light.surface, 0xe8e8e0);
    assert_eq!(dark.fg, 0xf3f4eb);
    assert_eq!(light.fg, 0x20211d);
    assert_ne!(dark.danger_bg, light.danger_bg);
    assert_ne!(dark.avatar_bg, light.avatar_bg);
    // The fill keeps its hue; the text accent reuses the theme ink so the
    // ring stays readable on a light surface.
    assert_eq!(dark.accent, light.accent);
    assert_eq!(dark.accent_text, dark_palette.accent_ink);
    assert_eq!(light.accent_text, light_palette.accent_ink);
}
