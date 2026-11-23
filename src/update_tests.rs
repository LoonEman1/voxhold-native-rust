//! Offline update state tests: startup verification, auto-install decisions and preferences.

use super::*;
use voxhold_update::manifest::{FullPackage, SetupPackage};

fn manifest(version: &str) -> Manifest {
    Manifest {
        schema: 1,
        channel: CHANNEL.into(),
        version: version.into(),
        min_windows: "10.0.19041".into(),
        published_at: "2026-11-19T12:00:00Z".into(),
        notes: "Что нового".into(),
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
    }
}

#[test]
fn build_version_prefers_a_valid_override() {
    assert_eq!(
        version_from_sources(Some("0.2.0"), "0.1.0"),
        Version::new(0, 2, 0)
    );
    assert_eq!(
        version_from_sources(Some("not-a-version"), "0.1.0"),
        Version::new(0, 1, 0)
    );
    assert_eq!(
        version_from_sources(Some("  "), "0.1.0"),
        Version::new(0, 1, 0)
    );
}

#[test]
fn offers_only_a_newer_version_and_respects_dismissal() {
    let current = Version::new(0, 1, 0);
    let phase = evaluate_manifest(&manifest("0.2.0"), &current, None, None).unwrap();
    assert!(matches!(
        phase,
        UpdatePhase::Available { ref version, .. } if version == "0.2.0"
    ));

    let dismissed = Version::new(0, 2, 0);
    assert!(matches!(
        evaluate_manifest(&manifest("0.2.0"), &current, Some(&dismissed), None).unwrap(),
        UpdatePhase::UpToDate
    ));
    assert!(matches!(
        evaluate_manifest(&manifest("0.2.0"), &Version::new(0, 2, 0), None, None).unwrap(),
        UpdatePhase::UpToDate
    ));
    assert!(matches!(
        evaluate_manifest(&manifest("0.0.9"), &current, None, None),
        Err(UpdateError::Downgrade { .. })
    ));
}

#[test]
fn errors_are_short_and_never_leak_a_query() {
    let error = UpdateError::HttpStatus {
        url: "https://example.invalid/latest.json?token=secret".into(),
        status: 404,
    };
    let message = message_for_error(&error);
    assert!(message.contains("404"));
    assert!(!message.contains("secret"));
    assert!(!message.contains("https://"));

    let broken = message_for_error(&UpdateError::BadSignature);
    assert!(broken.contains("Подпись"));
    let network = message_for_error(&UpdateError::Network(
        "https://example.invalid/?token=secret".into(),
    ));
    assert!(!network.contains("secret"));
}

#[test]
fn urls_accept_loopback_and_https_but_reject_remote_http() {
    let (manifest, signature) = urls_from_base(Some("http://127.0.0.1:8123/")).unwrap();
    assert_eq!(manifest, "http://127.0.0.1:8123/latest.json");
    assert_eq!(signature, "http://127.0.0.1:8123/latest.json.sig");

    let (manifest, _) = urls_from_base(Some("https://example.invalid/assets")).unwrap();
    assert_eq!(manifest, "https://example.invalid/assets/latest.json");

    assert!(urls_from_base(Some("http://example.invalid")).is_err());
    let (manifest, signature) = urls_from_base(None).unwrap();
    assert!(manifest.starts_with("https://github.com/"));
    assert!(signature.ends_with("latest.json.sig"));
}

#[test]
fn progress_percent_is_bounded() {
    assert_eq!(percent(0, 100), 0);
    assert_eq!(percent(50, 100), 50);
    assert_eq!(percent(100, 100), 100);
    assert_eq!(percent(200, 100), 100);
    assert_eq!(percent(10, 0), 0);
}

#[test]
fn handshake_creates_parents_and_is_not_empty() {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-handshake-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join("nested").join("handshake.ok");
    write_handshake(&path, Some("op-1")).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(&current_version().to_string()));
    assert!(text.contains("op-1"));
    std::fs::remove_dir_all(&dir).ok();
}

