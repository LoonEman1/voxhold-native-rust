//! Restart-dialog regressions: the live quality dialog is bound to the source
//! the capture engine already accepted, while a normal Start still needs a row
//! in the current list. The confirmed identity lives in `last_request` only;
//! the enumeration caches never contain or inject it.

use super::voice_screen_tests::{draw, focused, monitor, wait_for, window};
use gpui::TestAppContext;
use std::sync::Arc;
use std::time::Duration;
use voxhold_media::publisher::{Codec, Phase, PublishRequest, Quality, Sharing};
use voxhold_media::screen_capture::SourceKind;
use voxhold_media::system_audio::AudioDeviceInfo;
use voxhold_media::system_audio::AudioScope;

fn device() -> AudioDeviceInfo {
    AudioDeviceInfo {
        id: "test".into(),
        label: "Устройство вывода по умолчанию".into(),
        format: "48000 Гц · 2 канала · 16 бит PCM".into(),
    }
}

fn live_sharing() -> Sharing {
    Sharing {
        phase: Phase::Live,
        live: true,
        quality: Some(Quality::default()),
        has_audio: true,
        ..Sharing::default()
    }
}

/// Bound restart state on top of a controlled picker snapshot.
fn open_restart(
    panel: &mut crate::voice_panel::VoicePanel,
    snapshot: Vec<voxhold_media::screen_capture::ScreenSource>,
    live: voxhold_media::screen_capture::ScreenSource,
    cx: &mut gpui::Context<crate::voice_panel::VoicePanel>,
) {
    panel.test_attach_detached_handle();
    panel.test_open_screen_picker_with(SourceKind::Window, snapshot, None);
    panel.test_set_screen_sharing(live_sharing());
    panel.test_set_screen_last_request(live);
    panel.test_open_live_screen_settings(cx);
}

#[gpui::test]
async fn a_live_window_restart_binds_to_the_confirmed_source_with_an_empty_list(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(SourceKind::Window, Vec::new(), None);
            // The live dialog repeats the endpoint probe; Start must wait.
            panel.test_set_screen_audio_pending(Arc::new(|| {
                std::thread::sleep(Duration::from_millis(150));
                Ok(device())
            }));
            panel.test_set_screen_sharing(live_sharing());
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
                panel.test_screen_source_count(SourceKind::Window),
                0,
                "the confirmed source is never injected into the enumeration cache"
            );
            assert_eq!(
                panel.test_selected_source(),
                None,
                "there is no cached row, only the confirmed identity"
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
async fn restart_rows_are_inert_for_mouse_and_keyboard(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    let other = window(12, "Другой документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            // Both rows are cached, so a normal picker could select either.
            open_restart(panel, vec![live.clone(), other.clone()], live.clone(), cx);

            assert_eq!(
                panel.test_selected_source_id(),
                Some(live.source_id()),
                "the confirmed live identity wins over the cached rows"
            );
            // A click on the other row must not move the restart.
            panel.test_try_select_screen_source(other.clone(), cx);
            assert_eq!(
                panel.test_selected_source_id(),
                Some(live.source_id()),
                "a click on a cached row is rejected in restart mode"
            );
            assert_eq!(
                panel.test_screen_source_count(SourceKind::Window),
                2,
                "rejecting the row leaves the cache untouched"
            );
        });
    });

    // Tab through a full ring: no source row may become focusable, so Enter
    // can never route to `select_screen_source` either.
    for _ in 0..40 {
        visual.simulate_keystrokes("tab");
        if let Some(name) = focused(&mut visual, &voice) {
            assert!(
                !name.starts_with("Source("),
                "a restart source row must stay out of the focus ring: {name}"
            );
        }
    }
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
    assert_eq!(commands, vec![("restart", 0)]);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_selected_source_id(),
            Some(live.source_id()),
            "the restart still targets the live source after keyboard traversal"
        );
        assert_eq!(panel.test_screen_request().source, live);
    });
}

