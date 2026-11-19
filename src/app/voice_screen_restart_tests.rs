//! Restart-dialog regressions: the live quality dialog binds to the source the
//! capture engine already accepted, while a normal Start still needs a row in
//! the current list.

use super::voice_screen_tests::{draw, focused, monitor, wait_for, window};
use gpui::TestAppContext;
use std::sync::Arc;
use std::time::Duration;
use voxhold_media::screen_capture::SourceKind;
use voxhold_media::system_audio::AudioDeviceInfo;

#[gpui::test]
async fn a_live_window_restart_binds_to_the_confirmed_source_with_an_empty_list(
    cx: &mut TestAppContext,
) {
    use voxhold_media::publisher::{Phase, Quality, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            // The captured window is not in the picker list: its answer is
            // empty/stale, as it is after the window set changed.
            panel.test_open_screen_picker_with(SourceKind::Window, Vec::new(), None);
            panel.test_set_screen_audio_pending(Arc::new(|| {
                std::thread::sleep(Duration::from_millis(150));
                Ok(AudioDeviceInfo {
                    id: "test".into(),
                    label: "Устройство вывода по умолчанию".into(),
                    format: "48000 Гц · 2 канала · 16 бит PCM".into(),
                })
            }));
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(Quality::default()),
                has_audio: true,
                ..Sharing::default()
            });
            panel.test_set_screen_last_request(live.clone());
            panel.test_open_live_screen_settings(cx);

            assert!(panel.screen_share_open(), "the restart dialog opened");
            assert!(panel.test_screen_restart());
            assert_eq!(
                panel.test_share_kind(),
                SourceKind::Window,
                "the dialog shows the live source's category"
            );
            assert_eq!(
                panel.test_selected_source_id(),
                Some(live.source_id()),
                "the restart is bound to the confirmed live identity"
            );
            assert_eq!(
                panel.test_selected_source(),
                Some(live.clone()),
                "the confirmed live row is inserted into the empty list"
            );
            assert!(
                !panel.test_can_start(),
                "Start still waits for the repeated endpoint probe"
            );
            assert!(panel.test_screen_error().is_empty());
        });
    });

    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());
    draw(&mut visual);

    for _ in 0..40 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(focused(&mut visual, &voice).as_deref(), Some("Start"));
    visual.simulate_keystrokes("enter");
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(
        commands,
        vec![("restart", 0)],
        "Start in a window restart queues exactly one restart: {commands:?}"
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_request().source, live);
    });

    // The general Start rule is unchanged: a normal share whose id left the
    // current list is refused visibly and never reaches the worker.
    let removed = monitor(3, "Экран A", 1920, 1080);
    let replacement = monitor(5, "Экран B", 2560, 1440);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_screen_picker_with(SourceKind::Monitor, vec![removed.clone()], None);
            panel.test_select_screen_source(removed, cx);
            panel.test_replace_screen_list_without_refresh(SourceKind::Monitor, vec![replacement]);
            assert!(!panel.test_can_start(), "the id is no longer in the list");
            panel.test_start_screen_share(cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a normal Start must not use a removed source"
            );
            assert!(
                panel.test_screen_error().contains("больше недоступен"),
                "the refusal stays visible: {}",
                panel.test_screen_error()
            );
        });
    });
}

#[gpui::test]
async fn a_live_restart_uses_the_current_row_and_survives_a_stale_refresh(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Quality, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    let renamed = window(11, "Документ · обновлён");
    let sharing = Sharing {
        phase: Phase::Live,
        live: true,
        quality: Some(Quality::default()),
        has_audio: false,
        ..Sharing::default()
    };
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            // A refresh already described the live window with a newer title;
            // the restart must keep that row instead of the remembered copy.
            panel.test_open_screen_picker_with(SourceKind::Window, vec![renamed.clone()], None);
            panel.test_set_screen_sharing(sharing.clone());
            panel.test_set_screen_last_request(live.clone());
            panel.test_open_live_screen_settings(cx);
            assert_eq!(panel.test_share_kind(), SourceKind::Window);
            assert_eq!(
                panel.test_selected_source(),
                Some(renamed.clone()),
                "a present row keeps its newer title and size"
            );
            assert!(panel.test_can_start());
            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(commands, vec![("restart", 0)]);
            assert_eq!(
                panel.test_screen_request().source,
                renamed,
                "the restart request is built from the current row"
            );

            // An answer that lost the live window must not unbind the restart:
            // the capture engine accepted the source and only it can report
            // the failure.
            panel.test_open_screen_picker_with(SourceKind::Window, Vec::new(), None);
            panel.test_set_screen_sharing(sharing.clone());
            panel.test_set_screen_last_request(live.clone());
            panel.test_open_live_screen_settings(cx);
            panel.test_apply_screen_sources(SourceKind::Window, Vec::new());
            assert_eq!(panel.test_selected_source_id(), Some(live.source_id()));
            assert_eq!(panel.test_selected_source(), Some(live.clone()));
            assert!(
                panel.test_can_start(),
                "the confirmed live source stays startable"
            );
            assert!(
                panel.test_screen_error().is_empty(),
                "a stale refresh must not claim the live source is gone"
            );
        });
    });
}
