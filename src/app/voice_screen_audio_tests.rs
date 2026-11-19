//! Audio-scope regressions of the screen picker: a fresh share is silent, a
//! whole-screen share excludes Voxhold, a window share carries its own process,
//! and the explicit whole-device choice stays distinct.

use super::voice_screen_tests::{draw, open_picker, open_picker_with, window};
use gpui::TestAppContext;
use voxhold_media::screen_capture::SourceKind;
use voxhold_media::system_audio::AudioScope;

#[gpui::test]
async fn system_audio_is_off_by_default_and_the_scope_is_explicit(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    voice.read_with(&*visual, |panel, _| {
        assert!(
            !panel.test_screen_request().audio,
            "a fresh monitor share must not publish the endpoint silently"
        );
        assert_eq!(panel.test_audio_toggle_label(), "Выключен");
        assert!(
            !panel.test_audio_scope_enabled(),
            "the scope choices are inert while audio is off"
        );
        assert_eq!(panel.test_audio_scope(), AudioScope::EverythingExceptSelf);
        assert!(
            panel
                .test_audio_scope_sentence()
                .contains("кроме процессов Voxhold"),
            "{}",
            panel.test_audio_scope_sentence()
        );
    });

    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_toggle_system_audio(cx);
            assert!(panel.test_audio_scope_enabled());
            let request = panel.test_screen_request();
            assert!(request.audio);
            assert_eq!(
                request.audio_scope,
                AudioScope::EverythingExceptSelf,
                "a whole-screen share excludes Voxhold by default"
            );
            panel.test_select_audio_scope(AudioScope::Device, cx);
            assert_eq!(
                panel.test_screen_request().audio_scope,
                AudioScope::Device,
                "the explicit whole-device choice is a distinct scope"
            );
            assert_ne!(
                panel.test_screen_request().audio_scope,
                AudioScope::EverythingExceptSelf
            );
        });
    });

    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Window,
        vec![window(9, "Браузер")],
    );
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_select_screen_source(window(9, "Браузер"), cx);
            assert_eq!(
                panel.test_audio_scope(),
                AudioScope::WindowApplication,
                "a window offers its own process by default"
            );
            panel.test_toggle_system_audio(cx);
            let request = panel.test_screen_request();
            assert!(request.audio);
            assert_eq!(
                request.audio_scope,
                AudioScope::WindowApplication,
                "the request carries the selected window scope"
            );
            assert!(
                panel.test_audio_scope_sentence().contains("окна"),
                "{}",
                panel.test_audio_scope_sentence()
            );
        });
    });
}
