//! Local interface cues for the voice room: the panel records one cue per real
//! transition and the parent drains it through the saved preferences. These
//! tests never open an audio device; the app sink captures the same call the
//! WinMM player would receive.

use super::*;
use crate::ui_sounds::VoiceCue;
use crate::voice_room::CallPhase;
use gpui::TestAppContext;
use voxhold_media::controller::Participant;
use voxhold_media::runtime::VoiceUpdate;

fn panel_cues(panel: &mut voice_panel::VoicePanel) -> Vec<VoiceCue> {
    panel.take_cues()
}

#[gpui::test]
async fn join_and_leave_cues_follow_confirmed_transitions(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture_with(cx, None, false);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel_cues(panel).is_empty(), "an idle panel is silent");

            panel.test_apply_update(VoiceUpdate::Connecting, window, cx);
            assert!(
                panel_cues(panel).is_empty(),
                "Connecting is not a confirmed join"
            );

            panel.test_apply_update(VoiceUpdate::Connected, window, cx);
            assert_eq!(panel_cues(panel), vec![VoiceCue::SelfJoin]);

            // Roster, tracks and stream snapshots are not transitions. A frame
            // or a repeated Connected must never replay the join cue.
            panel.test_apply_update(
                VoiceUpdate::Participants(vec![
                    super::voice_tests::participant("alice", 1),
                    super::voice_tests::participant("bob", 2),
                ]),
                window,
                cx,
            );
            panel.test_apply_update(
                VoiceUpdate::Tracks(["alice".into()].into_iter().collect()),
                window,
                cx,
            );
            panel.test_apply_update(VoiceUpdate::Streams(Vec::new()), window, cx);
            panel.test_apply_update(VoiceUpdate::Connected, window, cx);
            assert!(
                panel_cues(panel).is_empty(),
                "roster and repeated Connected updates are silent"
            );

            // The runtime emits Reconnecting and then a fresh Connecting before
            // the confirmed Connected; neither may replay the join cue.
            panel.test_apply_update(VoiceUpdate::Reconnecting, window, cx);
            panel.test_apply_update(VoiceUpdate::Connecting, window, cx);
            panel.test_apply_update(VoiceUpdate::Connected, window, cx);
            assert!(
                panel_cues(panel).is_empty(),
                "a reconnect dedupes the join cue"
            );

            panel.test_apply_update(VoiceUpdate::Ended(None), window, cx);
            assert_eq!(panel_cues(panel), vec![VoiceCue::SelfLeave]);
            panel.test_apply_update(VoiceUpdate::Ended(None), window, cx);
            assert!(
                panel_cues(panel).is_empty(),
                "a repeated Idle state is silent"
            );

            panel.test_apply_update(VoiceUpdate::Connected, window, cx);
            assert_eq!(panel_cues(panel), vec![VoiceCue::SelfJoin]);
        });
    });
}

#[gpui::test]
async fn a_failed_connection_leaves_once(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture_with(cx, None, false);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(VoiceUpdate::Connecting, window, cx);
            panel.test_apply_update(VoiceUpdate::Ended(Some("refused".into())), window, cx);
            assert_eq!(panel_cues(panel), vec![VoiceCue::SelfLeave]);
            panel.test_apply_update(VoiceUpdate::Ended(Some("refused".into())), window, cx);
            assert!(panel_cues(panel).is_empty(), "Failed is not replayed");
            assert_eq!(panel.phase(), CallPhase::Failed);
        });
    });
}

#[gpui::test]
async fn local_toggles_cue_once_and_server_state_stays_silent(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.mute(cx);
            assert_eq!(panel_cues(panel), vec![VoiceCue::Mute]);
            panel.mute(cx);
            assert_eq!(panel_cues(panel), vec![VoiceCue::Unmute]);

            panel.deafen(cx);
            assert_eq!(panel_cues(panel), vec![VoiceCue::Deafen]);
            panel.deafen(cx);
            assert_eq!(panel_cues(panel), vec![VoiceCue::Undeafen]);

            // Push-to-talk changes the capture permission, not the local mute
            // state; it has no cue.
            panel.set_push_to_talk(true);
            panel.activate_push_to_talk(crate::voice_room::Hotkey::parse("Space"));
            panel.release_push_to_talk();
            assert!(panel_cues(panel).is_empty(), "PTT is silent");

            // A settings refresh and a participant update are not user toggles.
            panel.apply_voice_prefs(
                Some("mic-a".into()),
                Some("spk-a".into()),
                64,
                100,
                100,
                "auto",
                15,
                false,
                true,
                Default::default(),
            );
            panel.test_apply_update(
                VoiceUpdate::Participants(vec![
                    super::voice_tests::participant("alice", 1),
                    super::voice_tests::participant("bob", 2),
                ]),
                window,
                cx,
            );
            assert!(
                panel_cues(panel).is_empty(),
                "a refresh or roster change is silent"
            );

            // The server owns the effective microphone state; a local toggle
            // under it is not the user's own action and stays silent.
            panel.test_apply_update(
                VoiceUpdate::SelfConnection(Some("alice".into())),
                window,
                cx,
            );
            let mut me: Participant = super::voice_tests::participant("alice", 1);
            me.server_mute = true;
            panel.test_apply_update(VoiceUpdate::Participants(vec![me]), window, cx);
            panel.mute(cx);
            assert!(
                panel_cues(panel).is_empty(),
                "server mute is not a local cue"
            );
        });
    });
}

