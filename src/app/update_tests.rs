//! Update integration tests: state mapping, popup layout, cancellation and the
//! helper launch seam. No test performs a real network request.

use super::update_ops::{LaunchRequest, UpdateLauncher};
use super::*;
use crate::update::{self, RetryKind, UpdatePhase};
use gpui::{px, size, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use voxhold_update::download::CancelToken;
use voxhold_update::error::UpdateError;
use voxhold_update::manifest::{FullPackage, Manifest, SetupPackage, VerifiedManifest};
use voxhold_update::stage::{StageMethod, StageOutcome, StagedUpdate};

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

fn center(visual: &mut VisualTestContext, id: &'static str) -> gpui::Point<gpui::Pixels> {
    let bounds = visual
        .debug_bounds(id)
        .unwrap_or_else(|| panic!("{id} is painted"));
    gpui::point(
        bounds.origin.x + bounds.size.width / 2.,
        bounds.origin.y + bounds.size.height / 2.,
    )
}

fn click(visual: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>) {
    visual.simulate_mouse_move(position, None, Modifiers::default());
    visual.simulate_click(position, Modifiers::default());
    draw(visual);
}

fn make_app(cx: &mut TestAppContext) -> (gpui::WindowHandle<Root>, Entity<Voxhold>, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-update-ui-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let shell = dir.join("workspace-layout.json");
    let instance = dir.join("instance.json");
    let sessions = dir.join("sessions.bin");
    let install = dir.join("install");
    std::fs::create_dir_all(&install).unwrap();
    let slot: Rc<RefCell<Option<Entity<Voxhold>>>> = Default::default();
    let captured = slot.clone();
    let app_dir = dir.clone();
    let window = cx.add_window(move |window, cx| {
        let view = cx.new(|cx| {
            let mut view =
                Voxhold::new(window, cx, "", Some(shell), Some(instance), Some(sessions));
            view.client = Some(voxhold_api::Client::new("http://127.0.0.1:1").unwrap());
            view.self_user_id = 1;
            view.update.root_override = Some(app_dir.join("updates"));
            view.update.prefs_path = Some(app_dir.join("update-prefs.json"));
            view.update.prefs = update::UpdatePrefs::default();
            view.update.declined = None;
            view.update.install_dir = app_dir.join("install");
            view.update.exe_name = "Voxhold.exe".into();
            view
        });
        *captured.borrow_mut() = Some(view.clone());
        Root::new(view.into(), window, cx)
    });
    let view = slot.borrow().clone().expect("view");
    (window, view, dir)
}

fn staged_update(dir: &Path) -> StagedUpdate {
    staged_update_with_exe(dir, Some("Voxhold.exe"))
}

fn staged_update_with_exe(dir: &Path, exe: Option<&str>) -> StagedUpdate {
    let staging = dir.join("updates").join("9.9.9");
    let app = staging.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("voxhold-updater.exe"), b"helper").unwrap();
    if let Some(exe) = exe {
        std::fs::write(app.join(exe), b"app").unwrap();
    }
    StagedUpdate {
        version: semver::Version::new(9, 9, 9),
        method: StageMethod::Full,
        staging_dir: staging,
        app_dir: app,
        package_sha256: "a".repeat(64),
    }
}

