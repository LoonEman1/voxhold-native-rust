//! C06: a codec change in the live quality dialog is a restart, while a
//! bitrate-only change stays on the running sender.

use gpui::TestAppContext;

use super::voice_screen_tests::{draw, focused, source};

#[gpui::test]
async fn a_codec_change_in_the_live_dialog_warns_and_queues_a_restart(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Codec, Phase, Quality, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|_window, app| {
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
    visual.update(|_window, app| {
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
