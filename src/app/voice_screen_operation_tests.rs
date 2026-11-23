//! Quality-operation regressions: each Apply carries a tag and the dialog
//! only accepts the result of the operation it is waiting for. A coalesced
//! update can therefore never lose a fast failure or let a late answer of an
//! older operation touch a newer dialog.

use super::voice_screen_restart_tests::{
    cancelled_preflight, failed_preflight, open_quality_dialog, preflighting,
};
use super::voice_screen_tests::{draw, window};
use gpui::TestAppContext;
use voxhold_media::publisher::{
    Codec, Confirmed, Phase, Quality, QualityOutcome, QualityStatus, Sharing,
};
use voxhold_media::system_audio::AudioScope;

/// The worker can report `preflighting` and the failure before the panel reads
/// its coalesced queue; only the failure snapshot is then visible. It still
/// belongs to the Apply that started the preflight and must unblock the dialog.
#[gpui::test]
async fn a_fast_preflight_failure_through_the_update_queue_unblocks_the_dialog(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", op)]);

            // Both samples are queued before the panel reads anything. The
            // real queue keeps only the newest slot, so the GUI first sees the
            // failure without the intermediate `preflighting`.
            panel.test_push_screen_sharing(preflighting(op));
            panel.test_push_screen_sharing(failed_preflight(op, "Новая настройка не открылась."));
            let queued = panel
                .test_poll_screen_sharing()
                .expect("one coalesced screen snapshot");
            assert!(
                matches!(
                    queued.quality_status.as_ref().map(|status| &status.outcome),
                    Some(QualityOutcome::Failed(_))
                ),
                "the coalesced snapshot still names the failed operation: {queued:?}"
            );

            // One poll plus the UI application, exactly as the panel loop does.
            assert!(panel.test_apply_screen_sharing(window, queued));
            assert!(
                !panel.screen_share_busy(),
                "a failure without an observed preflight must still clear busy"
            );
            assert!(panel.screen_share_open(), "the dialog stays open");
            assert!(
                panel.test_screen_error().contains("не открылась"),
                "the failure is visible: {}",
                panel.test_screen_error()
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
            assert!(panel.test_can_start(), "the user may try again");

            // The failed preflight restores the confirmed quality; the user
            // repeats the edit before the retry so it still differs from Live.
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let retry = panel
                .test_pending_quality_op()
                .expect("the retry carries an operation id");
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("restart", retry)],
                "the retry sends a new restart command"
            );
            assert_ne!(
                Some(retry),
                Some(op),
                "a retry is a new operation, not the failed one"
            );
        });
    });
}

/// A late failure of operation N must not clear the busy state of the newer
/// operation N+1, change its reason, close its dialog or end the stream.
#[gpui::test]
async fn a_late_failure_of_an_earlier_operation_does_not_touch_the_new_dialog(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let first_op = panel
                .test_pending_quality_op()
                .expect("the first restart has an operation id");
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("restart", first_op)]
            );
            assert!(panel.test_apply_screen_sharing(
                window,
                failed_preflight(first_op, "Первая проверка не открылась.",)
            ));
            assert!(!panel.screen_share_busy());

            // The user corrects the settings and applies again.
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let second_op = panel
                .test_pending_quality_op()
                .expect("the second restart has an operation id");
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("restart", second_op)]
            );
            assert_ne!(second_op, first_op);

            // The old failure arrives after the new Apply. It belongs to the
            // old operation and must not touch this dialog or the stream.
            assert!(panel.test_apply_screen_sharing(
                window,
                failed_preflight(first_op, "Первая проверка не открылась.",)
            ));
            assert!(panel.screen_share_open(), "the new dialog stays open");
            assert!(
                panel.screen_share_busy(),
                "a stale failure must not clear the new busy state"
            );
            assert_eq!(panel.test_pending_quality_op(), Some(second_op));
            assert!(
                !panel.test_screen_error().contains("Первая"),
                "the old reason must not replace the new dialog: {}",
                panel.test_screen_error()
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a stale failure must not send stream.stop"
            );
        });
    });
}

/// A failure that arrives after the dialog was closed is not an answer to any
/// open dialog: it must not reopen it or change its busy state.
#[gpui::test]
async fn a_late_failure_after_closing_the_dialog_is_ignored(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart has an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", op)]);

            panel.test_close_screen_share(window, cx);
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("cancel-restart", op)],
                "closing before the confirmation cancels the preflight"
            );

            assert!(panel.test_apply_screen_sharing(
                window,
                failed_preflight(op, "Новая настройка не открылась.",)
            ));
            assert!(!panel.screen_share_open(), "the dialog must not reopen");
            assert!(!panel.screen_share_busy(), "busy must not change");
            assert_eq!(panel.test_pending_quality_op(), None);
            assert!(
                panel.test_screen_error().is_empty(),
                "a closed dialog must not adopt the late reason: {}",
                panel.test_screen_error()
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a late failure must not stop the live stream"
            );
        });
    });
}