#[gpui::test]
async fn an_unmounted_room_delivers_server_cues_through_the_call_handler(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    super::voice_tests::draw(&mut visual);

    // Switch to the text channel so the voice room pane is not painted, then
    // install the same deferred handler the app installs when it opens a room.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice_settings.prefs.sounds = true;
            v.voice_settings.prefs.sound_volume = 30;
            v.voice_settings.prefs.output = "2:Speakers".into();
            v.voice_cue_calls.clear();
            v.install_voice_call_handler(cx);
            v.channel = 9;
            cx.notify();
        });
        let _ = window;
    });
    super::voice_tests::draw(&mut visual);
    view.read_with(&*visual, |v, _| assert!(!v.voice_room_visible()));

    // A server-driven leave must reach the player without a shell repaint or
    // an explicit refresh_voice_call; the deferred handler fills the gap.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(VoiceUpdate::Ended(None), window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_cue_calls,
            vec![CueCall {
                cue: VoiceCue::SelfLeave,
                volume: 30,
                output: "2:Speakers".into(),
            }],
            "a leave is delivered while the room is unmounted"
        );
    });

    // The same path carries the confirmed join after a fresh connection.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(VoiceUpdate::Connected, window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_cue_calls,
            vec![
                CueCall {
                    cue: VoiceCue::SelfLeave,
                    volume: 30,
                    output: "2:Speakers".into(),
                },
                CueCall {
                    cue: VoiceCue::SelfJoin,
                    volume: 30,
                    output: "2:Speakers".into(),
                },
            ],
            "a join is delivered while the room is unmounted"
        );
    });

    // A later shell repaint drains an empty queue: no duplicate cue.
    super::voice_tests::draw(&mut visual);
    view.read_with(&*visual, |v, _| assert_eq!(v.voice_cue_calls.len(), 2));
}

#[gpui::test]
async fn the_app_drains_cues_through_the_saved_preferences(cx: &mut TestAppContext) {
    let (visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice_settings.prefs.sounds = true;
            v.voice_settings.prefs.sound_volume = 40;
            v.voice_settings.prefs.output = "2:Speakers".into();
            v.voice_cue_calls.clear();

            voice.update(cx, |panel, cx| panel.mute(cx));
            v.refresh_voice_call(window, cx);
            assert_eq!(
                v.voice_cue_calls,
                vec![CueCall {
                    cue: VoiceCue::Mute,
                    volume: 40,
                    output: "2:Speakers".into(),
                }],
                "the selected output and volume reach the player"
            );

            // The checkbox off drops the cue without a player call.
            v.voice_settings.prefs.sounds = false;
            voice.update(cx, |panel, cx| panel.mute(cx));
            v.refresh_voice_call(window, cx);
            assert_eq!(v.voice_cue_calls.len(), 1, "sounds off is silent");

            // Volume 0 is also silent, and neither case leaves a cue queued.
            v.voice_settings.prefs.sounds = true;
            v.voice_settings.prefs.sound_volume = 0;
            voice.update(cx, |panel, cx| panel.mute(cx));
            v.refresh_voice_call(window, cx);
            assert_eq!(v.voice_cue_calls.len(), 1, "volume 0 is silent");

            // The next audible toggle uses the current prefs and plays once.
            v.voice_settings.prefs.sound_volume = 25;
            voice.update(cx, |panel, cx| panel.mute(cx));
            v.refresh_voice_call(window, cx);
            assert_eq!(
                v.voice_cue_calls,
                vec![
                    CueCall {
                        cue: VoiceCue::Mute,
                        volume: 40,
                        output: "2:Speakers".into(),
                    },
                    CueCall {
                        cue: VoiceCue::Unmute,
                        volume: 25,
                        output: "2:Speakers".into(),
                    },
                ],
                "a suppressed cue is dropped, not replayed later"
            );
        });
    });
}