#[gpui::test]
async fn a_restart_ignores_the_category_and_refresh_with_a_stale_snapshot(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            // The snapshot is empty from the start: the engine accepted the
            // window, but the enumeration does not describe it any more.
            open_restart(panel, Vec::new(), live.clone(), cx);
            assert!(panel.test_can_start(), "the confirmed restart may start");

            let before = panel.test_source_generations();
            panel.test_switch_share_kind(SourceKind::Monitor, cx);
            assert_eq!(
                panel.test_share_kind(),
                SourceKind::Window,
                "a restart is source-locked, so the category cannot change"
            );
            panel.test_refresh_screen_sources(cx);
            assert_eq!(
                panel.test_source_generations(),
                before,
                "refresh is inert while the restart is bound"
            );

            // A stale/empty answer must not clear the confirmed identity or
            // disable Start.
            panel.test_apply_screen_sources(SourceKind::Window, Vec::new());
            assert_eq!(panel.test_selected_source_id(), Some(live.source_id()));
            assert!(panel.test_can_start());
            assert!(
                panel.test_screen_error().is_empty(),
                "a stale answer must not claim the live source is gone"
            );

            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(commands, vec![("restart", 0)]);
            assert_eq!(
                panel.test_screen_request().source,
                live,
                "the restart uses the confirmed source, not the snapshot"
            );
        });
    });
}

#[gpui::test]
async fn a_restart_keeps_the_confirmed_identity_when_the_row_was_renamed(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    let renamed = window(11, "Документ · обновлён");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            // A refresh already described the live window with a newer title;
            // the restart row and request stay bound to the accepted identity.
            open_restart(panel, vec![renamed.clone()], live.clone(), cx);
            assert_eq!(panel.test_selected_source_id(), Some(live.source_id()));
            assert_eq!(
                panel.test_selected_source(),
                Some(renamed),
                "the cached row keeps its newer title"
            );
            assert!(panel.test_can_start());

            // An answer that lost the live window must not unbind the restart:
            // the capture engine accepted the source and only it can report
            // the failure.
            panel.test_apply_screen_sources(SourceKind::Window, Vec::new());
            assert_eq!(panel.test_selected_source_id(), Some(live.source_id()));
            assert!(
                panel.test_can_start(),
                "the confirmed live source stays startable"
            );
            assert!(
                panel.test_screen_error().is_empty(),
                "a stale refresh must not claim the live source is gone"
            );

            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(commands, vec![("restart", 0)]);
            assert_eq!(
                panel.test_screen_request().source,
                live,
                "the restart request is built from the accepted source"
            );
        });
    });
}

#[gpui::test]
async fn a_same_phase_live_stats_update_keeps_an_uncommitted_restart_dialog_open(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_restart(panel, vec![live.clone()], live.clone(), cx);
            assert!(panel.screen_share_open());
            assert!(panel.test_screen_restart());
            assert!(panel.test_can_start());

            // The publisher emits fresh stats every second while Live. That
            // update is not an answer to Start: it must not close an
            // uncommitted restart dialog or unbind its Start.
            let sampled = Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(Quality::default()),
                has_audio: true,
                stats: Some(voxhold_media::publisher::engine::PublishStats {
                    audio_active: true,
                    audio_kbps: 96.0,
                    ..Default::default()
                }),
                ..Sharing::default()
            };
            assert!(panel.test_apply_screen_sharing(sampled));
            assert!(
                panel.screen_share_open(),
                "a same-phase stats update must leave the dialog open"
            );
            assert!(
                panel.test_screen_restart(),
                "the restart stays bound to the live source"
            );
            assert!(panel.test_can_start(), "Start must stay reachable");

            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(
                commands,
                vec![("restart", 0)],
                "Start queues exactly one restart: {commands:?}"
            );
            assert!(panel.screen_share_busy(), "the restart is committed");

            // Once committed, the accepted live update closes the dialog as
            // before and the room card takes over the status.
            let accepted = Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(Quality::default()),
                has_audio: true,
                stats: Some(voxhold_media::publisher::engine::PublishStats {
                    audio_active: true,
                    audio_kbps: 97.0,
                    ..Default::default()
                }),
                ..Sharing::default()
            };
            panel.test_apply_screen_sharing(accepted);
            assert!(
                !panel.screen_share_open(),
                "the committed dialog closes on the accepted live update"
            );
            assert!(!panel.screen_share_busy());
        });
    });
}