/// Build a complete, correctly signed staging directory. The startup scan
/// re-verifies everything on disk, so a marker alone is not enough.
fn signed_staging(
    temp: &Path,
    root: &Path,
    install: &Path,
    version: &str,
) -> (PathBuf, UpdatePublicKey) {
    let source = temp.join(format!("source-{version}"));
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("Voxhold.exe"), b"new exe").unwrap();
    let packed = voxhold_update::package::pack_with_index(&source, version).unwrap();
    let staging = root.join(version);
    let app = staging.join(APP_DIR);
    voxhold_update::package::unpack(&packed.bytes, &app).unwrap();
    std::fs::write(
        staging.join(voxhold_update::stage::PACKAGE_FILE),
        &packed.bytes,
    )
    .unwrap();
    let mut manifest = manifest(version);
    manifest.full.size = packed.size();
    manifest.full.sha256 = packed.sha256.clone();
    manifest.full.package_sha256 = packed.sha256.clone();
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let (private, public) = voxhold_update::verify::generate_keypair().unwrap();
    let signature = voxhold_update::verify::sign_bytes(&private, &bytes).unwrap();
    std::fs::write(staging.join(MANIFEST_FILE), &bytes).unwrap();
    std::fs::write(staging.join(SIGNATURE_FILE), &signature).unwrap();
    let marker = voxhold_update::stage::ReadyMarker {
        version: version.into(),
        package_sha256: packed.sha256,
        install_dir: install.to_string_lossy().into_owned(),
        created_at: 1,
        method: voxhold_update::stage::StageMethod::Full,
        fallback_reason: None,
        manifest_sha256: voxhold_update::package::sha256_hex(&bytes),
        manifest_signature: signature,
    };
    std::fs::write(
        staging.join(READY_FILE),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    (staging, UpdatePublicKey::from_base64(&public).unwrap())
}