fn long_notes() -> String {
    (1..=40)
        .map(|line| format!("Строка {line}: исправления, улучшения и подробности выпуска."))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A manifest signed with a throw-away Ed25519 key. No network is involved:
/// the test only needs the real `VerifiedManifest` value.
fn verified_manifest(version: &str) -> VerifiedManifest {
    let (private, public) = voxhold_update::verify::generate_keypair().unwrap();
    let manifest = Manifest {
        schema: 1,
        channel: update::CHANNEL.into(),
        version: version.into(),
        min_windows: "10.0.19041".into(),
        published_at: "2026-11-19T12:00:00Z".into(),
        notes: "Заметки".into(),
        authenticode: false,
        full: FullPackage {
            url: "https://example.invalid/full.zip".into(),
            size: 10,
            sha256: "a".repeat(64),
            package_sha256: "a".repeat(64),
        },
        setup: SetupPackage {
            url: "https://example.invalid/Setup.exe".into(),
            size: 10,
            sha256: "b".repeat(64),
        },
        deltas: Vec::new(),
    };
    let bytes = manifest.to_json();
    let signature = voxhold_update::verify::sign_bytes(&private, &bytes).unwrap();
    let key = voxhold_update::verify::UpdatePublicKey::from_base64(&public).unwrap();
    voxhold_update::verify::verify_and_parse(&key, &bytes, &signature).unwrap()
}

#[gpui::test]
async fn the_header_icon_shows_the_version_tooltip_and_opens_the_panel(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.phase = UpdatePhase::Available {
                version: "0.2.0".into(),
                notes: "Заметки".into(),
            };
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    assert!(visual.debug_bounds("update-icon").is_some());

    let icon = center(&mut visual, "update-icon");
    visual.simulate_mouse_move(icon, None, Modifiers::default());
    draw(&mut visual);
    assert!(
        visual.debug_bounds("update-tooltip").is_some(),
        "hovering the icon shows the current and available version"
    );

    click(&mut visual, icon);
    assert!(visual.debug_bounds("update-panel").is_some());
    assert!(visual.debug_bounds("update-download").is_some());
    assert!(visual.debug_bounds("update-skip").is_some());
}

#[gpui::test]
async fn the_panel_renders_every_state_in_both_themes(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let staged = staged_update(&dir);
    let states: Vec<(UpdatePhase, &[&str])> = vec![
        (UpdatePhase::Idle, &["update-check"]),
        (UpdatePhase::Checking, &[]),
        (UpdatePhase::UpToDate, &["update-check"]),
        (
            UpdatePhase::Available {
                version: "0.2.0".into(),
                notes: "Что нового".into(),
            },
            &["update-download", "update-skip", "update-notes"],
        ),
        (
            UpdatePhase::Downloading {
                version: "0.2.0".into(),
                downloaded: 512,
                total: 1024,
            },
            &["update-progress", "update-percent", "update-cancel"],
        ),
        (
            UpdatePhase::Preparing {
                version: "0.2.0".into(),
            },
            &[],
        ),
        (
            UpdatePhase::Ready {
                version: "0.2.0".into(),
                staged: staged.clone(),
            },
            &["update-install"],
        ),
        (
            UpdatePhase::Error {
                message: "Сервер обновлений не нашёл манифест (404).".into(),
                retry: RetryKind::Check,
            },
            &["update-error", "update-retry"],
        ),
        (UpdatePhase::Installing, &[]),
    ];
    for light in [false, true] {
        for (phase, selectors) in &states {
            visual.update(|window, app| {
                view.update(app, |v, cx| {
                    v.palette = if light {
                        theme::light_palette()
                    } else {
                        theme::dark_palette()
                    };
                    v.update.phase = (*phase).clone();
                    v.update.panel_open = true;
                    cx.notify();
                });
                let _ = window;
            });
            draw(&mut visual);
            assert!(
                visual.debug_bounds("update-panel").is_some(),
                "{phase:?} must paint the panel"
            );
            for selector in ["update-auto-download", "update-install-next"] {
                assert!(
                    visual.debug_bounds(selector).is_some(),
                    "{phase:?} is missing {selector}"
                );
            }
            for selector in *selectors {
                assert!(
                    visual.debug_bounds(selector).is_some(),
                    "{phase:?} is missing {selector}"
                );
            }
        }
    }
}

#[gpui::test]
async fn the_panel_stays_inside_narrow_and_scaled_windows(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.phase = UpdatePhase::Available {
                version: "0.2.0".into(),
                notes: "Заметки".into(),
            };
            v.update.panel_open = true;
            cx.notify();
        });
        let _ = window;
    });
    for (scale, width, height) in [
        (1.0_f32, 320.0_f32, 360.0_f32),
        (1.0, 390.0, 760.0),
        (1.0, 720.0, 700.0),
        (1.0, 1280.0, 900.0),
        (1.25, 1024.0, 700.0),
        (1.5, 853.0, 600.0),
    ] {
        visual.update(|window, app| {
            view.update(app, |v, _| v.test_pin_ui_scale(scale));
            let _ = window;
        });
        visual.simulate_resize(size(px(width), px(height)));
        draw(&mut visual);
        draw(&mut visual);
        let panel = visual.debug_bounds("update-panel").expect("panel");
        let left = f32::from(panel.origin.x);
        let top = f32::from(panel.origin.y);
        let right = left + f32::from(panel.size.width);
        let bottom = top + f32::from(panel.size.height);
        assert!(left >= -0.5, "{width}x{height} @{scale}: left {left}");
        assert!(top >= -0.5, "{width}x{height} @{scale}: top {top}");
        assert!(
            right <= width + 0.5,
            "{width}x{height} @{scale}: right {right}"
        );
        assert!(
            bottom <= height + 0.5,
            "{width}x{height} @{scale}: bottom {bottom}"
        );
        for selector in ["update-download", "update-skip"] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{width}x{height} @{scale}: {selector}"));
            let button_right = f32::from(bounds.origin.x) + f32::from(bounds.size.width);
            assert!(
                f32::from(bounds.origin.x) >= -0.5 && button_right <= width + 0.5,
                "{width}x{height} @{scale}: {selector} leaves the client"
            );
        }
    }
}

