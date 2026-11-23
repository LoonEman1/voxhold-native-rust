//! Process-wide update controller state: preference errors are cleared by a
//! successful save, and auto-started/cancelled versions stay remembered for
//! the whole process. The fixture and the signed-manifest helper are reused
//! from `update_tests`, so no second `VisualTestContext` setup is needed.

use super::update_tests::{draw, make_app, staged_update, verified_manifest};
use super::*;
use crate::update::{self, StartupScan, UpdatePhase};
use gpui::{TestAppContext, VisualTestContext};
use voxhold_update::download::CancelToken;
use voxhold_update::error::UpdateError;
use voxhold_update::stage::StageOutcome;

#[gpui::test]
async fn a_successful_skip_clears_an_old_preferences_error(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    // A file where the parent directory should be makes the toggle fail.
    let blocker = dir.join("blocked");
    std::fs::write(&blocker, b"file").unwrap();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.prefs_path = Some(blocker.join("update-prefs.json"));
            v.toggle_auto_download(cx);
            assert!(
                v.update.prefs_error.is_some(),
                "the failed toggle records the error"
            );
        });
        let _ = window;
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.prefs_path = Some(dir.join("update-prefs.json"));
            v.update.panel_open = true;
            v.update.phase = UpdatePhase::Available {
                version: "9.9.9".into(),
                notes: String::new(),
            };
            v.skip_version(cx);
        });
        let _ = window;
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(matches!(v.update.phase, UpdatePhase::UpToDate));
        assert!(
            v.update.prefs_error.is_none(),
            "a successful save clears the old error"
        );
    });
    assert!(visual.debug_bounds("update-prefs-error").is_none());
}

/// Run one signed-manifest check and record the automatic start decision.
fn check(view: &Entity<Voxhold>, cx: &TestAppContext, version: &'static str) {
    let manifest = verified_manifest(version);
    cx.update(|app| {
        view.update(app, |v, cx| {
            v.update.prefs.auto_download = true;
            v.update.download_task_enabled = false;
            v.update.phase = UpdatePhase::Checking;
            let generation = v.update.generation;
            v.finish_check(generation, Ok(manifest), cx);
        });
    });
}

#[gpui::test]
async fn a_verified_startup_candidate_is_the_only_automatic_install(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view, dir) = make_app(cx);
    let mut staged = staged_update(&dir);
    view.update(cx, |v, _| {
        v.update.prefs.install_on_next_start = true;
        v.update.prefs.install_refused_version = None;
        v.update.single_instance = true;
        v.update.phase = UpdatePhase::Idle;
    });
    let auto = view.update(cx, |v, cx| {
        v.apply_startup_scan_auto(
            StartupScan {
                root: None,
                ready: Some(staged.clone()),
                error: None,
                warning: None,
            },
            true,
            cx,
        )
    });
    assert!(auto, "a verified package with the switch on installs");
    assert!(matches!(
        view.read_with(cx, |v, _| v.update.phase.clone()),
        UpdatePhase::Ready { .. }
    ));

    // Another copy of the same installation owns the update mutex.
    view.update(cx, |v, _| {
        v.update.phase = UpdatePhase::Idle;
        v.update.single_instance = false;
    });
    let auto = view.update(cx, |v, cx| {
        v.apply_startup_scan_auto(
            StartupScan {
                root: None,
                ready: Some(staged.clone()),
                error: None,
                warning: None,
            },
            true,
            cx,
        )
    });
    assert!(
        !auto,
        "a second process leaves the package for the next start"
    );

    // A marker without signed proof is never installed.
    staged.verified = false;
    view.update(cx, |v, _| {
        v.update.phase = UpdatePhase::Idle;
        v.update.single_instance = true;
    });
    let auto = view.update(cx, |v, cx| {
        v.apply_startup_scan_auto(
            StartupScan {
                root: None,
                ready: Some(staged),
                error: None,
                warning: None,
            },
            true,
            cx,
        )
    });
    assert!(!auto, "an unsigned marker is not installable");
}

#[gpui::test]
async fn every_version_auto_starts_at_most_once_per_process(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view, _dir) = make_app(cx);

    // The build version is 0.2.0, so the offered pair is 9.9.9/9.9.10; the
    // sequence stands for 0.2.0 -> 0.3.0 -> 0.2.0.
    check(&view, cx, "9.9.9");
    check(&view, cx, "9.9.10");
    view.read_with(cx, |v, _| {
        assert_eq!(
            v.update.download_starts, 2,
            "each version auto-starts exactly once"
        );
        assert!(v.update.auto_started.contains("9.9.9"));
        assert!(v.update.auto_started.contains("9.9.10"));
    });

    check(&view, cx, "9.9.9");
    view.read_with(cx, |v, _| {
        assert_eq!(
            v.update.download_starts, 2,
            "re-offering an already started version must not download it again"
        );
        assert!(matches!(
            &v.update.phase,
            UpdatePhase::Available { version, .. } if version == "9.9.9"
        ));
    });
}

#[gpui::test]
async fn cancelled_versions_stay_suppressed_for_the_whole_process(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view, _dir) = make_app(cx);
    for version in ["9.9.9", "9.9.10"] {
        cx.update(|app| {
            view.update(app, |v, cx| {
                v.update.cancel = Some(CancelToken::new());
                v.update.phase = UpdatePhase::Downloading {
                    version: version.into(),
                    downloaded: 1,
                    total: 10,
                };
                v.cancel_download(cx);
                let generation = v.update.generation;
                v.finish_download(generation, StageOutcome::Failed(UpdateError::Cancelled), cx);
            });
        });
    }
    view.read_with(cx, |v, _| {
        assert!(v.update.auto_suppressed.contains("9.9.9"));
        assert!(v.update.auto_suppressed.contains("9.9.10"));
        assert!(
            !update::should_auto_download(
                &v.update.prefs,
                "9.9.9",
                v.update.declined.as_ref(),
                &v.update.auto_suppressed,
                &v.update.auto_started,
            ),
            "cancelling a newer version keeps the older one suppressed"
        );
    });

    check(&view, cx, "9.9.9");
    view.read_with(cx, |v, _| {
        assert_eq!(
            v.update.download_starts, 0,
            "a cancelled version is never auto-started again"
        );
        assert!(matches!(
            &v.update.phase,
            UpdatePhase::Available { version, .. } if version == "9.9.9"
        ));
    });
}