#[test]
fn startup_scan_reports_a_ready_update_and_keeps_it() {
    let temp = std::env::temp_dir().join(format!(
        "voxhold-startup-scan-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let install = temp.join("VoxHold");
    let root = temp.join("updates");
    std::fs::create_dir_all(install.join("assets")).unwrap();
    std::fs::create_dir_all(root.join("0.1.9")).unwrap();
    let (staging, key) = signed_staging(&temp, &root, &install, "0.2.0");

    let scan = startup_scan_with_key(
        &install,
        &Version::new(0, 1, 0),
        Some(root.as_path()),
        true,
        &key,
    );
    let ready = scan.ready.expect("ready update");
    assert_eq!(ready.version, Version::new(0, 2, 0));
    assert!(staging.exists(), "the ready directory must be preserved");
    assert!(!root.join("0.1.9").exists());

    std::fs::remove_dir_all(&temp).ok();
}

#[test]
fn startup_scan_keeps_a_handshake_directory_without_ready() {
    let temp = std::env::temp_dir().join(format!(
        "voxhold-handshake-keep-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let install = temp.join("VoxHold");
    let root = temp.join("updates");
    std::fs::create_dir_all(&install).unwrap();
    let staging = root.join("0.2.0");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("handshake.ok"), b"ok").unwrap();
    std::fs::create_dir_all(root.join("0.1.9")).unwrap();

    let scan = startup_scan(&install, &Version::new(0, 1, 0), Some(root.as_path()), true);
    assert!(scan.ready.is_none());
    assert!(
        staging.exists(),
        "the handshake directory must be preserved"
    );
    assert!(!root.join("0.1.9").exists());
    std::fs::remove_dir_all(&temp).ok();
}

#[test]
fn a_confirmed_update_start_completes_before_the_second_cleanup() {
    use voxhold_update::recovery::{recover_install, RecoveryMode, RecoveryOutcome};
    use voxhold_update::state::{self, InstallState, OperationStage, StateLoad};

    fn install_package(root: &std::path::Path, version: &str, exe: &[u8]) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("Voxhold.exe"), exe).unwrap();
        let packed = voxhold_update::package::pack_with_index(root, version).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        voxhold_update::package::unpack(&packed.bytes, root).unwrap();
    }

    let temp = std::env::temp_dir().join(format!(
        "voxhold-confirmed-start-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let install = temp.join("VoxHold");
    let root = temp.join("updates");
    let update_dir = root.join("0.2.0");
    let backup = state::backup_path(&install);
    install_package(&install, "0.2.0", b"new exe");
    install_package(&backup, "0.1.0", b"old exe");
    std::fs::create_dir_all(&update_dir).unwrap();
    // The helper stored the handshake in the staged update directory.
    let handshake = update_dir.join("handshake.ok");
    write_handshake(&handshake, Some("op-1")).unwrap();

    let state = InstallState {
        schema: state::STATE_SCHEMA,
        operation_id: "op-1".into(),
        stage: OperationStage::NewInPlace,
        install_dir: install.clone(),
        backup_dir: backup.clone(),
        staging_dir: state::operation_staging_path(&install, "op-1"),
        handshake_path: handshake.clone(),
        version: "0.2.0".into(),
        previous_version: Some("0.1.0".into()),
        had_install: true,
        created_at: 1,
    };
    state::save(&state).unwrap();

    // An update start with a written handshake must run the observer,
    // which persists completion while the proof still exists.
    let scan = startup_scan(&install, &Version::new(0, 2, 0), Some(root.as_path()), true);
    assert!(scan.error.is_none(), "{scan:?}");
    assert!(handshake.exists(), "the observer must run before cleanup");
    assert_eq!(state::load(&install), StateLoad::Missing);
    assert!(!backup.exists());

    // Only now may the 10 s cleanup remove the handshake directory.
    second_cleanup(&root, None).unwrap();
    assert!(!update_dir.exists());
    assert_eq!(
        recover_install(&install, RecoveryMode::Observer).unwrap(),
        RecoveryOutcome::Clean
    );
    assert_eq!(
        std::fs::read(install.join("Voxhold.exe")).unwrap(),
        b"new exe"
    );
    std::fs::remove_dir_all(&temp).ok();
}

#[test]
fn a_failed_handshake_keeps_the_update_unconfirmed() {
    // Without the handshake file the observer must not complete the
    // operation; recovery keeps the state and the backup for the helper.
    use voxhold_update::recovery::{recover_install, RecoveryMode, RecoveryOutcome};
    use voxhold_update::state::{self, InstallState, OperationStage, StateLoad};

    let temp = std::env::temp_dir().join(format!(
        "voxhold-unconfirmed-start-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let install = temp.join("VoxHold");
    std::fs::create_dir_all(&install).unwrap();
    let backup = state::backup_path(&install);
    std::fs::create_dir_all(&backup).unwrap();
    let handshake = temp.join("updates").join("handshake.ok");
    let state = InstallState {
        schema: state::STATE_SCHEMA,
        operation_id: "op-1".into(),
        stage: OperationStage::NewInPlace,
        install_dir: install.clone(),
        backup_dir: backup.clone(),
        staging_dir: state::operation_staging_path(&install, "op-1"),
        handshake_path: handshake,
        version: "0.2.0".into(),
        previous_version: Some("0.1.0".into()),
        had_install: true,
        created_at: 1,
    };
    state::save(&state).unwrap();

    let outcome = recover_install(&install, RecoveryMode::Observer).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::NeedsUpdater { .. }));
    assert!(matches!(state::load(&install), StateLoad::Valid(_)));
    assert!(backup.exists());
    std::fs::remove_dir_all(&temp).ok();
}

#[test]
fn second_cleanup_keeps_only_the_ready_directory() {
    let temp = std::env::temp_dir().join(format!(
        "voxhold-second-cleanup-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let root = temp.join("updates");
    std::fs::create_dir_all(root.join("0.2.0")).unwrap();
    std::fs::create_dir_all(root.join("0.3.0")).unwrap();
    let removed = second_cleanup(&root, Some(&Version::new(0, 2, 0))).unwrap();
    assert_eq!(removed, 1);
    assert!(root.join("0.2.0").exists());
    assert!(!root.join("0.3.0").exists());
    std::fs::remove_dir_all(&temp).ok();
}

#[test]
fn auto_install_requires_a_verified_newer_package_and_no_refusal() {
    let mut prefs = UpdatePrefs::default();
    let staging = PathBuf::from(r"C:\tmp\updates\id\0.2.0");
    let mut staged = StagedUpdate {
        version: Version::new(0, 2, 0),
        method: voxhold_update::stage::StageMethod::Full,
        staging_dir: staging.clone(),
        app_dir: staging.join(APP_DIR),
        package_sha256: "a".repeat(64),
        verified: true,
    };
    let current = Version::new(0, 1, 0);
    assert!(should_auto_install(&prefs, &staged, &current));
    staged.verified = false;
    assert!(!should_auto_install(&prefs, &staged, &current));
    staged.verified = true;
    prefs.install_on_next_start = false;
    assert!(!should_auto_install(&prefs, &staged, &current));
    prefs.install_on_next_start = true;
    prefs.install_refused_version = Some("0.2.0".into());
    assert!(!should_auto_install(&prefs, &staged, &current));
}

#[test]
fn a_refusal_sidecar_is_consumed_once() {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-refusal-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("update-prefs.refusal.json");
    std::fs::write(&path, br#"{"version":"0.2.0"}"#).unwrap();
    assert_eq!(take_refusal(Some(&path)).as_deref(), Some("0.2.0"));
    assert!(take_refusal(Some(&path)).is_none());
    std::fs::remove_dir_all(&dir).ok();
}
