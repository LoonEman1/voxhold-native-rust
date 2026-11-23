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
    // The refusal seam and the per-item availability must describe the same
    // build: a build-level «no SDR codec» may not paint every item selectable.
    let unavailable: &'static str = Box::leak(
        format!(
            "share-codec-unavailable:H.264 недоступен: {reason} · VP8 недоступен: {reason} · VP9 недоступен: {reason} · AV1 недоступен: {reason}"
        )
        .into_boxed_str(),
    );
    assert!(
        visual.debug_bounds(unavailable).is_some(),
        "no item stays painted as available when the build has no encoder"
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
                encoder: Default::default(),
            });
            panel.test_open_live_screen_settings(window, cx);
        });
    });
    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());
    draw(&mut visual);
    assert!(
        visual
            .debug_bounds("share-codec-label:VP9 недоступен")
            .is_some(),
        "the live codec is refused by name, not as «no SDR codec»"
    );
    assert!(
        visual
            .debug_bounds("share-codec-label:Нет доступного SDR-кодека")
            .is_none(),
        "other codecs are still encodable, so the build-level line is wrong"
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

/// P2: the live dialog must name the codec the encoder really confirmed, not
/// the configuration the attempt merely accepted. Before the first stats
/// sample only `confirmed` exists; after it, `stats.confirmed_codec` wins.
#[gpui::test]
async fn the_live_dialog_prefers_the_codec_confirmed_by_the_encoder(cx: &mut TestAppContext) {
    use voxhold_media::publisher::engine::PublishStats;
    use voxhold_media::publisher::{Confirmed, Phase, Quality, Sharing};
    use voxhold_media::system_audio::AudioScope;
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker(window, source());
            let quality = Quality::default();
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(quality),
                has_audio: false,
                confirmed: Some(Confirmed {
                    attempt: 1,
                    source: source(),
                    quality,
                    codec: Codec::Vp8,
                    audio: false,
                    audio_scope: AudioScope::EverythingExceptSelf,
                    encoder: Default::default(),
                }),
                stats: Some(PublishStats {
                    confirmed_codec: Some(Codec::Vp9),
                    ..Default::default()
                }),
                ..Sharing::default()
            });
            panel.test_open_live_screen_settings(window, cx);
        });
    });
    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());
    draw(&mut visual);
    assert!(
        visual.debug_bounds("share-codec-confirmed:VP9").is_some(),
        "the dialog must paint the codec the encoder really confirmed"
    );
    assert!(
        visual.debug_bounds("share-codec-confirmed:VP8").is_none(),
        "the accepted but not measured codec must not pretend to be on air"
    );
}

/// P3: an equation of the on-air card line — the measured codec wins over the
/// accepted configuration, so the card never names a codec the encoder did not
/// confirm.
#[gpui::test]
async fn the_on_air_line_prefers_the_codec_confirmed_by_the_encoder(cx: &mut TestAppContext) {
    use voxhold_media::publisher::engine::PublishStats;
    use voxhold_media::publisher::{Confirmed, Phase, Quality, Sharing};
    use voxhold_media::system_audio::AudioScope;
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    let quality = Quality::default();
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(quality),
                has_audio: false,
                confirmed: Some(Confirmed {
                    attempt: 1,
                    source: source(),
                    quality,
                    codec: Codec::Vp8,
                    audio: false,
                    audio_scope: AudioScope::EverythingExceptSelf,
                    encoder: Default::default(),
                }),
                stats: Some(PublishStats {
                    confirmed_codec: Some(Codec::Vp9),
                    ..Default::default()
                }),
                ..Sharing::default()
            });
        });
    });
    draw(&mut visual);
    let line = voice.read_with(&*visual, |panel, _| panel.test_screen_on_air_line());
    assert!(
        line.contains(Codec::Vp9.label()),
        "the card names the measured codec: {line}"
    );
    assert!(
        !line.contains(Codec::Vp8.label()),
        "the card must not name the accepted codec once stats disagree: {line}"
    );
}

/// P3: a picker Start carries the explicit codec the user chose, not Auto's
/// first item. The queued publish command is inspected, not only the dialog.
#[gpui::test]
async fn a_picker_start_sends_the_selected_explicit_codec(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker(window, source());
            panel.test_set_screen_codec_availability(all_four_available());
        });
    });
    draw(&mut visual);
    tab_to(&mut visual, &voice, "Codec(Explicit(Vp9))");
    visual.simulate_keystrokes("enter");
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_codec_intent()),
        CodecIntent::Explicit(Codec::Vp9)
    );
    tab_to(&mut visual, &voice, "Start");
    visual.simulate_keystrokes("enter");
    let requests = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_requests()));
    assert_eq!(
        requests,
        vec![("publish", 7, Some(Codec::Vp9))],
        "a picker Start must publish the codec the dialog shows"
    );
}

/// P3: switching the live dialog from an explicit codec to «Автоматически» when
/// Auto resolves to the same payload is a verified no-op: no preflight, no
/// stop, no start, and the dialog closes on the untouched stream.
#[gpui::test]
async fn applying_auto_for_the_live_codec_sends_no_command(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, PublishRequest, Quality, Sharing};
    use voxhold_media::system_audio::AudioScope;
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker(window, source());
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(Quality::default()),
                has_audio: false,
                ..Sharing::default()
            });
            panel.test_set_screen_confirmed_request(PublishRequest {
                source: source(),
                quality: Quality::default(),
                codec: Codec::H264,
                audio: false,
                audio_scope: AudioScope::EverythingExceptSelf,
                encoder: Default::default(),
            });
            panel.test_open_live_screen_settings(window, cx);
        });
    });
    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_screen_codec_availability(all_four_available());
            cx.notify();
        });
    });
    draw(&mut visual);
    tab_to(&mut visual, &voice, "Codec(Auto)");
    visual.simulate_keystrokes("enter");
    draw(&mut visual);
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_codec_intent()),
        CodecIntent::Auto
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_restart_warning(),
            None,
            "an unchanged live request must not promise a restart"
        );
    });
    tab_to(&mut visual, &voice, "Start");
    visual.simulate_keystrokes("enter");
    let (commands, open) = visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            (panel.test_take_screen_commands(), panel.screen_share_open())
        })
    });
    assert!(
        commands.is_empty(),
        "an unchanged Apply queues no command: {commands:?}"
    );
    assert!(!open, "the dialog closes without touching the stream");
}
