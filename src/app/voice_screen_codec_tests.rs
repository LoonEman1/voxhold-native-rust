//! C06: a codec change in the live quality dialog is a restart, while a
//! bitrate-only change stays on the running sender.

use gpui::TestAppContext;

use super::voice_screen_tests::{draw, focused, monitor, open_picker_with, source};
use voxhold_media::screen_capture::SourceKind;

#[gpui::test]
async fn a_codec_change_in_the_live_dialog_warns_and_queues_a_restart(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Codec, Phase, Quality, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_settings(
                source(),
                Sharing {
                    phase: Phase::Live,
                    live: true,
                    quality: Some(Quality::default()),
                    has_audio: true,
                    ..Sharing::default()
                },
                window,
                cx,
            );
            let confirmed = panel
                .test_screen_sharing()
                .confirmed
                .expect("the live attempt is confirmed")
                .codec;
            let other = Codec::ALL
                .into_iter()
                .find(|codec| *codec != confirmed)
                .expect("four codecs");
            // Both the codec and the bitrate change. Before the fix the codec
            // was missing from `restart_request`, so this was classified as a
            // live bitrate apply and the dialog promised no restart.
            panel.test_set_screen_codec(other);
            panel.test_change_screen_bitrate(100, cx);
            assert_eq!(panel.test_screen_request().codec, other);
            let warning = panel.test_restart_warning().expect("a live change warns");
            assert!(
                warning.contains("перезапустит"),
                "a codec change restarts the stream: {warning}"
            );
        });
    });

    for _ in 0..40 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    visual.simulate_keystrokes("enter");
    let (commands, op) = visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            (
                panel.test_take_screen_commands(),
                panel.test_pending_quality_op(),
            )
        })
    });
    assert_eq!(
        commands,
        vec![("restart", op.expect("the restart carries an operation id"))],
        "a codec change must go through the restart/preflight path"
    );
}

#[gpui::test]
async fn a_bitrate_only_change_in_the_live_dialog_queues_the_live_sender(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Quality, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_settings(
                source(),
                Sharing {
                    phase: Phase::Live,
                    live: true,
                    quality: Some(Quality::default()),
                    has_audio: true,
                    ..Sharing::default()
                },
                window,
                cx,
            );
            panel.test_change_screen_bitrate(100, cx);
            let warning = panel.test_restart_warning().expect("a live change warns");
            assert!(
                warning.contains("только битрейт"),
                "a bitrate-only change applies in the air: {warning}"
            );
        });
    });

    for _ in 0..40 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    visual.simulate_keystrokes("enter");
    let (commands, op) = visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            (
                panel.test_take_screen_commands(),
                panel.test_pending_quality_op(),
            )
        })
    });
    assert_eq!(
        commands,
        vec![("bitrate", op.expect("the apply carries an operation id"))],
        "only the bitrate reaches the live sender"
    );
}

/// Part A: a codec the sender probe refuses shows the probe's reason in the
/// dialog and never silently names another codec.
#[gpui::test]
async fn an_unavailable_codec_shows_the_probe_reason_in_the_dialog(cx: &mut TestAppContext) {
    let reason = "Профиль VP9 не объявлен кодировщиком для SDR: profile-id=2, для SDR нужен 0.";
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![monitor(3, "Экран", 1920, 1080)],
    );
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_codec_error(reason);
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(
        visual
            .debug_bounds("share-codec-label:Нет доступного SDR-кодека")
            .is_some(),
        "the label names no available SDR codec"
    );
    let reason_selector = Box::leak(format!("share-codec-reason:{reason}").into_boxed_str());
    assert!(
        visual.debug_bounds(reason_selector).is_some(),
        "the dialog paints «{reason_selector}»"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            !panel.test_can_start(),
            "Start refuses the unavailable codec"
        );
    });
}
