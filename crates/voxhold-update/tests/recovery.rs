//! Public recovery contract used by the application and the helper.

use std::path::Path;

use voxhold_update::recovery::{recover_install, RecoveryMode, RecoveryOutcome};
use voxhold_update::state::{self, InstallState, OperationStage};

fn backup(target: &Path, exe: &[u8]) {
    let backup = state::backup_path(target);
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("Voxhold.exe"), exe).unwrap();
}

#[test]
fn an_unknown_backup_is_kept_while_the_target_exists() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("Voxhold.exe"), b"current").unwrap();
    backup(&target, b"older");

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::Ambiguous { .. }));
    assert!(state::backup_path(&target).exists());
    assert_eq!(
        std::fs::read(target.join("Voxhold.exe")).unwrap(),
        b"current"
    );
}

#[test]
fn the_observer_never_restores_a_missing_target() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    backup(&target, b"older");

    let outcome = recover_install(&target, RecoveryMode::Observer).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::NeedsUpdater { .. }));
    assert!(!target.exists());
    assert!(state::backup_path(&target).exists());

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::RolledBack { .. }));
    assert_eq!(std::fs::read(target.join("Voxhold.exe")).unwrap(), b"older");
    assert!(!state::backup_path(&target).exists());
}

#[test]
fn a_corrupt_state_keeps_the_backup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("Voxhold.exe"), b"current").unwrap();
    backup(&target, b"older");
    std::fs::write(state::state_path(&target), b"not json").unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::Corrupt { .. }));
    assert!(state::backup_path(&target).exists());
    assert!(state::state_path(&target).exists());
}

#[test]
fn a_completed_state_without_its_installation_keeps_the_backup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    backup(&target, b"older");
    let operation = "0123456789abcdef";
    let document = InstallState {
        schema: state::STATE_SCHEMA,
        operation_id: operation.into(),
        stage: OperationStage::Completed,
        install_dir: target.clone(),
        backup_dir: state::backup_path(&target),
        staging_dir: state::operation_staging_path(&target, operation),
        handshake_path: temp.path().join("handshake.ok"),
        version: "1.1.0".into(),
        previous_version: Some("1.0.0".into()),
        had_install: true,
        created_at: 1,
    };
    state::save(&document).unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::Ambiguous { .. }));
    assert!(state::backup_path(&target).exists());
    assert!(state::state_path(&target).exists());
}
