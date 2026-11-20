//! Voice shortcut recording, the typing guard and the power transition path.
//!
//! These drive the shipped handlers: the settings card key-down listener, the
//! root voice keyboard router and the power event that the window subclass
//! forwards.

use super::*;
use crate::app::hotkeys::shortcut_from_key;
use gpui::{Focusable, Keystroke, TestAppContext, VisualTestContext};

fn key(text: &str) -> gpui::KeyDownEvent {
    gpui::KeyDownEvent {
        keystroke: Keystroke::parse(text).unwrap(),
        is_held: false,
    }
}

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

/// The regular call fixture with the voice tab open and its card focused.
fn voice_tab(cx: &mut TestAppContext) -> (&'static mut VisualTestContext, Entity<Voxhold>) {
    let (mut visual, view, _voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            v.personal.as_mut().unwrap().dialog.active = 1;
            v.voice_settings.path = None;
            v.voice_settings.prefs.mute = "Ctrl+Shift+M".into();
            v.voice_settings.prefs.deafen = "Ctrl+Shift+D".into();
            v.voice_settings.prefs.push_to_talk = "Space".into();
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    // The card must own the keyboard; otherwise the simulated key would go to
    // the composer and the recorder would never see it.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            if let Some((handle, _)) = v.settings_ring.first() {
                let handle = handle.clone();
                window.focus(&handle);
            }
            cx.notify();
        });
    });
    draw(&mut visual);
    (visual, view)
}

#[gpui::test]
async fn a_duplicate_combination_is_rejected_and_the_recorder_stays_open(cx: &mut TestAppContext) {
    let (_visual, view) = voice_tab(cx);
    let event = key("ctrl-shift-d");
    assert_eq!(
        shortcut_from_key(
            &event.keystroke.key.to_lowercase(),
            event.keystroke.modifiers
        ),
        Some("Ctrl+Shift+D".to_owned()),
        "key={:?} mods={:?}",
        event.keystroke.key,
        event.keystroke.modifiers
    );
    view.update(cx, |v, cx| {
        v.voice_settings.saved_error = "предыдущая ошибка".into();
        v.start_shortcut_recording(0, cx);
        assert!(v.record_shortcut_key(&key("ctrl-shift-d"), cx));
        assert_eq!(
            v.voice_settings.prefs.mute, "Ctrl+Shift+M",
            "the previous mute binding is kept"
        );
        assert_eq!(
            v.voice_settings.recording_shortcut,
            Some(0),
            "the recorder stays open for another combination"
        );
        assert_eq!(
            v.voice_settings.shortcut_conflict,
            "Сочетание уже занято: Звук вкл/выкл. Нажмите другое."
        );
        assert_eq!(
            v.voice_settings.saved_error, "предыдущая ошибка",
            "a rejected attempt is not a save error"
        );
        assert_eq!(v.voice_settings.writes, 0);
        assert!(
            v.voice_settings
                .store
                .is_newest(v.voice_settings.saved_generation),
            "a rejected attempt must not queue a save"
        );
        // A unique combination after the rejection applies and clears the
        // notice.
        assert!(v.record_shortcut_key(&key("ctrl-shift-k"), cx));
        assert_eq!(v.voice_settings.prefs.mute, "Ctrl+Shift+K");
        assert_eq!(v.voice_settings.recording_shortcut, None);
        assert!(v.voice_settings.shortcut_conflict.is_empty());
        assert!(v.voice_settings.prefs.shortcuts_valid());
    });
}

#[gpui::test]
async fn recording_deafen_rejects_the_push_to_talk_combination(cx: &mut TestAppContext) {
    let (_visual, view) = voice_tab(cx);
    view.update(cx, |v, cx| {
        v.start_shortcut_recording(1, cx);
        assert!(v.record_shortcut_key(&key("space"), cx));
        assert_eq!(
            v.voice_settings.prefs.deafen, "Ctrl+Shift+D",
            "the previous deafen binding is kept"
        );
        assert_eq!(v.voice_settings.recording_shortcut, Some(1));
        assert_eq!(
            v.voice_settings.shortcut_conflict,
            "Сочетание уже занято: Нажми и говори. Нажмите другое."
        );
        assert_eq!(v.voice_settings.writes, 0);
        assert!(
            v.voice_settings
                .store
                .is_newest(v.voice_settings.saved_generation),
            "a rejected attempt must not queue a save"
        );
    });
}