#[gpui::test]
async fn the_panel_stops_above_the_mobile_bar_and_keeps_the_buttons(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.test_pin_ui_scale(1.0);
            v.update.phase = UpdatePhase::Available {
                version: "0.2.0".into(),
                notes: long_notes(),
            };
            v.update.panel_open = true;
            cx.notify();
        });
        let _ = window;
    });
    visual.simulate_resize(size(px(320.), px(360.)));
    draw(&mut visual);
    draw(&mut visual);

    let panel = visual.debug_bounds("update-panel").expect("panel");
    let panel_top = f32::from(panel.origin.y);
    let panel_bottom = panel_top + f32::from(panel.size.height);
    let chat_bottom = 360.0 - crate::layout::BOTTOM_HEIGHT;
    assert!(
        panel_bottom <= chat_bottom + 0.5,
        "the panel ends at {panel_bottom}, under the mobile bar at {chat_bottom}"
    );
    assert!(
        visual.debug_bounds("update-notes").is_some(),
        "long notes are painted"
    );
    for selector in ["update-download", "update-skip"] {
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"));
        let top = f32::from(bounds.origin.y);
        let bottom = top + f32::from(bounds.size.height);
        let right = f32::from(bounds.origin.x) + f32::from(bounds.size.width);
        assert!(
            top >= panel_top - 0.5 && bottom <= panel_bottom + 0.5,
            "{selector} leaves the panel ({top}..{bottom} vs {panel_top}..{panel_bottom})"
        );
        assert!(
            bottom <= chat_bottom + 0.5,
            "{selector} is cut by the mobile bar"
        );
        assert!(right <= 320.0 + 0.5, "{selector} leaves the client area");
    }
}

#[gpui::test]
async fn clicking_outside_the_panel_closes_it(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.phase = UpdatePhase::Available {
                version: "0.2.0".into(),
                notes: "Заметки".into(),
            };
            v.update.panel_open = true;
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    assert!(visual.debug_bounds("overlay-backdrop").is_some());
    click(&mut visual, gpui::point(px(2.), px(2.)));
    view.read_with(&*visual, |v, _| assert!(!v.update.panel_open));
}