#[gpui::test]
async fn a_normal_picker_after_a_restart_starts_with_no_rows(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_restart(panel, vec![live.clone()], live.clone(), cx);
            assert_eq!(panel.test_selected_source_id(), Some(live.source_id()));

            // Closing the restart dialog leaves the live stream alone.
            panel.test_close_screen_share(cx);
            assert!(!panel.screen_share_open());
            assert_eq!(panel.test_screen_phase(), Phase::Live);
            assert!(panel.test_take_screen_commands().is_empty());

            // A fresh picker must not inherit the confirmed row before the
            // new enumeration arrives.
            panel.test_set_share_gate(Ok(()));
            panel.test_set_audio_probe(Arc::new(|| Ok(device())));
            panel.test_open_screen_share(cx);
            assert!(panel.screen_share_open());
            assert!(!panel.test_screen_restart());
            assert_eq!(panel.test_selected_source_id(), None);
            assert_eq!(panel.test_screen_source_count(SourceKind::Monitor), 0);
            assert_eq!(panel.test_screen_source_count(SourceKind::Window), 0);
            assert_eq!(panel.test_selected_source(), None);

            // The old confirmed id is not selectable until it is part of a
            // fresh answer. A normal picker may switch category again.
            panel.test_switch_share_kind(SourceKind::Window, cx);
            panel.test_try_select_screen_source(live.clone(), cx);
            assert_eq!(panel.test_selected_source_id(), None);

            panel.test_apply_screen_sources(SourceKind::Window, vec![live.clone()]);
            panel.test_try_select_screen_source(live.clone(), cx);
            assert_eq!(panel.test_selected_source_id(), Some(live.source_id()));
        });
    });
}

fn sharing_with(quality: Quality, has_audio: bool) -> Sharing {
    Sharing {
        phase: Phase::Live,
        live: true,
        quality: Some(quality),
        has_audio,
        status: "В эфире".into(),
        ..Sharing::default()
    }
}

/// The worker reporting that the restart preflight of this attempt runs. The
/// live stream keeps running and nothing was stopped yet.
fn preflighting() -> Sharing {
    Sharing {
        preflighting: true,
        status: "Проверяем новую настройку…".into(),
        ..sharing_with(Quality::default(), false)
    }
}

/// Open the live quality dialog with a fully described accepted request, so
/// the dialog's classification sees the same audio and scope the engine
/// confirmed.
fn open_quality_dialog(
    panel: &mut crate::voice_panel::VoicePanel,
    live_source: voxhold_media::screen_capture::ScreenSource,
    quality: Quality,
    audio: bool,
    cx: &mut gpui::Context<crate::voice_panel::VoicePanel>,
) {
    panel.test_attach_detached_handle();
    panel.test_set_audio_probe(Arc::new(|| Ok(device())));
    panel.test_open_screen_picker_with(live_source.kind, Vec::new(), None);
    panel.test_set_screen_sharing(sharing_with(quality, audio));
    panel.test_set_screen_live_request(PublishRequest {
        source: live_source,
        quality,
        codec: Codec::Vp8,
        audio,
        audio_scope: AudioScope::Device,
    });
    panel.test_open_live_screen_settings(cx);
}

