//! Quality-operation regressions: each Apply carries a tag and the dialog
//! only accepts the result of the operation it is waiting for. A coalesced
//! update can therefore never lose a fast failure or let a late answer of an
//! older operation touch a newer dialog.

use super::voice_screen_restart_tests::{failed_preflight, open_quality_dialog, preflighting};
use super::voice_screen_tests::{draw, window};
use gpui::TestAppContext;
use voxhold_media::publisher::{Phase, Quality, QualityOutcome, QualityStatus, Sharing};

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
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart carries an operation id");

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
            assert!(panel.test_apply_screen_sharing(queued));
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

            panel.test_start_screen_share(cx);
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("restart", 0)],
                "the retry sends a new restart command"
            );
            assert_ne!(
                panel.test_pending_quality_op(),
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
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(cx);
            let first_op = panel
                .test_pending_quality_op()
                .expect("the first restart has an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);
            assert!(panel.test_apply_screen_sharing(failed_preflight(
                first_op,
                "Первая проверка не открылась.",
            )));
            assert!(!panel.screen_share_busy());

            // The user corrects the settings and applies again.
            panel.test_start_screen_share(cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);
            let second_op = panel
                .test_pending_quality_op()
                .expect("the second restart has an operation id");
            assert_ne!(second_op, first_op);

            // The old failure arrives after the new Apply. It belongs to the
            // old operation and must not touch this dialog or the stream.
            assert!(panel.test_apply_screen_sharing(failed_preflight(
                first_op,
                "Первая проверка не открылась.",
            )));
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
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(cx);
            let op = panel
                .test_pending_quality_op()
                .expect("the restart has an operation id");
            assert_eq!(panel.test_take_screen_commands(), vec![("restart", 0)]);

            panel.test_close_screen_share(cx);
            assert_eq!(
                panel.test_take_screen_commands(),
                vec![("cancel-restart", 0)],
                "closing before the confirmation cancels the preflight"
            );

            assert!(panel
                .test_apply_screen_sharing(failed_preflight(op, "Новая настройка не открылась.",)));
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
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(cx);
            let first_op = panel.test_pending_quality_op().expect("first operation");
            assert!(panel.test_apply_screen_sharing(failed_preflight(
                first_op,
                "Первая проверка не открылась.",
            )));

            panel.test_start_screen_share(cx);
            let second_op = panel.test_pending_quality_op().expect("second operation");
            // The old result reappears before the new one.
            assert!(panel.test_apply_screen_sharing(failed_preflight(
                first_op,
                "Первая проверка не открылась.",
            )));
            assert!(panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), Some(second_op));

            assert!(panel.test_apply_screen_sharing(failed_preflight(
                second_op,
                "Вторая проверка не открылась.",
            )));
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

/// A committed restart closes the dialog and releases busy exactly as before.
#[gpui::test]
async fn a_committed_restart_closes_the_dialog_and_clears_busy(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let live = window(11, "Документ");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            open_quality_dialog(panel, live.clone(), Quality::default(), false, cx);
            panel.test_set_screen_resolution(voxhold_media::publisher::Resolution::P720, cx);
            panel.test_start_screen_share(cx);
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
            assert!(panel.test_apply_screen_sharing(committed));
            assert!(!panel.screen_share_open(), "the dialog closes on commit");
            assert!(!panel.screen_share_busy());
            assert_eq!(panel.test_pending_quality_op(), None);
        });
    });
}