#[gpui::test]
async fn equivalent_modifier_spelling_and_order_are_detected(cx: &mut TestAppContext) {
    let (_visual, view) = voice_tab(cx);
    view.update(cx, |v, cx| {
        v.voice_settings.prefs.deafen = "Shift+Control+D".into();
        v.start_shortcut_recording(0, cx);
        assert!(v.record_shortcut_key(&key("ctrl-shift-d"), cx));
        assert_eq!(v.voice_settings.prefs.mute, "Ctrl+Shift+M");
        assert_eq!(
            v.voice_settings.shortcut_conflict,
            "Сочетание уже занято: Звук вкл/выкл. Нажмите другое.",
            "Ctrl+Shift+D and Shift+Control+D are the same binding"
        );
    });
}

#[gpui::test]
async fn escape_clears_the_conflict_message_and_keeps_the_old_binding(cx: &mut TestAppContext) {
    let (visual, view) = voice_tab(cx);
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.voice_settings.saved_error = "предыдущая ошибка".into();
            v.start_shortcut_recording(0, cx);
            assert!(v.record_shortcut_key(&key("ctrl-shift-d"), cx));
            assert!(!v.voice_settings.shortcut_conflict.is_empty());
        });
    });
    visual.simulate_event(key("escape"));
    visual.update(|_, app| {
        view.update(app, |v, _| {
            assert_eq!(v.voice_settings.prefs.mute, "Ctrl+Shift+M");
            assert_eq!(v.voice_settings.recording_shortcut, None);
            assert!(v.voice_settings.shortcut_conflict.is_empty());
            assert_eq!(v.voice_settings.saved_error, "предыдущая ошибка");
        });
    });
}

#[gpui::test]
async fn re_pressing_the_own_combination_is_not_a_conflict(cx: &mut TestAppContext) {
    let (_visual, view) = voice_tab(cx);
    view.update(cx, |v, cx| {
        v.start_shortcut_recording(0, cx);
        assert!(v.record_shortcut_key(&key("ctrl-shift-m"), cx));
        assert_eq!(v.voice_settings.prefs.mute, "Ctrl+Shift+M");
        assert!(v.voice_settings.shortcut_conflict.is_empty());
        assert_eq!(v.voice_settings.recording_shortcut, None);
        assert_eq!(
            v.voice_settings.writes, 0,
            "keeping the same binding is a no-op, not a new write"
        );
    });
}

#[gpui::test]
async fn closing_the_voice_tab_clears_the_conflict_message(cx: &mut TestAppContext) {
    let (visual, view) = voice_tab(cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.start_shortcut_recording(0, cx);
            assert!(v.record_shortcut_key(&key("ctrl-shift-d"), cx));
            assert!(!v.voice_settings.shortcut_conflict.is_empty());
            v.close_settings(false, window, cx);
            assert!(v.voice_settings.shortcut_conflict.is_empty());
            assert_eq!(v.voice_settings.recording_shortcut, None);
        });
    });
}

#[gpui::test]
async fn escape_and_a_lone_modifier_keep_or_cancel_the_recorder(cx: &mut TestAppContext) {
    let (visual, view) = voice_tab(cx);
    visual.update(|_, app| {
        view.update(app, |v, cx| v.start_shortcut_recording(2, cx));
    });
    // A modifier-only press is not a binding; the recorder keeps waiting.
    visual.simulate_event(key("control"));
    visual.update(|_, app| {
        view.update(app, |v, _| {
            assert_eq!(v.voice_settings.recording_shortcut, Some(2));
            assert_eq!(v.voice_settings.prefs.push_to_talk, "Space");
        });
    });
    visual.simulate_event(key("escape"));
    visual.update(|_, app| {
        view.update(app, |v, _| {
            assert_eq!(v.voice_settings.recording_shortcut, None);
            assert_eq!(v.voice_settings.prefs.push_to_talk, "Space");
        });
    });
}

#[gpui::test]
async fn typing_in_the_composer_blocks_mute_deafen_and_push_to_talk(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    // Show the text channel so its composer is mounted, then give it the
    // keyboard. The call stays in room 7 in the panel.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.channel = 9;
            v.show_search = false;
            v.voice_settings.prefs.push_to_talk = "Space".into();
            v.voice_settings.prefs.input_mode = "push_to_talk".into();
            cx.notify();
        });
        voice.update(app, |panel, _| panel.set_push_to_talk(true));
    });
    draw(&mut visual);
    for _ in 0..2 {
        visual.update(|window, app| {
            view.update(app, |v, cx| {
                let handle = v.composer.read(cx).focus_handle(cx);
                window.focus(&handle);
            });
        });
        draw(&mut visual);
    }
    visual.simulate_event(key("ctrl-shift-m"));
    visual.simulate_event(key("space"));
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_self_muted(), "typing must not mute the call");
        assert!(!panel.test_ptt_held(), "typing must not open push-to-talk");
    });
    // Outside the composer the same combination works.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.channel = 7;
            cx.notify();
        });
    });
    draw(&mut visual);
    for _ in 0..2 {
        visual.update(|window, app| {
            let focus = voice.read(app).focus_handle();
            window.focus(&focus);
        });
        draw(&mut visual);
    }
    visual.simulate_event(key("ctrl-shift-m"));
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.test_self_muted(), "the shortcut works in the room");
    });
}

