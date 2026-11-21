//! Regressions for the confirmed configuration of a live publication.
//!
//! The quality dialog must show what the media layer really accepted, not the
//! request the picker last sent. A refused or cancelled attempt therefore may
//! not replace the live scope seen by the next dialog.

use super::voice_screen_tests::{draw, monitor, window};
use gpui::TestAppContext;
use std::sync::Arc;
use voxhold_media::publisher::{
    Codec, Confirmed, Phase, Preset, Quality, QualityOutcome, QualityStatus, Sharing,
};
use voxhold_media::screen_capture::{ScreenSource, SourceKind};
use voxhold_media::system_audio::{AudioDeviceInfo, AudioScope};

fn device() -> AudioDeviceInfo {
    AudioDeviceInfo {
        id: "test".into(),
        label: "Устройство вывода по умолчанию".into(),
        format: "48000 Гц · 2 канала · 16 бит PCM".into(),
    }
}

/// The configuration the media layer confirms when the attempt turns Live.
fn confirmed(
    attempt: u64,
    source: &ScreenSource,
    quality: Quality,
    audio: bool,
    scope: AudioScope,
) -> Confirmed {
    Confirmed {
        attempt,
        source: source.clone(),
        quality,
        codec: Codec::Vp8,
        audio,
        audio_scope: scope,
    }
}

fn live(_source: &ScreenSource, quality: Quality, audio: bool, confirmed: Confirmed) -> Sharing {
    Sharing {
        phase: Phase::Live,
        live: true,
        attempt: confirmed.attempt,
        quality: Some(quality),
        confirmed: Some(confirmed),
        has_audio: audio,
        status: "В эфире".into(),
        ..Sharing::default()
    }
}

/// Open the live quality dialog on an attempt the media layer already accepted.
fn open_dialog(
    panel: &mut crate::voice_panel::VoicePanel,
    sharing: Sharing,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<crate::voice_panel::VoicePanel>,
) {
    panel.test_attach_detached_handle();
    panel.test_set_audio_probe(Arc::new(|| Ok(device())));
    panel.test_open_screen_picker_with(
        window,
        sharing
            .confirmed
            .as_ref()
            .expect("a confirmed attempt")
            .source
            .kind,
        Vec::new(),
        None,
    );
    panel.test_set_screen_sharing(sharing);
    panel.test_open_live_screen_settings(window, cx);
}

fn preflighting(op: u64, base: &Sharing) -> Sharing {
    Sharing {
        preflighting: true,
        quality_status: Some(QualityStatus::new(op, QualityOutcome::Preflighting)),
        status: "Проверяем новую настройку…".into(),
        ..base.clone()
    }
}

fn failed(op: u64, message: &str, base: &Sharing) -> Sharing {
    Sharing {
        preflighting: false,
        quality_status: Some(QualityStatus::new(
            op,
            QualityOutcome::Failed(message.into()),
        )),
        error: Some(message.into()),
        status: message.into(),
        ..base.clone()
    }
}

fn cancelled(op: u64, base: &Sharing) -> Sharing {
    Sharing {
        preflighting: false,
        quality_status: Some(QualityStatus::new(op, QualityOutcome::Cancelled)),
        error: None,
        status: "В эфире".into(),
        ..base.clone()
    }
}

