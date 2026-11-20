//! Recovery tests for the crash windows that hooks cannot reproduce.

use super::*;
use crate::package;
use std::fs;

fn install_package(root: &Path, version: &str, exe: &[u8]) {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("Voxhold.exe"), exe).unwrap();
    fs::write(root.join("README.md"), b"readme").unwrap();
    let packed = package::pack_with_index(root, version).unwrap();
    fs::remove_dir_all(root).unwrap();
    package::unpack(&packed.bytes, root).unwrap();
}

fn state_for(
    temp: &Path,
    install: &Path,
    backup: &Path,
    stage: OperationStage,
    version: &str,
    previous: &str,
) -> InstallState {
    InstallState {
        schema: state::STATE_SCHEMA,
        operation_id: "op-1".into(),
        stage,
        install_dir: install.to_path_buf(),
        backup_dir: backup.to_path_buf(),
        staging_dir: state::operation_staging_path(install, "op-1"),
        handshake_path: temp.join("handshake.ok"),
        version: version.into(),
        previous_version: Some(previous.into()),
        had_install: true,
        created_at: 1,
    }
}

#[test]
fn a_prepared_state_without_a_target_restores_the_backup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let backup = state::backup_path(&target);
    install_package(&backup, "1.0.0", b"old exe");
    // The state was saved, then the directory rename happened, then the
    // helper died before saving `old-renamed`.
    let state = state_for(
        temp.path(),
        &target,
        &backup,
        OperationStage::Prepared,
        "1.1.0",
        "1.0.0",
    );
    state::save(&state).unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::RolledBack { .. }));
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
    assert!(!backup.exists());
    assert_eq!(state::load(&target), StateLoad::Missing);
}

#[test]
fn an_old_renamed_state_is_restored_before_the_new_rename() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let backup = state::backup_path(&target);
    install_package(&backup, "1.0.0", b"old exe");
    // The rename to the backup and the state save succeeded; the staging
    // rename did not run. The on-disk state is the only witness.
    let state = state_for(
        temp.path(),
        &target,
        &backup,
        OperationStage::OldRenamed,
        "1.1.0",
        "1.0.0",
    );
    state::save(&state).unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::RolledBack { .. }));
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
    assert!(!backup.exists());
    assert_eq!(state::load(&target), StateLoad::Missing);
}

#[test]
fn rolling_back_renames_the_failed_tree_and_restores_the_backup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let backup = state::backup_path(&target);
    install_package(&target, "1.1.0", b"new exe");
    install_package(&backup, "1.0.0", b"old exe");
    let staging = state::operation_staging_path(&target, "op-1");
    fs::create_dir_all(&staging).unwrap();
    let state = state_for(
        temp.path(),
        &target,
        &backup,
        OperationStage::RollingBack,
        "1.1.0",
        "1.0.0",
    );
    state::save(&state).unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::RolledBack { .. }));
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
    assert!(!backup.exists());
    assert!(!staging.exists());
    assert_eq!(state::load(&target), StateLoad::Missing);
    assert!(
        !state::operation_failed_path(&target, "op-1", 0).exists(),
        "the failed tree is removed after a successful restore"
    );
}

#[test]
fn rolling_back_without_a_backup_keeps_the_confirmed_installation() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let backup = state::backup_path(&target);
    install_package(&target, "1.1.0", b"new exe");
    let state = state_for(
        temp.path(),
        &target,
        &backup,
        OperationStage::RollingBack,
        "1.1.0",
        "1.0.0",
    );
    state::save(&state).unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(
        outcome,
        RecoveryOutcome::Completed {
            backup_removed: true,
            ..
        }
    ));
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");
    assert_eq!(state::load(&target), StateLoad::Missing);
}

#[test]
fn a_failed_restore_puts_the_failed_installation_back() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let backup = state::backup_path(&target);
    install_package(&target, "1.1.0", b"new exe");
    install_package(&backup, "1.0.0", b"old exe");
    let state = state_for(
        temp.path(),
        &target,
        &backup,
        OperationStage::RollingBack,
        "1.1.0",
        "1.0.0",
    );
    state::save(&state).unwrap();

    let result = restore_backup_with(&state, &|from, to| {
        if from == state.backup_dir.as_path() {
            Err(UpdateError::Apply("simulated backup rename failure".into()))
        } else {
            fsutil::rename_retry(from, to)
        }
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read(target.join("Voxhold.exe")).unwrap(),
        b"new exe",
        "the failed installation is renamed back when the backup cannot be restored"
    );
    assert!(backup.exists(), "the backup is still available");
    assert!(
        !state::operation_failed_path(&target, "op-1", 0).exists(),
        "no failed tree is left between the two paths"
    );
}

#[test]
fn a_missing_state_removes_orphan_staging_copies() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    let staging = state::operation_staging_path(&target, "deadbeef");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("settings.json"), b"user settings").unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert_eq!(outcome, RecoveryOutcome::Clean);
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
    assert!(!staging.exists(), "the orphan staging copy is removed");
    assert_eq!(state::load(&target), StateLoad::Missing);
}

#[test]
fn an_unusable_state_keeps_its_staging_copy() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    let staging = state::operation_staging_path(&target, "op-1");
    fs::create_dir_all(&staging).unwrap();
    fs::write(state::state_path(&target), b"{ not json").unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::Corrupt { .. }));
    assert!(
        staging.exists(),
        "only a missing state proves that no operation owns the staging copy"
    );
}
