//! Helper handoff regressions: proof re-verification, the `helper-ready.ok`
//! handshake and the launch failures that must leave the app open.
//!
//! The staged package is a real signed one, so `reverify_staged` exercises the
//! same checks as the startup scan. A throw-away key is injected through
//! `UpdateUi::proof_key`; the embedded release key is not available to tests.

use super::update_ops::{LaunchRequest, UpdateLauncher};
use super::update_tests::make_app;
use crate::update::{self, RetryKind, UpdatePhase};
use gpui::{TestAppContext, VisualTestContext};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use voxhold_update::manifest::{FullPackage, Manifest, SetupPackage};
use voxhold_update::stage::{
    ReadyMarker, StageMethod, StagedUpdate, APP_DIR, MANIFEST_FILE, PACKAGE_FILE, READY_FILE,
    SIGNATURE_FILE,
};
use voxhold_update::verify::UpdatePublicKey;

/// Build a complete, signed staging directory under the app's test root.
///
/// The marker points at `dir/install`, which is the `install_dir` set by
/// `make_app`, so the proof matches.
fn signed_staged(dir: &Path, exe: Option<&str>) -> (StagedUpdate, UpdatePublicKey) {
    let install = dir.join("install");
    std::fs::create_dir_all(&install).unwrap();
    let source = dir.join("package-source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("voxhold-updater.exe"), b"helper").unwrap();
    if let Some(exe) = exe {
        std::fs::write(source.join(exe), b"app").unwrap();
    }
    let packed = voxhold_update::package::pack_with_index(&source, "9.9.9").unwrap();
    let staging = dir.join("updates").join("9.9.9");
    let app = staging.join(APP_DIR);
    voxhold_update::package::unpack(&packed.bytes, &app).unwrap();
    std::fs::write(staging.join(PACKAGE_FILE), &packed.bytes).unwrap();

    let manifest = Manifest {
        schema: 1,
        channel: update::CHANNEL.into(),
        version: "9.9.9".into(),
        min_windows: "10.0.19041".into(),
        published_at: "2026-11-19T12:00:00Z".into(),
        notes: "Заметки".into(),
        authenticode: false,
        full: FullPackage {
            url: "https://example.invalid/full.zip".into(),
            size: packed.size(),
            sha256: packed.sha256.clone(),
            package_sha256: packed.sha256.clone(),
        },
        setup: SetupPackage {
            url: "https://example.invalid/Setup.exe".into(),
            size: 10,
            sha256: "b".repeat(64),
        },
        deltas: Vec::new(),
    };
    let bytes = manifest.to_json();
    let (private, public) = voxhold_update::verify::generate_keypair().unwrap();
    let signature = voxhold_update::verify::sign_bytes(&private, &bytes).unwrap();
    std::fs::write(staging.join(MANIFEST_FILE), &bytes).unwrap();
    std::fs::write(staging.join(SIGNATURE_FILE), signature.as_bytes()).unwrap();
    let marker = ReadyMarker {
        version: "9.9.9".into(),
        package_sha256: packed.sha256.clone(),
        install_dir: install.to_string_lossy().into_owned(),
        created_at: 1,
        method: StageMethod::Full,
        fallback_reason: None,
        manifest_sha256: voxhold_update::package::sha256_hex(&bytes),
        manifest_signature: signature,
    };
    std::fs::write(
        staging.join(READY_FILE),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    let staged = StagedUpdate {
        version: semver::Version::new(9, 9, 9),
        method: StageMethod::Full,
        app_dir: app,
        staging_dir: staging,
        package_sha256: packed.sha256,
        verified: true,
    };
    (staged, UpdatePublicKey::from_base64(&public).unwrap())
}

/// Launcher that records the request and optionally writes the ready marker.
struct RecordingLauncher {
    seen: Rc<RefCell<Vec<LaunchRequest>>>,
    fail: bool,
    write_ready: bool,
}

impl RecordingLauncher {
    fn new(seen: &Rc<RefCell<Vec<LaunchRequest>>>, fail: bool, write_ready: bool) -> Self {
        Self {
            seen: seen.clone(),
            fail,
            write_ready,
        }
    }
}

impl UpdateLauncher for RecordingLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<(), String> {
        self.seen.borrow_mut().push(LaunchRequest {
            exe: request.exe.clone(),
            args: request.args.clone(),
        });
        if let Some(ready) = request.value_after("--ready").filter(|_| self.write_ready) {
            std::fs::write(ready, b"ready\n").map_err(|error| error.to_string())?;
        }
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
    let (staged, key) = signed_staged(&dir, Some("Voxhold.exe"));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let install = dir.join("install");
    let staging = staged.staging_dir.clone();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher::new(&seen, false, false));
            v.update.proof_key = Some(key);
            v.update.phase = UpdatePhase::Ready {
                version: "9.9.9".into(),
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
        assert!(v.install_started());
    });
}