#[gpui::test]
async fn skipping_a_version_persists_the_choice(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.phase = UpdatePhase::Available {
                version: "0.2.0".into(),
                notes: String::new(),
            };
            v.skip_version(cx);
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(matches!(v.update.phase, UpdatePhase::UpToDate));
        assert_eq!(v.update.declined, Some(semver::Version::new(0, 2, 0)));
        let prefs = update::load_prefs(v.update.prefs_path.as_deref());
        assert_eq!(prefs.declined(), Some(semver::Version::new(0, 2, 0)));
    });
}

#[gpui::test]
async fn a_background_check_respects_the_switch_and_the_cancel_suppression(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();

    // The switch off leaves the version visible but does not download it.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.prefs.auto_download = false;
            v.update.phase = UpdatePhase::Checking;
            let generation = v.update.generation;
            v.finish_check(generation, Ok(verified_manifest("9.9.9")), cx);
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(matches!(
            &v.update.phase,
            UpdatePhase::Available { version, .. } if version == "9.9.9"
        ));
        assert!(v.update.auto_started.is_none());
    });

    // The switch on, but this process already suppressed the version.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.prefs.auto_download = true;
            v.update.auto_suppressed = Some("9.9.9".into());
            v.update.phase = UpdatePhase::Checking;
            let generation = v.update.generation;
            v.finish_check(generation, Ok(verified_manifest("9.9.9")), cx);
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(matches!(
            &v.update.phase,
            UpdatePhase::Available { version, .. } if version == "9.9.9"
        ));
        assert!(v.update.auto_started.is_none());
    });
}

#[gpui::test]
async fn a_logout_and_a_closed_panel_keep_the_download(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let token = CancelToken::new();
    let generation = 5;
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.phase = UpdatePhase::Downloading {
                version: "0.2.0".into(),
                downloaded: 10,
                total: 100,
            };
            v.update.cancel = Some(token.clone());
            v.update.generation = generation;
            v.update.panel_open = true;
            cx.notify();
        });
        let _ = window;
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| v.close_update_panel(cx));
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(!token.is_cancelled(), "closing the panel is not a cancel");
        assert!(matches!(v.update.phase, UpdatePhase::Downloading { .. }));
        assert_eq!(v.update.generation, generation);
        assert!(!v.update.panel_open);
    });

    visual.update(|window, app| {
        view.update(app, |v, _| v.end_update_session());
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(!token.is_cancelled(), "logout is not a cancel");
        assert!(matches!(v.update.phase, UpdatePhase::Downloading { .. }));
        assert_eq!(v.update.generation, generation);
        assert!(!v.update.panel_open);
    });

    // A ready package found before the logout also survives the session.
    let staged = staged_update(&dir);
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.update.phase = UpdatePhase::Ready {
                version: "9.9.9".into(),
                staged: staged.clone(),
            };
            v.end_update_session();
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(matches!(v.update.phase, UpdatePhase::Ready { .. }));
    });
}

#[gpui::test]
async fn the_preference_switches_persist_and_keep_the_skipped_version(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.prefs.set_declined(&semver::Version::new(0, 2, 0));
            v.update.declined = v.update.prefs.declined();
            update::save_prefs(v.update.prefs_path.as_deref(), &v.update.prefs).unwrap();
            v.update.phase = UpdatePhase::UpToDate;
            v.update.panel_open = true;
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    let auto = center(&mut visual, "update-auto-download");
    click(&mut visual, auto);
    view.read_with(&*visual, |v, _| {
        assert!(!v.update.prefs.auto_download);
        assert!(v.update.prefs.install_on_next_start);
        assert_eq!(v.update.declined, Some(semver::Version::new(0, 2, 0)));
        let saved = update::load_prefs(v.update.prefs_path.as_deref());
        assert!(!saved.auto_download);
        assert!(saved.install_on_next_start);
        assert_eq!(saved.declined(), Some(semver::Version::new(0, 2, 0)));
    });

    let install = center(&mut visual, "update-install-next");
    click(&mut visual, install);
    view.read_with(&*visual, |v, _| {
        assert!(!v.update.prefs.auto_download);
        assert!(!v.update.prefs.install_on_next_start);
        let saved = update::load_prefs(v.update.prefs_path.as_deref());
        assert!(!saved.auto_download);
        assert!(!saved.install_on_next_start);
        assert_eq!(saved.declined(), Some(semver::Version::new(0, 2, 0)));
    });
}

