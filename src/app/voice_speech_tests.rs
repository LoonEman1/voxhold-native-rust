use super::voice_tests::{fixture, participant};
use crate::voice_room::CallPhase;
use gpui::TestAppContext;
use voxhold_media::runtime::VoiceUpdate;

#[gpui::test]
async fn the_local_speech_indicator_tracks_only_this_connection(cx: &mut TestAppContext) {
    let (visual, _, voice) = fixture(cx);
    let wakeups = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = wakeups.clone();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.set_speaking_handler(Box::new(move |_| count.set(count.get() + 1)));
            panel.test_attach_detached_handle();
            panel.test_set_call(
                7,
                CallPhase::Connecting,
                vec![participant("alice", 1)],
                vec![],
            );
            panel.test_apply_update(
                VoiceUpdate::SelfConnection(Some("alice".into())),
                window,
                cx,
            );
            panel.test_apply_update(VoiceUpdate::Connected, window, cx);

            assert!(panel.test_apply_update(
                VoiceUpdate::LocalLevel {
                    level: 0.02,
                    speaking: true
                },
                window,
                cx
            ));
            assert!(
                panel.speaking("alice"),
                "quiet speech still crosses the indicator threshold"
            );
            assert!(!panel.speaking("another-connection"));
            assert!(!panel.test_apply_update(
                VoiceUpdate::LocalLevel {
                    level: 0.03,
                    speaking: true
                },
                window,
                cx
            ));
            assert_eq!(wakeups.get(), 1, "only the edge repaints the sidebar");

            panel.mute(cx);
            assert!(!panel.speaking("alice"));
            assert!(panel.test_apply_update(
                VoiceUpdate::LocalLevel {
                    level: 0.02,
                    speaking: false
                },
                window,
                cx
            ));
            assert_eq!(wakeups.get(), 2);
            panel.mute(cx);
            assert!(
                !panel.speaking("alice"),
                "unmuting needs a fresh speech sample"
            );
            panel.test_apply_update(
                VoiceUpdate::LocalLevel {
                    level: 0.06,
                    speaking: true,
                },
                window,
                cx,
            );
            assert!(panel.speaking("alice"));
            panel.test_apply_update(VoiceUpdate::Reconnecting, window, cx);
            assert!(
                !panel.speaking("alice"),
                "reconnecting erases the old indicator"
            );
        });
    });
}