/// Two sequential operations keep their own results: the first failure cannot
/// overwrite the second, and the second one is still handled.
#[gpui::test]
async fn two_sequential_operations_keep_their_own_results(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let first_op = panel.test_pending_quality_op().expect("first operation");
            assert!(panel.test_apply_screen_sharing(
                window,
                failed_preflight(first_op, "Первая проверка не открылась.",)
            ));

            // The failure restored the confirmed quality: repeat the edit so
            // the second operation is a real restart, not a no-op Apply.
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let second_op = panel.test_pending_quality_op().expect("second operation");
            // The old result reappears before the new one.
            assert!(panel.test_apply_screen_sharing(
                window,
                failed_preflight(first_op, "Первая проверка не открылась.",)
            ));
            assert!(panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), Some(second_op));

            assert!(panel.test_apply_screen_sharing(
                window,
                failed_preflight(second_op, "Вторая проверка не открылась.",)
            ));
            assert!(!panel.screen_share_busy(), "the second result is handled");
            assert_eq!(panel.test_pending_quality_op(), None);
            assert!(panel.screen_share_open());
            assert!(
                panel.test_screen_error().contains("Вторая"),
                "the second reason is visible: {}",
                panel.test_screen_error()
            );
            assert!(panel.test_can_start(), "a third attempt is available");
        });
    });
}

/// A cancelled preflight that carries a reason (a revoked permission or a lost
/// device) must explain it in the still-open dialog. `None` is the user closing
/// the dialog and adds no error; neither case stops the live stream.
#[gpui::test]
async fn a_cancelled_preflight_with_a_reason_is_shown_in_the_dialog(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel.test_pending_quality_op().expect("operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", op)]);
            assert!(panel.test_apply_screen_sharing(window, preflighting(op)));
            assert!(panel.screen_share_open());

            // A revoked permission or a lost device keeps the reason in the
            // snapshot; the open dialog must show it instead of staying mute.
            let reason = "Разрешение на трансляцию отозвано.";
            assert!(panel.test_apply_screen_sharing(window, cancelled_preflight(op, Some(reason))));
            assert!(
                panel.screen_share_open(),
                "the dialog stays open to explain the cancellation"
            );
            assert!(!panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), None);
            assert!(
                panel.test_screen_error().contains("Разрешение"),
                "the cancellation reason is visible: {}",
                panel.test_screen_error()
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a cancelled preflight must not stop the live stream"
            );

            // The user closing the dialog reports no reason: Start cleared the
            // previous answer and the cancelled snapshot adds no new error.
            panel.test_start_screen_share(window, cx);
            let next = panel.test_pending_quality_op().expect("operation id");
            assert!(panel.test_apply_screen_sharing(window, cancelled_preflight(next, None)));
            assert!(panel.screen_share_open());
            assert!(!panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), None);
            assert!(
                panel.test_screen_error().is_empty(),
                "a user close adds no error: {}",
                panel.test_screen_error()
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
        });
    });
}

/// One live-bitrate snapshot with a fully described confirmed attempt.
fn bitrate_sharing(
    live: Quality,
    op: u64,
    outcome: QualityOutcome,
    error: Option<&str>,
) -> Sharing {
    let source = window(11, "Документ");
    Sharing {
        phase: Phase::Live,
        live: true,
        attempt: 1,
        quality: Some(live),
        confirmed: Some(Confirmed {
            attempt: 1,
            source,
            quality: live,
            codec: Codec::Vp8,
            audio: false,
            audio_scope: AudioScope::Device,
        }),
        has_audio: false,
        quality_status: Some(QualityStatus::new(op, outcome)),
        status: error.unwrap_or("В эфире").into(),
        error: error.map(str::to_owned),
        ..Sharing::default()
    }
}

/// `Applying` is not success: the dialog stays open and busy, no stop is sent,
/// and the previous confirmed value remains the on-air one.
#[gpui::test]
async fn a_live_bitrate_stays_applying_until_the_engine_confirms_it(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_change_screen_bitrate(200, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the bitrate change carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("bitrate", op)]);

            let applying = bitrate_sharing(Quality::default(), op, QualityOutcome::Applying, None);
            assert!(panel.test_apply_screen_sharing(window, applying));
            assert!(
                panel.screen_share_open(),
                "the dialog must stay open while the limit is applied"
            );
            assert!(panel.screen_share_busy(), "the Apply stays in flight");
            assert_eq!(panel.test_pending_quality_op(), Some(op));
            assert!(
                panel.test_screen_error().is_empty(),
                "applying is not an error: {}",
                panel.test_screen_error()
            );
            assert_eq!(panel.test_screen_phase(), Phase::Live);
            assert!(
                panel
                    .test_screen_sharing()
                    .confirmed
                    .is_some_and(|confirmed| confirmed.quality.bitrate_kbps == 6000),
                "the confirmed 6000 kbit/s limit is still on air"
            );
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "applying must not stop the live stream"
            );
        });
    });
}