#[gpui::test]
async fn a_logout_does_not_invalidate_a_manual_check(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.phase = UpdatePhase::Checking;
            v.update.generation = 3;
            v.update.prefs.auto_download = false;
            v.end_update_session();
            v.finish_check(3, Ok(verified_manifest("9.9.9")), cx);
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(matches!(
            &v.update.phase,
            UpdatePhase::Available { version, .. } if version == "9.9.9"
        ));
    });
}

#[gpui::test]
async fn cancelling_a_download_sets_the_token(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let token = CancelToken::new();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.phase = UpdatePhase::Downloading {
                version: "0.2.0".into(),
                downloaded: 10,
                total: 100,
            };
            v.update.cancel = Some(token.clone());
            v.update.panel_open = true;
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    let cancel = center(&mut visual, "update-cancel");
    click(&mut visual, cancel);
    view.read_with(&*visual, |v, _| {
        assert!(v.update.cancel.as_ref().unwrap().is_cancelled());
        assert!(v.update.cancelling);
        assert_eq!(v.update.auto_suppressed.as_deref(), Some("0.2.0"));
    });
}

#[gpui::test]
async fn a_cancelled_download_returns_to_available_without_a_new_auto_start(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let token = CancelToken::new();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.verified = Some(verified_manifest("9.9.9"));
            v.update.phase = UpdatePhase::Downloading {
                version: "9.9.9".into(),
                downloaded: 10,
                total: 100,
            };
            v.update.cancel = Some(token.clone());
            v.update.generation = 4;
            v.cancel_download(cx);
            v.finish_download(4, StageOutcome::Failed(UpdateError::Cancelled), cx);
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(matches!(
            &v.update.phase,
            UpdatePhase::Available { version, .. } if version == "9.9.9"
        ));
        assert_eq!(v.update.auto_suppressed.as_deref(), Some("9.9.9"));
        // A later check must not start this version again automatically.
        assert!(!update::should_auto_download(
            &v.update.prefs,
            "9.9.9",
            v.update.declined.as_ref(),
            v.update.auto_suppressed.as_deref(),
            v.update.auto_started.as_deref(),
        ));
    });
}

#[gpui::test]
async fn a_404_or_a_broken_signature_becomes_an_error_state(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, _dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let generation = v.update.generation;
            v.finish_check(
                generation,
                Err(UpdateError::HttpStatus {
                    url: "https://example.invalid/latest.json?token=secret".into(),
                    status: 404,
                }),
                cx,
            );
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| match &v.update.phase {
        UpdatePhase::Error { message, retry } => {
            assert_eq!(*retry, RetryKind::Check);
            assert!(message.contains("404"));
            assert!(!message.contains("secret"));
        }
        other => panic!("expected an error, got {other:?}"),
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let generation = v.update.generation;
            v.finish_check(generation, Err(UpdateError::BadSignature), cx);
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| match &v.update.phase {
        UpdatePhase::Error { message, retry } => {
            assert_eq!(*retry, RetryKind::Check);
            assert!(message.contains("Подпись"));
        }
        other => panic!("expected an error, got {other:?}"),
    });

    // A late answer from an older generation must not replace the state.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.generation = 7;
            v.update.phase = UpdatePhase::UpToDate;
            v.finish_check(6, Err(UpdateError::BadSignature), cx);
        });
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        assert!(matches!(v.update.phase, UpdatePhase::UpToDate));
    });
}