#[gpui::test]
async fn a_bitrate_only_change_applies_live_without_a_restart(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            assert!(panel.screen_share_open());
            assert!(panel.test_screen_restart());
            assert!(panel.test_can_start());

            panel.test_change_screen_bitrate(200, cx);
            assert!(
                panel
                    .test_restart_warning()
                    .is_some_and(|warning| warning.contains("без перезапуска")),
                "the dialog must say that only the bitrate is live: {:?}",
                panel.test_restart_warning()
            );
            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(
                commands,
                vec![("bitrate", 0)],
                "a bitrate-only change must not queue a restart: {commands:?}"
            );
            let request = panel.test_screen_request();
            assert_eq!(request.quality.bitrate_kbps, 6200);
            assert_eq!(request.source, live);
            assert_eq!(
                panel.test_screen_phase(),
                Phase::Live,
                "the phase stays Live while the limit is applied"
            );
            assert!(panel.screen_share_busy());
        });
    });
}

#[gpui::test]
async fn a_resolution_change_keeps_the_confirmed_restart(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            assert!(
                panel
                    .test_restart_warning()
                    .is_some_and(|warning| warning.contains("перезапустит")),
                "a resolution change must warn about the restart: {:?}",
                panel.test_restart_warning()
            );
            assert!(panel.test_can_start());

            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(
                commands,
                vec![("restart", 0)],
                "a resolution change still needs the confirmed stop+start: {commands:?}"
            );
        });
    });
}

#[gpui::test]
async fn closing_the_dialog_during_a_preflight_cancels_it_and_keeps_live(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);

            // The worker reports the running preflight without stopping the
            // live stream. The dialog must stay open so closing it cancels.
            let preflighting = Sharing {
                preflighting: true,
                status: "Проверяем новую настройку…".into(),
                ..sharing_with(Quality::default(), false)
            };
            assert!(panel.test_apply_screen_sharing(preflighting));
            assert!(
                panel.screen_share_open(),
                "a running preflight must keep the dialog open"
            );
            assert!(panel.screen_share_busy());
            assert_eq!(panel.test_screen_phase(), Phase::Live);

            panel.test_close_screen_share(cx);
            assert!(!panel.screen_share_open());
            let commands = panel.test_take_screen_commands();
            assert_eq!(
                commands,
                vec![("cancel-restart", 0)],
                "closing cancels the preflight, not the live stream: {commands:?}"
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
        });
    });
}

#[gpui::test]
async fn a_failed_preflight_keeps_live_and_shows_the_error(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);

            // The worker confirms the running preflight before it can fail it.
            assert!(panel.test_apply_screen_sharing(preflighting()));
            let failed = Sharing {
                preflighting: false,
                error: Some("Новая настройка не открылась.".into()),
                status: "Новая настройка не открылась.".into(),
                ..sharing_with(Quality::default(), false)
            };
            assert!(panel.test_apply_screen_sharing(failed));
            assert!(
                panel.screen_share_open(),
                "a failed preflight keeps the dialog open for a correction"
            );
            assert!(!panel.screen_share_busy());
            assert!(
                panel.test_screen_error().contains("не открылась"),
                "the preflight reason is visible: {}",
                panel.test_screen_error()
            );
            assert_eq!(
                panel.test_screen_phase(),
                Phase::Live,
                "the live attempt is untouched"
            );
            assert!(
                panel.test_can_start(),
                "the user may correct the settings and try again"
            );
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a failed preflight must not send stream.stop"
            );
        });
    });
}