/// Escape/backdrop/Cancel must not silently close the dialog while the live
/// limit is still `Applying`: the user would lose the only visible status and
/// a stop/cancel would not belong to this operation. Once the engine refuses
/// the change, the same close intent works.
#[gpui::test]
async fn an_applying_live_bitrate_cannot_be_closed_until_the_engine_answers(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_change_screen_bitrate(200, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the bitrate change carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("bitrate", op)]);

            let applying = bitrate_sharing(Quality::default(), op, QualityOutcome::Applying, None);
            assert!(panel.test_apply_screen_sharing(window, applying));
            assert!(panel.screen_share_open());
            assert!(panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), Some(op));

            // Escape, the backdrop and Cancel all use this close intent.
            panel.test_close_screen_share(window, cx);
            assert!(
                panel.screen_share_open(),
                "an applying change must not close the dialog"
            );
            assert!(
                panel.screen_share_busy(),
                "the operation is still in flight"
            );
            assert_eq!(panel.test_pending_quality_op(), Some(op));
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "closing while applying must not stop or cancel anything"
            );

            // The engine refused the limit: the dialog stays open with the
            // reason, and the next close intent works again.
            let message = "Медиадвижок не подтвердил новый битрейт.";
            let failed = bitrate_sharing(
                Quality::default(),
                op,
                QualityOutcome::Failed(message.into()),
                Some(message),
            );
            assert!(panel.test_apply_screen_sharing(window, failed));
            assert!(panel.screen_share_open());
            assert!(!panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), None);

            panel.test_close_screen_share(window, cx);
            assert!(
                !panel.screen_share_open(),
                "a refused change may be closed like before"
            );
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "the refused change still must not stop the live stream"
            );
        });
    });
}

/// A verified `Applied` closes the dialog and stops the busy state.
#[gpui::test]
async fn an_applied_live_bitrate_closes_the_dialog(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_change_screen_bitrate(200, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the bitrate change carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("bitrate", op)]);

            let changed = Quality {
                bitrate_kbps: 6200,
                ..Quality::default()
            };
            let applied = bitrate_sharing(changed, op, QualityOutcome::Applied, None);
            assert!(panel.test_apply_screen_sharing(window, applied));
            assert!(
                !panel.screen_share_open(),
                "a verified change closes the dialog"
            );
            assert!(!panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), None);
            assert_eq!(panel.test_screen_phase(), Phase::Live);
            assert!(
                panel
                    .test_screen_sharing()
                    .confirmed
                    .is_some_and(|confirmed| confirmed.quality.bitrate_kbps == 6200),
                "the confirmed configuration follows the verified limit"
            );
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a live change never stops the stream"
            );
        });
    });
}

/// A refused change shows the error, returns the dialog to the confirmed
/// limit, keeps the stream live and allows a retry.
#[gpui::test]
async fn a_refused_live_bitrate_shows_the_error_and_the_confirmed_value(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_change_screen_bitrate(200, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the bitrate change carries an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("bitrate", op)]);

            let message = "Не удалось применить новый битрейт (видео): отказ. Прежние параметры восстановлены.";
            let failed = bitrate_sharing(
                Quality::default(),
                op,
                QualityOutcome::Failed(message.into()),
                Some(message),
            );
            assert!(panel.test_apply_screen_sharing(window, failed));
            assert!(
                panel.screen_share_open(),
                "the failed dialog stays open for a retry"
            );
            assert!(!panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), None);
            assert!(
                panel.test_screen_error().contains("видео"),
                "the refusal is visible: {}",
                panel.test_screen_error()
            );
            assert_eq!(
                panel.test_screen_request().quality.bitrate_kbps,
                6000,
                "the dialog returns to the confirmed limit"
            );
            assert!(panel.test_can_start(), "the user may retry");
            assert_eq!(
                panel.test_screen_sharing().confirmed.map(|confirmed| confirmed.quality),
                Some(Quality::default())
            );
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a refused bitrate must not stop the live stream"
            );

            // The dialog shows the confirmed limit again; the user dials the
            // refused value once more and the retry is a new operation.
            panel.test_change_screen_bitrate(200, cx);
            panel.test_start_screen_share(window, cx);
            let retry = panel
                .test_pending_quality_op()
                .expect("the retry carries an operation id");
            assert_ne!(retry, op, "a retry is a new operation");
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("bitrate", retry)],
                "the retry goes to the live sender again"
            );
        });
    });
}

/// A committed restart closes the dialog and releases busy exactly as before.
#[gpui::test]
async fn a_committed_restart_closes_the_dialog_and_clears_busy(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, window, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(window, cx);
            let op = panel.test_pending_quality_op().expect("operation id");

            let committed = Sharing {
                phase: Phase::Stopping,
                live: false,
                restarting: true,
                quality_status: Some(QualityStatus::new(op, QualityOutcome::Committed)),
                quality: Some(Quality::default()),
                has_audio: false,
                ..Sharing::default()
            };
            assert!(panel.test_apply_screen_sharing(window, committed));
            assert!(!panel.screen_share_open(), "the dialog closes on commit");
            assert!(!panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), None);
        });
    });
}