struct RecordingLauncher {
    seen: Rc<RefCell<Vec<LaunchRequest>>>,
    fail: bool,
}

impl UpdateLauncher for RecordingLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<(), String> {
        self.seen.borrow_mut().push(LaunchRequest {
            exe: request.exe.clone(),
            args: request.args.clone(),
        });
        if self.fail {
            Err("Не удалось запустить установщик.".into())
        } else {
            Ok(())
        }
    }
}

#[gpui::test]
async fn install_uses_absolute_arguments_and_never_detached(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let staged = staged_update(&dir);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let install = dir.join("install");
    let staging = staged.staging_dir.clone();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher {
                seen: seen.clone(),
                fail: false,
            });
            v.update.phase = UpdatePhase::Ready {
                version: "0.2.0".into(),
                staged: staged.clone(),
            };
            v.update.install_dir = install.clone();
            v.install_ready(window, cx);
        });
    });
    let seen = seen.borrow();
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(request.exe, staged.app_dir.join("voxhold-updater.exe"));
    assert!(!request.has_arg("--detached"));
    assert_eq!(
        request.value_after("--target").as_deref(),
        Some(install.to_string_lossy().as_ref())
    );
    assert_eq!(
        request.value_after("--staged").as_deref(),
        Some(staging.to_string_lossy().as_ref())
    );
    let handshake = request.value_after("--handshake").expect("handshake");
    assert!(Path::new(&handshake).is_absolute());
    assert!(handshake.ends_with("handshake.ok"));
    assert_eq!(request.value_after("--exe").as_deref(), Some("Voxhold.exe"));
    let expected_pid = std::process::id().to_string();
    assert_eq!(
        request.value_after("--pid").as_deref(),
        Some(expected_pid.as_str())
    );
    view.read_with(&*visual, |v, _| {
        assert!(v.update.quit_requested);
        assert!(matches!(v.update.phase, UpdatePhase::Installing));
    });
}

#[gpui::test]
async fn a_failed_helper_launch_keeps_the_app_in_the_error_state(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let staged = staged_update(&dir);
    let seen = Rc::new(RefCell::new(Vec::new()));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher {
                seen: seen.clone(),
                fail: true,
            });
            v.update.phase = UpdatePhase::Ready {
                version: "0.2.0".into(),
                staged: staged.clone(),
            };
            v.update.install_dir = dir.join("install");
            v.install_ready(window, cx);
        });
    });
    assert_eq!(seen.borrow().len(), 1);
    view.read_with(&*visual, |v, _| {
        assert!(!v.update.quit_requested);
        match &v.update.phase {
            UpdatePhase::Error { retry, .. } => assert_eq!(*retry, RetryKind::Install),
            other => panic!("expected an install error, got {other:?}"),
        }
    });
}

#[gpui::test]
async fn install_falls_back_to_the_packaged_exe_name(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    // The package ships `Voxhold.exe`; the running development binary has a
    // different name and must not be handed to the helper.
    let staged = staged_update(&dir);
    let seen = Rc::new(RefCell::new(Vec::new()));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher {
                seen: seen.clone(),
                fail: false,
            });
            v.update.exe_name = "voxhold-native-rust.exe".into();
            v.update.phase = UpdatePhase::Ready {
                version: "0.2.0".into(),
                staged: staged.clone(),
            };
            v.update.install_dir = dir.join("install");
            v.install_ready(window, cx);
        });
    });
    let seen = seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].value_after("--exe").as_deref(), Some("Voxhold.exe"));
    view.read_with(&*visual, |v, _| {
        assert!(v.update.quit_requested);
        assert!(matches!(v.update.phase, UpdatePhase::Installing));
    });
}

