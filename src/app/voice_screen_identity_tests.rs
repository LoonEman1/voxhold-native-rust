//! Selection identity of the screen picker: a capturer number is not a
//! display, so a reused number is refused and a reorder follows the surface.

use super::voice_screen_tests::{draw, monitor_on, open_picker_with};
use gpui::TestAppContext;
use voxhold_media::screen_capture::SourceKind;

#[gpui::test]
async fn a_monitor_id_that_now_names_another_display_is_refused(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let selected = monitor_on(0, 4, "Экран A", 1920, 1080);
    // The capturer now reports the same numeric id for a different display.
    let intruder = monitor_on(0, 6, "Экран B", 2560, 1440);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![selected.clone()],
    );
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_select_screen_source(selected.clone(), cx);
            assert!(panel.test_inject_screen_preview(window, &selected, 8, 8));
            panel.test_apply_screen_sources(window, SourceKind::Monitor, vec![intruder.clone()]);
            assert_eq!(
                panel.test_selected_source_id(),
                None,
                "a moved capturer id is not the display the user chose"
            );
            assert_eq!(
                panel.test_screen_preview_counts().0,
                0,
                "the preview of the old display must be released"
            );
            assert!(!panel.test_can_start());
            panel.test_start_screen_share(window, cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "stream.start must never be sent for the other display"
            );
        });
    });
}

#[gpui::test]
async fn a_reordered_capturer_starts_the_same_display_under_its_new_id(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let selected = monitor_on(0, 4, "Экран A", 1920, 1080);
    let other = monitor_on(1, 6, "Экран B", 2560, 1440);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![selected.clone(), other.clone()],
    );
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_select_screen_source(selected.clone(), cx);
            // The capturer swapped the numbers: A is now id 1, B is id 0.
            panel.test_apply_screen_sources(
                window,
                SourceKind::Monitor,
                vec![
                    monitor_on(0, 6, "Экран B", 2560, 1440),
                    monitor_on(1, 4, "Экран A", 1920, 1080),
                ],
            );
            assert_eq!(
                panel.test_selected_source_id(),
                Some(monitor_on(1, 4, "Экран A", 1920, 1080).source_id()),
                "the choice follows the display, not the number"
            );
            assert!(panel.test_can_start());
            panel.test_start_screen_share(window, cx);
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("publish", 7)],
                "the publication still starts"
            );
            let request = panel.test_screen_request();
            assert_eq!(request.source.id, 1, "the current capturer id is 1");
            assert_eq!(
                request.source.identity(),
                selected.identity(),
                "the request still names display A"
            );
        });
    });
}