#[gpui::test]
async fn a_failed_helper_launch_keeps_the_app_and_the_clock_alive(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (staged, key) = signed_staged(&dir, Some("Voxhold.exe"));
    let seen = Rc::new(RefCell::new(Vec::new()));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher::new(&seen, true, false));
            v.update.proof_key = Some(key);
            v.update.phase = UpdatePhase::Ready {
                version: "9.9.9".into(),
                staged: staged.clone(),
            };
            v.update.install_dir = dir.join("install");
            v.install_ready(window, cx);
        });
    });
    assert_eq!(seen.borrow().len(), 1);
    view.read_with(&*visual, |v, _| {
        assert!(!v.update.quit_requested);
        assert!(
            !v.install_started(),
            "a failed automatic install must not stop the check clock"
        );
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
    let (staged, key) = signed_staged(&dir, Some("Voxhold.exe"));
    let seen = Rc::new(RefCell::new(Vec::new()));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher::new(&seen, false, false));
            v.update.proof_key = Some(key);
            v.update.exe_name = "voxhold-native-rust.exe".into();
            v.update.phase = UpdatePhase::Ready {
                version: "9.9.9".into(),
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
    let (staged, key) = signed_staged(&dir, None);
    let seen = Rc::new(RefCell::new(Vec::new()));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher::new(&seen, false, false));
            v.update.proof_key = Some(key);
            v.update.exe_name = "voxhold-native-rust.exe".into();
            v.update.phase = UpdatePhase::Ready {
                version: "9.9.9".into(),
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

#[gpui::test]
async fn a_replaced_app_package_json_blocks_the_install(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (staged, key) = signed_staged(&dir, Some("Voxhold.exe"));
    // `package.json` is not in its own file table; the proof must compare it
    // with the verified ZIP index again at install time.
    std::fs::write(
        staged.app_dir.join(voxhold_update::package::PACKAGE_JSON),
        br#"{"version":"9.9.9","files":[]}"#,
    )
    .unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher::new(&seen, false, false));
            v.update.proof_key = Some(key);
            v.update.phase = UpdatePhase::Ready {
                version: "9.9.9".into(),
                staged: staged.clone(),
            };
            v.update.install_dir = dir.join("install");
            v.install_ready(window, cx);
        });
    });
    assert!(
        seen.borrow().is_empty(),
        "the helper must not start with a replaced package index"
    );
    view.read_with(&*visual, |v, _| {
        assert!(!v.update.quit_requested);
        match &v.update.phase {
            UpdatePhase::Error { retry, .. } => assert_eq!(*retry, RetryKind::Check),
            other => panic!("expected a proof error, got {other:?}"),
        }
    });
}

#[gpui::test]
async fn the_client_quits_only_after_the_helper_ready_marker(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (staged, key) = signed_staged(&dir, Some("Voxhold.exe"));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let ready = staged.staging_dir.join("helper-ready.ok");
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher::new(&seen, false, true));
            v.update.proof_key = Some(key);
            v.update.wait_for_helper_ready = true;
            v.update.phase = UpdatePhase::Ready {
                version: "9.9.9".into(),
                staged: staged.clone(),
            };
            v.update.install_dir = dir.join("install");
            v.install_ready(window, cx);
        });
        assert_eq!(seen.borrow().len(), 1);
        view.update(app, |v, cx| {
            assert!(
                !v.update.quit_requested,
                "the app must wait for helper-ready.ok"
            );
            assert!(
                v.helper_ready_tick(window, cx, &ready, false),
                "the marker written by the launcher finishes the wait"
            );
            assert!(v.update.quit_requested);
        });
    });
}

#[gpui::test]
async fn a_helper_ready_timeout_keeps_the_app_open_with_an_error(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view, dir) = make_app(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (staged, key) = signed_staged(&dir, Some("Voxhold.exe"));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let ready = staged.staging_dir.join("helper-ready.ok");
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.update.launcher = Box::new(RecordingLauncher::new(&seen, false, false));
            v.update.proof_key = Some(key);
            v.update.wait_for_helper_ready = true;
            v.update.phase = UpdatePhase::Ready {
                version: "9.9.9".into(),
                staged: staged.clone(),
            };
            v.update.install_dir = dir.join("install");
            v.install_ready(window, cx);
        });
        view.update(app, |v, cx| {
            assert!(!v.helper_ready_tick(window, cx, &ready, false));
            assert!(v.helper_ready_tick(window, cx, &ready, true));
        });
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.update.quit_requested, "the app stays open on a timeout");
        match &v.update.phase {
            UpdatePhase::Error { retry, .. } => assert_eq!(*retry, RetryKind::Install),
            other => panic!("expected a helper timeout, got {other:?}"),
        }
    });
}