#[gpui::test]
async fn a_package_without_the_app_image_never_quits(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let staged = staged_update_with_exe(&dir, None);
    let seen = Rc::new(RefCell::new(Vec::new()));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher {
                seen: seen.clone(),
                fail: false,
            });
            v.update.exe_name = "voxhold-native-rust.exe".into();
            v.update.phase = UpdatePhase::Ready {
                version: "0.2.0".into(),
                staged: staged.clone(),
            };
            v.update.install_dir = dir.join("install");
            v.install_ready(window, cx);
        });
    });
    assert!(
        seen.borrow().is_empty(),
        "the helper must not start without an image"
    );
    view.read_with(&*visual, |v, _| {
        assert!(!v.update.quit_requested, "the app stays open");
        match &v.update.phase {
            UpdatePhase::Error { message, retry } => {
                assert_eq!(*retry, RetryKind::Install);
                assert!(
                    message.contains("voxhold-native-rust.exe"),
                    "the error names the missing image: {message}"
                );
            }
            other => panic!("expected an install error, got {other:?}"),
        }
    });
}

#[cfg(windows)]
#[test]
fn a_busy_helper_does_not_break_the_startup_scan() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };

    struct Locked(HANDLE);
    impl Drop for Locked {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    fn lock(path: &Path) -> Locked {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0x8000_0000,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        Locked(handle)
    }

    let temp = std::env::temp_dir().join(format!(
        "voxhold-busy-helper-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let install = temp.join("install");
    let root = temp.join("updates");
    let busy = root.join("0.1.9");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&busy).unwrap();
    let helper = busy.join("helper.exe");
    std::fs::write(&helper, b"image").unwrap();
    let ready = root.join("0.2.0");
    std::fs::create_dir_all(ready.join("app")).unwrap();
    let marker = voxhold_update::stage::ReadyMarker {
        version: "0.2.0".into(),
        package_sha256: "a".repeat(64),
        install_dir: install.to_string_lossy().into_owned(),
        created_at: 1,
        method: StageMethod::Full,
        fallback_reason: None,
    };
    std::fs::write(
        ready.join(voxhold_update::stage::READY_FILE),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("0.1.8")).unwrap();

    let _lock = lock(&helper);
    let scan = update::startup_scan(
        &install,
        &semver::Version::new(0, 1, 0),
        Some(root.as_path()),
        true,
    );
    assert!(
        scan.error.is_none(),
        "a locked helper is not a startup error"
    );
    assert!(scan.ready.is_some(), "the ready marker is still reported");
    assert!(busy.exists(), "the locked helper directory is skipped");
    assert!(!root.join("0.1.8").exists(), "other leftovers are removed");
    assert!(ready.exists(), "the ready directory is preserved");

    std::fs::remove_dir_all(&temp).ok();
}

#[test]
fn only_a_written_handshake_allows_recovery_on_an_update_start() {
    use super::update_ops::startup_recovery_allowed;

    // Normal start: nothing to prove, the observer always runs.
    assert!(startup_recovery_allowed(false, false));
    assert!(startup_recovery_allowed(false, true));
    // Update start: only the handshake proves that the new version is alive.
    assert!(startup_recovery_allowed(true, true));
    assert!(!startup_recovery_allowed(true, false));
}

#[gpui::test]
async fn a_ready_marker_is_shown_but_not_applied(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let staged = staged_update(&dir);
    let scan = update::StartupScan {
        root: Some(dir.join("updates")),
        ready: Some(staged.clone()),
        error: None,
        warning: None,
    };
    visual.update(|window, app| {
        view.update(app, |v, cx| v.apply_startup_scan(scan, cx));
        let _ = window;
    });
    view.read_with(&*visual, |v, _| {
        match &v.update.phase {
            UpdatePhase::Ready { version, .. } => assert_eq!(version, "9.9.9"),
            other => panic!("expected Ready, got {other:?}"),
        }
        assert!(!v.update.quit_requested);
    });
}