/// After an earlier preflight failure, the dialog's next Apply must keep its
/// cancel intent. A stale stats sample of the previous answer can clear `busy`,
/// and the worker's `preflighting` update may land only afterwards; closing the
/// dialog must still cancel the preflight and leave the live stream untouched.
#[gpui::test]
async fn a_preflight_cancel_survives_a_stale_failure_state_and_keeps_live(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);

            // The first Apply fails: the dialog stays open with the reason.
            panel.test_start_screen_share(cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);
            assert!(panel.test_apply_screen_sharing(preflighting()));
            let failed = Sharing {
                preflighting: false,
                error: Some("Новая настройка не открылась.".into()),
                status: "Новая настройка не открылась.".into(),
                ..sharing_with(Quality::default(), false)
            };
            assert!(panel.test_apply_screen_sharing(failed.clone()));
            assert!(!panel.screen_share_busy());
            assert!(panel.screen_share_open());
            assert!(panel.test_screen_error().contains("не открылась"));

            // Apply again. Start must drop the previous attempt's reason.
            panel.test_start_screen_share(cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);
            assert!(panel.screen_share_busy());
            assert!(
                panel.test_screen_sharing().error.is_none(),
                "Start clears the stale sharing error: {:?}",
                panel.test_screen_sharing().error
            );
            assert!(!panel.test_screen_error().contains("не открылась"));

            // A stale failure sample of the *previous* answer lands after the
            // click. It is not the new answer: it must not stop, close, show
            // the old reason or end the pending attempt.
            assert!(panel.test_apply_screen_sharing(failed));
            assert!(
                panel.screen_share_open(),
                "a stale state must not close the dialog"
            );
            assert!(
                panel.screen_share_busy(),
                "a stale failure sample must not end the attempt"
            );
            assert!(
                !panel.test_screen_error().contains("не открылась"),
                "a stale failure sample must not show the old reason: {}",
                panel.test_screen_error()
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a stale state must not send stream.stop"
            );

            // The worker's preflight update lands afterwards. The dialog keeps
            // its cancel intent, and Start is refused while it runs.
            assert!(panel.test_apply_screen_sharing(preflighting()));
            assert!(panel.screen_share_open());
            assert!(panel.test_screen_sharing().preflighting);
            assert!(
                !panel.test_can_start(),
                "Start cannot run a second preflight"
            );

            // Closing cancels the pending preflight; Live is untouched.
            panel.test_close_screen_share(cx);
            assert!(!panel.screen_share_open());
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("cancel-restart", 0)],
                "closing during a preflight must cancel it"
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
        });
    });
}

/// The deterministic window of the second Apply: the stale failure sample of
/// the first preflight is the last update before the worker's confirmation.
/// It must not end the pending attempt, and closing in that gap must still
/// cancel the preflight instead of letting it restart the closed stream.
#[gpui::test]
async fn a_preflight_cancel_survives_a_stale_failure_state_before_the_confirmation(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);

            // The first preflight is confirmed by the worker and then fails,
            // so the next Start sees the same stale answer as the field case.
            panel.test_start_screen_share(cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);
            assert!(panel.test_apply_screen_sharing(preflighting()));
            let failed = Sharing {
                preflighting: false,
                error: Some("Новая настройка не открылась.".into()),
                status: "Новая настройка не открылась.".into(),
                ..sharing_with(Quality::default(), false)
            };
            assert!(panel.test_apply_screen_sharing(failed.clone()));
            assert!(!panel.screen_share_busy());
            assert!(panel.test_screen_error().contains("не открылась"));

            // Apply again: this attempt is unconfirmed until the worker says
            // `preflighting`, and Start drops the previous attempt's reason.
            panel.test_start_screen_share(cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);
            assert!(panel.screen_share_busy());

            // The stale failed sample arrives before the confirmation. It must
            // neither end the attempt nor re-show the old reason.
            assert!(panel.test_apply_screen_sharing(failed));
            assert!(panel.screen_share_open());
            assert!(
                panel.screen_share_busy(),
                "the stale sample must not clear the pending attempt"
            );
            assert!(
                panel.test_screen_error().is_empty(),
                "the stale sample must not re-show the old reason: {}",
                panel.test_screen_error()
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);

            // Closing before `preflighting` lands cancels the preflight the
            // worker is about to run and leaves Live untouched.
            panel.test_close_screen_share(cx);
            assert!(!panel.screen_share_open());
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("cancel-restart", 0)],
                "closing before the confirmation must still cancel the preflight"
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
        });
    });
}