#[gpui::test]
async fn a_refused_whole_device_attempt_does_not_replace_the_confirmed_scope(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live_source = window(11, "Документ");
    let quality = Quality::default();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            let base = live(
                &live_source,
                quality,
                true,
                confirmed(
                    1,
                    &live_source,
                    quality,
                    true,
                    AudioScope::WindowApplication,
                ),
            );
            open_dialog(panel, base.clone(), window, cx);
            assert_eq!(
                panel.test_audio_scope(),
                AudioScope::WindowApplication,
                "opening the dialog must not widen the confirmed scope"
            );

            // The user explicitly widens the scope; the preflight refuses it.
            panel.test_select_audio_scope(AudioScope::Device, cx);
            assert_eq!(panel.test_audio_scope(), AudioScope::Device);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", op)]);
            assert!(panel.test_apply_screen_sharing(
                window,
                failed(op, "Новая настройка не открылась.", &base,)
            ));
            assert!(!panel.screen_share_busy());
            assert_eq!(
                panel.test_screen_sharing().confirmed,
                Some(base.confirmed.clone().expect("confirmed")),
                "a refused attempt must not touch the confirmed configuration"
            );
            panel.test_close_screen_share(window, cx);

            // Reopening the live dialog must show the confirmed scope again.
            panel.test_open_live_screen_settings(window, cx);
            assert_eq!(
                panel.test_audio_scope(),
                AudioScope::WindowApplication,
                "a refused attempt must not replace the confirmed scope"
            );

            // A bitrate-only change stays live: the refused scope cannot turn
            // it into a restart. The request keeps the confirmed scope too.
            panel.test_change_screen_bitrate(200, cx);
            assert!(
                panel
                    .test_restart_warning()
                    .is_some_and(|warning| warning.contains("без перезапуска")),
                "only the bitrate changed: {:?}",
                panel.test_restart_warning()
            );

            // A frame-rate change needs a restart for the frame rate only; it
            // must carry the confirmed narrow scope, never the refused one.
            panel.test_apply_preset(Preset::Games, cx);
            assert!(
                panel
                    .test_restart_warning()
                    .is_some_and(|warning| warning.contains("перезапустит")),
                "the frame rate change restarts the capture"
            );
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", op)]);
            let request = panel.test_screen_request();
            assert_eq!(request.source, live_source);
            assert_eq!(
                request.audio_scope,
                AudioScope::WindowApplication,
                "a frame-rate-only change must not widen the capture"
            );
            assert!(request.audio, "the confirmed audio flag is preserved");
            assert_eq!(request.quality.fps, 60);
            assert_eq!(request.quality.bitrate_kbps, 12_000);
        });
    });
}

#[gpui::test]
async fn a_cancelled_attempt_keeps_the_confirmed_scope(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live_source = window(11, "Документ");
    let quality = Quality::default();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            let base = live(
                &live_source,
                quality,
                true,
                confirmed(
                    1,
                    &live_source,
                    quality,
                    true,
                    AudioScope::WindowApplication,
                ),
            );
            open_dialog(panel, base.clone(), window, cx);
            panel.test_select_audio_scope(AudioScope::Device, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", op)]);
            assert!(panel.test_apply_screen_sharing(window, preflighting(op, &base)));

            // Closing before the commit cancels the preflight; the live
            // attempt and its confirmed configuration stay untouched.
            panel.test_close_screen_share(window, cx);
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("cancel-restart", op)]
            );
            assert!(panel.test_apply_screen_sharing(window, cancelled(op, &base)));
            assert_eq!(
                panel.test_screen_sharing().confirmed,
                Some(base.confirmed.clone().expect("confirmed"))
            );

            panel.test_open_live_screen_settings(window, cx);
            assert_eq!(
                panel.test_audio_scope(),
                AudioScope::WindowApplication,
                "a cancelled attempt must not replace the confirmed scope"
            );
        });
    });
}

#[gpui::test]
async fn a_confirmed_whole_device_change_is_shown_on_the_next_open(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live_source = window(11, "Документ");
    let quality = Quality::default();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            let base = live(
                &live_source,
                quality,
                true,
                confirmed(
                    1,
                    &live_source,
                    quality,
                    true,
                    AudioScope::WindowApplication,
                ),
            );
            open_dialog(panel, base.clone(), window, cx);

            // The user widens the scope and the restart is really committed.
            panel.test_select_audio_scope(AudioScope::Device, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", op)]);
            let committed = Sharing {
                phase: Phase::Stopping,
                live: false,
                restarting: true,
                quality_status: Some(QualityStatus::new(op, QualityOutcome::Committed)),
                ..base.clone()
            };
            assert!(panel.test_apply_screen_sharing(window, committed));
            assert!(!panel.screen_share_open(), "the committed dialog closes");

            // The new attempt turns Live with the accepted whole-device scope.
            let accepted = live(
                &live_source,
                quality,
                true,
                confirmed(2, &live_source, quality, true, AudioScope::Device),
            );
            assert!(panel.test_apply_screen_sharing(window, accepted));
            panel.test_open_live_screen_settings(window, cx);
            assert_eq!(
                panel.test_audio_scope(),
                AudioScope::Device,
                "a confirmed whole-device change is shown as applied"
            );
            assert_eq!(
                panel.test_screen_request().audio_scope,
                AudioScope::Device,
                "the next restart starts from the confirmed scope"
            );
        });
    });
}

