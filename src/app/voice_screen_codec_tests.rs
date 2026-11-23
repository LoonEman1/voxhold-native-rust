//! C06: a codec change in the live quality dialog is a restart, while a
//! bitrate-only change stays on the running sender.

use gpui::TestAppContext;

use super::voice_screen_tests::{draw, monitor, open_picker_with, source, tab_to, wait_for};
use voxhold_media::publisher::{Codec, CodecIntent};
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

    tab_to(&mut visual, &voice, "Start");
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

    tab_to(&mut visual, &voice, "Start");
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

const VP9_REASON: &str = "Кодировщик VP9 отсутствует в этой сборке WebRTC.";
const AV1_REASON: &str =
    "Профиль AV1 не объявлен кодировщиком для SDR: profile=2, для SDR нужен Main (0).";

fn all_four_available() -> Vec<(Codec, Option<String>)> {
    Codec::ALL.into_iter().map(|codec| (codec, None)).collect()
}

/// P2: the dialog offers the website's «Автоматически» and the four SDR
/// codecs. An item this build cannot encode is painted with its probe reason
/// and is inert: Enter on the greyed row cannot select it.
#[gpui::test]
async fn the_dialog_offers_auto_and_the_four_sdr_codecs(cx: &mut TestAppContext) {
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
            panel.test_set_screen_codec_availability(vec![
                (Codec::H264, None),
                (Codec::Vp8, None),
                (Codec::Vp9, Some(VP9_REASON.into())),
                (Codec::Av1, Some(AV1_REASON.into())),
            ]);
            cx.notify();
        });
    });
    draw(&mut visual);
    for option in ["Автоматически", "H.264", "VP8", "VP9", "AV1"] {
        let selector: &'static str = Box::leak(format!("share-option-{option}").into_boxed_str());
        assert!(
            visual.debug_bounds(selector).is_some(),
            "{selector} is painted"
        );
    }
    let reasons = Box::leak(
        format!(
            "share-codec-unavailable:VP9 недоступен: {VP9_REASON} · AV1 недоступен: {AV1_REASON}"
        )
        .into_boxed_str(),
    );
    assert!(
        visual.debug_bounds(reasons).is_some(),
        "the greyed codecs paint their probe reasons"
    );

    tab_to(&mut visual, &voice, "Codec(Explicit(Vp9))");
    visual.simulate_keystrokes("enter");
    let intent = voice.read_with(&*visual, |panel, _| panel.test_screen_codec_intent());
    assert_eq!(
        intent,
        CodecIntent::Auto,
        "an unavailable codec is not a selectable item"
    );
}

/// P2: Auto names the codec the probe resolved right now; an explicit
/// available item replaces it and Start would send that concrete codec.
#[gpui::test]
async fn auto_names_the_resolved_codec_and_an_explicit_item_is_selectable(cx: &mut TestAppContext) {
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
            panel.test_set_screen_codec_availability(vec![
                (Codec::H264, None),
                (Codec::Vp8, None),
                (Codec::Vp9, Some(VP9_REASON.into())),
                (Codec::Av1, None),
            ]);
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(
        visual
            .debug_bounds("share-codec-label:Автоматически · H.264")
            .is_some(),
        "Auto names the first available codec in website order"
    );

    tab_to(&mut visual, &voice, "Codec(Explicit(Vp8))");
    visual.simulate_keystrokes("enter");
    draw(&mut visual);
    assert!(
        visual.debug_bounds("share-codec-label:VP8").is_some(),
        "the dialog paints the explicit choice"
    );
    let intent = voice.read_with(&*visual, |panel, _| panel.test_screen_codec_intent());
    assert_eq!(intent, CodecIntent::Explicit(Codec::Vp8));
}

/// P2/C06: the live dialog shows the confirmed codec separately from the
/// desired one, and changing the codec there warns about the restart and goes
/// through the preflight/restart command, not the live bitrate path.
#[gpui::test]
async fn the_live_dialog_shows_the_confirmed_codec_and_restarts_on_a_change(
    cx: &mut TestAppContext,
) {
    use voxhold_media::publisher::{Phase, Quality, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_set_screen_codec_availability(all_four_available());
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
        });
    });
    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_screen_confirmed_codec(),
            Some(Codec::H264),
            "the fixture confirms the resolved codec"
        );
    });
    assert!(
        visual.debug_bounds("share-codec-confirmed:H.264").is_some(),
        "the live dialog names what is really on air"
    );

    tab_to(&mut visual, &voice, "Codec(Explicit(Vp9))");
    visual.simulate_keystrokes("enter");
    draw(&mut visual);
    assert!(
        visual.debug_bounds("share-codec-label:VP9").is_some(),
        "the desired codec is painted"
    );
    assert!(
        visual.debug_bounds("share-codec-confirmed:H.264").is_some(),
        "the confirmed codec is not replaced by the desired one"
    );
    let warning = voice.read_with(&*visual, |panel, _| panel.test_restart_warning());
    assert!(
        warning.is_some_and(|warning| warning.contains("перезапустит")),
        "a codec change restarts the publication: {warning:?}"
    );

    tab_to(&mut visual, &voice, "Start");
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
        "a UI codec change must go through preflight/restart"
    );
}

/// P2: an explicitly confirmed codec that this build can no longer encode
/// keeps its reason and blocks Apply instead of silently substituting one.
#[gpui::test]
async fn an_explicit_codec_without_an_encoder_blocks_apply(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, PublishRequest, Quality, Sharing};
    use voxhold_media::system_audio::AudioScope;
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker(window, source());
            panel.test_set_screen_codec_availability(vec![
                (Codec::H264, None),
                (Codec::Vp8, None),
                (Codec::Vp9, Some(VP9_REASON.into())),
                (Codec::Av1, None),
            ]);
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(Quality::default()),
                has_audio: false,
                ..Sharing::default()
            });
            // `test_set_screen_sharing` replaces the whole snapshot, so the
            // accepted configuration is recorded after it.
            panel.test_set_screen_confirmed_request(PublishRequest {
                source: source(),
                quality: Quality::default(),
                codec: Codec::Vp9,
                audio: false,
                audio_scope: AudioScope::Device,
            });
            panel.test_open_live_screen_settings(window, cx);
        });
    });
    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());
    draw(&mut visual);
    assert!(
        visual
            .debug_bounds("share-codec-label:Нет доступного SDR-кодека")
            .is_some(),
        "the live codec is refused"
    );
    let reason = Box::leak(format!("share-codec-reason:{VP9_REASON}").into_boxed_str());
    assert!(
        visual.debug_bounds(reason).is_some(),
        "the dialog paints the probe reason"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            !panel.test_can_start(),
            "Apply refuses a codec this build cannot encode"
        );
    });
}
