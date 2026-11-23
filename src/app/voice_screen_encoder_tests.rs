//! Encoder UI minimum: the processor default, the GPU row and the explicit
//! «Продолжить на процессоре» action of a failed hardware attempt.

use super::voice_screen_tests::{draw, monitor, open_picker_with, tab_to};
use gpui::TestAppContext;
use voxhold_media::screen_capture::SourceKind;

/// The encoder backend line is part of the dialog and the default is the
/// processor: a build without a GPU/driver never asks the user to pick one.
#[gpui::test]
async fn the_picker_paints_the_encoder_backend_and_defaults_to_the_processor(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1200.), true);
    draw(&mut visual);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![monitor(3, "Экран", 1920, 1080)],
    );
    assert!(
        visual.debug_bounds("share-encoder").is_some(),
        "the picker must paint the «Кодирование» row"
    );
    let label = "share-encoder-label:Кодирование: Процессор · программный WebRTC";
    assert!(
        visual.debug_bounds(label).is_some(),
        "the default backend must be the processor"
    );
    let reason = "share-encoder-reason:SDR · программное кодирование";
    assert!(
        visual.debug_bounds(reason).is_some(),
        "the processor line names the path"
    );
}

#[gpui::test]
async fn a_lost_gpu_encoder_offers_the_cpu_continuation(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Failed,
                error: Some("Аппаратный кодировщик остановился: устройство отключено".into()),
                cpu_fallback: true,
                ..Sharing::default()
            });
        });
    });
    draw(&mut visual);
    let button = visual
        .debug_bounds("screen-continue-cpu")
        .expect("a lost GPU encoder must offer «Продолжить на процессоре»");
    visual.simulate_click(button.center(), gpui::Modifiers::default());
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            let commands = panel.test_take_screen_commands();
            assert!(
                commands.iter().any(|(kind, _)| *kind == "continue-cpu"),
                "the explicit action must reach the worker: {commands:?}"
            );
        });
    });
}

#[gpui::test]
async fn gpu_picker_explains_an_unsupported_capture_rate(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1280.), true);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![monitor(3, "Экран", 1920, 1080)],
    );
    for option in ["Fps(144)", "EncoderBackend(Gpu)"] {
        tab_to(&mut visual, &voice, option);
        visual.simulate_keystrokes("enter");
        draw(&mut visual);
    }
    assert!(visual
        .debug_bounds("share-encoder-reason:Частота кадров вне диапазона 1–120.")
        .is_some());
}