#[gpui::test]
async fn a_server_mute_outranks_a_held_push_to_talk_key(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.set_push_to_talk(true);
            panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::SelfConnection(Some("alice".into())),
                window,
                cx,
            );
            panel.activate_push_to_talk(crate::voice_room::Hotkey::parse("Space"));
            assert!(
                !panel.test_controls_muted(),
                "the fixture starts mutable: a held PTT opens the microphone"
            );
            // Another connection of the same user is not this client and must
            // not be mistaken for the authoritative row.
            let me = panel
                .test_self_participant("alice")
                .expect("the fixture has the self participant");
            let mut me = me;
            me.server_mute = true;
            let mut other = me.clone();
            other.connection_id = "alice-second".into();
            other.server_mute = false;
            panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Participants(vec![other, me]),
                window,
                cx,
            );
        });
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_self_muted(), "the local mute is not the cause");
        assert!(
            panel.test_controls_muted(),
            "the microphone stays closed while the server mute is active"
        );
        assert!(
            panel.test_ptt_held(),
            "the hold is kept, so only the server-mute flag can silence it"
        );
    });
}

#[gpui::test]
async fn per_user_sliders_are_keyed_by_user_id_beyond_sixteen_participants(
    cx: &mut TestAppContext,
) {
    use voxhold_media::controller::Participant;
    let (visual, view, voice) = super::voice_tests::fixture(cx);
    let roster = |range: std::ops::RangeInclusive<i64>| {
        range
            .map(|user| Participant {
                connection_id: format!("conn-{user}"),
                user_id: user,
                server_id: 1,
                channel_id: 7,
                ..Participant::default()
            })
            .collect::<Vec<_>>()
    };
    let mut after = roster(2..=17);
    after.extend(roster(19..=19));
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Participants(roster(1..=18)),
                window,
                cx,
            );
        });
    });
    voice.read_with(&*visual, |panel, _| {
        let rows = panel.call_participants();
        assert_eq!(
            rows.len(),
            18,
            "the list is not truncated at the old sixteen-slot cap"
        );
        assert!(rows.iter().any(|(user, _)| *user == 18));
    });
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            assert!(
                v.voice_settings_action(personal_settings::Action::SetVoiceUserVolume(18, 150), cx)
            );
        });
    });
    // The value is keyed by the user, so a roster change cannot move it to a
    // neighbour row.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Participants(after.clone()),
                window,
                cx,
            );
        });
    });
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.user_volume(18), 150, "user 18 keeps its own value");
        assert_eq!(
            panel.user_volume(19),
            100,
            "the new participant does not inherit a neighbour's value"
        );
        assert!(
            !panel
                .call_participants()
                .iter()
                .any(|(user, _)| *user == 18),
            "the departed user has no row"
        );
    });
    assert_eq!(
        super::settings_voice_view::user_volume_slider_id(18),
        "user-volume-18",
        "the slider id is the stable user id, not a slot index"
    );
}

#[gpui::test]
async fn suspend_closes_the_microphone_and_resume_reopens_the_median_path(cx: &mut TestAppContext) {
    let (visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_, app| {
        voice.update(app, |panel, _| panel.test_attach_detached_handle());
        view.update(app, |v, cx| v.power_event(false, cx));
    });
    visual.update(|_, app| {
        voice.update(app, |panel, _| {
            assert!(panel.test_power_paused());
            assert!(panel.test_controls_muted(), "a suspended system is silent");
            assert_eq!(
                panel.test_take_power(),
                Some(false),
                "the suspend reaches the real media request slot"
            );
        });
        view.update(app, |v, cx| v.power_event(true, cx));
    });
    visual.update(|_, app| {
        voice.update(app, |panel, _| {
            assert!(!panel.test_power_paused());
            assert_eq!(panel.test_take_power(), Some(true));
        });
    });
}