#[gpui::test]
async fn a_rollback_shows_the_restored_configuration(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live_source = window(11, "Документ");
    let quality = Quality::default();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            let base = live(
                &live_source,
                quality,
                true,
                confirmed(
                    1,
                    &live_source,
                    quality,
                    true,
                    AudioScope::WindowApplication,
                ),
            );
            open_dialog(panel, base.clone(), window, cx);
            panel.test_select_audio_scope(AudioScope::Device, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", op)]);
            let committed = Sharing {
                phase: Phase::Stopping,
                live: false,
                restarting: true,
                quality_status: Some(QualityStatus::new(op, QualityOutcome::Committed)),
                ..base.clone()
            };
            assert!(panel.test_apply_screen_sharing(window, committed));

            // The new capture failed locally and the media layer restored the
            // previous request on a new attempt.
            let restored = live(
                &live_source,
                quality,
                true,
                confirmed(
                    3,
                    &live_source,
                    quality,
                    true,
                    AudioScope::WindowApplication,
                ),
            );
            assert!(panel.test_apply_screen_sharing(window, restored));
            panel.test_open_live_screen_settings(window, cx);
            assert_eq!(
                panel.test_audio_scope(),
                AudioScope::WindowApplication,
                "the rollback restores the confirmed scope"
            );
            assert_eq!(
                panel.test_screen_request().audio_scope,
                AudioScope::WindowApplication,
                "the next restart starts from the restored request"
            );
        });
    });
}

#[gpui::test]
async fn a_narrow_monitor_scope_is_not_widened_by_opening_or_a_preset(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live_source = monitor(3, "Экран A", 1920, 1080);
    let quality = Quality::default();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            let base = live(
                &live_source,
                quality,
                true,
                confirmed(
                    1,
                    &live_source,
                    quality,
                    true,
                    AudioScope::EverythingExceptSelf,
                ),
            );
            open_dialog(panel, base, window, cx);
            assert_eq!(
                panel.test_audio_scope(),
                AudioScope::EverythingExceptSelf,
                "opening keeps the monitor's narrow scope"
            );
            panel.test_apply_preset(Preset::Games, cx);
            assert_eq!(panel.test_screen_request().source.kind, SourceKind::Monitor);
            assert_eq!(
                panel.test_screen_request().audio_scope,
                AudioScope::EverythingExceptSelf,
                "a preset must not widen the monitor scope"
            );
        });
    });
}

#[gpui::test]
async fn an_ordinary_picker_never_sends_a_stale_confirmed_source(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live_source = window(11, "Документ");
    let picked = monitor(3, "Экран A", 1920, 1080);
    let quality = Quality::default();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            // A previous publication left its confirmed configuration behind
            // while a new picker is open. The picker sends the row it selects,
            // never that leftover live source.
            panel.test_open_screen_picker(window, picked.clone());
            panel.test_set_screen_confirmed_source(live_source.clone());
            panel.test_select_screen_source(picked.clone(), cx);
            assert_eq!(
                panel.test_screen_request().source,
                picked,
                "an ordinary picker must not publish the stale confirmed source"
            );
            panel.test_close_screen_share(window, cx);

            // The restart dialog is the one place bound to that configuration.
            let base = live(
                &live_source,
                quality,
                false,
                confirmed(
                    1,
                    &live_source,
                    quality,
                    false,
                    AudioScope::WindowApplication,
                ),
            );
            panel.test_set_screen_sharing(base);
            panel.test_open_live_screen_settings(window, cx);
            assert_eq!(
                panel.test_screen_request().source,
                live_source,
                "the restart dialog keeps the confirmed live source"
            );
        });
    });
}
