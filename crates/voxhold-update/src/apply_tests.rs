use super::*;
use crate::package;
use crate::recovery::{recover_install, RecoveryMode, RecoveryOutcome};
use crate::stage::APP_DIR;
use std::cell::RefCell;
use std::fs;

struct FakeLauncher {
    handshake: bool,
    version: String,
    seen: RefCell<Option<String>>,
}

struct FakeProcess;

impl LaunchedProcess for FakeProcess {
    fn try_wait(&mut self) -> Result<Option<i32>> {
        Ok(None)
    }
    fn kill(&mut self) {}
}

impl UpdateLauncher for FakeLauncher {
    fn launch(
        &self,
        _exe: &Path,
        handshake: &Path,
        operation: &str,
    ) -> Result<Box<dyn LaunchedProcess>> {
        *self.seen.borrow_mut() = Some(operation.to_string());
        if self.handshake {
            crate::handshake::write(handshake, &self.version, Some(operation)).unwrap();
        }
        Ok(Box::new(FakeProcess))
    }
}

fn launcher(handshake: bool) -> FakeLauncher {
    FakeLauncher {
        handshake,
        version: "1.1.0".into(),
        seen: RefCell::new(None),
    }
}

struct Interrupt(Option<InterruptPoint>);

impl ApplyHooks for Interrupt {
    fn interrupt(&self, point: InterruptPoint) -> bool {
        self.0 == Some(point)
    }
}

struct RecordingObserver(RefCell<Vec<ApplyStage>>);

impl ApplyObserver for RecordingObserver {
    fn stage(&self, stage: ApplyStage) {
        self.0.borrow_mut().push(stage);
    }
}

impl RecordingObserver {
    fn stages(&self) -> Vec<ApplyStage> {
        self.0.borrow().clone()
    }
}

fn install_package(root: &Path, version: &str, exe: &[u8]) {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("Voxhold.exe"), exe).unwrap();
    fs::write(root.join("README.md"), b"readme").unwrap();
    let packed = package::pack_with_index(root, version).unwrap();
    fs::remove_dir_all(root).unwrap();
    package::unpack(&packed.bytes, root).unwrap();
}

fn staged_package_at(staging: &Path) -> PathBuf {
    let staged = staging.join(APP_DIR);
    fs::create_dir_all(&staged).unwrap();
    fs::write(staged.join("Voxhold.exe"), b"new exe").unwrap();
    fs::write(staged.join("README.md"), b"readme").unwrap();
    let packed = package::pack_with_index(&staged, "1.1.0").unwrap();
    fs::remove_dir_all(&staged).unwrap();
    package::unpack(&packed.bytes, &staged).unwrap();
    staged
}

fn staged_package(root: &Path) -> PathBuf {
    staged_package_at(&root.join("staged"))
}

fn run_with(
    target: &Path,
    staged: &Path,
    handshake: &Path,
    launcher: &dyn UpdateLauncher,
    hooks: &dyn ApplyHooks,
) -> Result<ApplyOutcome> {
    run_with_observer(target, staged, handshake, launcher, hooks, &NoObserver)
}

fn run_with_observer(
    target: &Path,
    staged: &Path,
    handshake: &Path,
    launcher: &dyn UpdateLauncher,
    hooks: &dyn ApplyHooks,
    observer: &dyn ApplyObserver,
) -> Result<ApplyOutcome> {
    apply_staged_with_hooks(
        target,
        staged,
        "Voxhold.exe",
        handshake,
        launcher,
        Duration::from_millis(400),
        hooks,
        observer,
    )
}

fn assert_state_cleared(target: &Path) {
    assert_eq!(state::load(target), StateLoad::Missing);
    assert!(!state::state_path(target).exists());
}

fn assert_runnable(target: &Path) {
    let index = read_package_json_from_dir(target).expect("package.json in the installation");
    assert!(!index.version.is_empty());
    assert!(target.join("Voxhold.exe").is_file());
}

fn staging_siblings(target: &Path) -> Vec<PathBuf> {
    let Some(parent) = target.parent() else {
        return Vec::new();
    };
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{name}{}", state::STAGING_SUFFIX).to_lowercase();
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_lowercase()
                .starts_with(&prefix)
        })
        .map(|entry| entry.path())
        .collect()
}

#[test]
fn applies_and_preserves_user_files() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    fs::write(target.join("settings.json"), b"user settings").unwrap();
    let staged = staged_package(temp.path());

    let outcome = run_with(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(true),
        &NoHooks,
    )
    .unwrap();

    assert!(outcome.backup_removed);
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");
    assert_eq!(
        fs::read(target.join("settings.json")).unwrap(),
        b"user settings"
    );
    assert!(!backup_path(&target).exists());
    assert!(!staged.exists());
    assert_state_cleared(&target);
}

#[test]
fn preserves_new_package_files_when_an_existing_path_differs_only_in_case() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("VOXHOLD.EXE"), b"old exe").unwrap();

    let staged = staged_package(temp.path());
    run_with(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(true),
        &NoHooks,
    )
    .unwrap();

    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");
    assert_eq!(fs::read_dir(&target).unwrap().count(), 3);
}

#[test]
fn the_observer_reports_the_apply_stages() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    let staged = staged_package(temp.path());
    let observer = RecordingObserver(RefCell::new(Vec::new()));
    let outcome = run_with_observer(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(true),
        &NoHooks,
        &observer,
    )
    .unwrap();
    assert!(outcome.backup_removed);
    assert_eq!(
        observer.stages(),
        vec![
            ApplyStage::Verifying,
            ApplyStage::Replacing,
            ApplyStage::Launching,
            ApplyStage::Done,
        ]
    );

    // A failed launch reports the rollback and never a completion.
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    let staged = staged_package(temp.path());
    let observer = RecordingObserver(RefCell::new(Vec::new()));
    let error = run_with_observer(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(false),
        &NoHooks,
        &observer,
    )
    .unwrap_err();
    assert!(matches!(error, UpdateError::LaunchTimeout(_)));
    assert_eq!(
        observer.stages(),
        vec![
            ApplyStage::Verifying,
            ApplyStage::Replacing,
            ApplyStage::Launching,
            ApplyStage::RollingBack,
        ]
    );
}

#[test]
fn a_broken_payload_still_reports_the_rollback_stage() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    // No package.json: the apply fails while verifying, before any durable
    // state exists, and only cleans the staging copy up.
    let staged = temp.path().join("broken");
    fs::create_dir_all(&staged).unwrap();
    let observer = RecordingObserver(RefCell::new(Vec::new()));
    let error = run_with_observer(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(true),
        &NoHooks,
        &observer,
    )
    .unwrap_err();
    let debug = format!("{error:?}");
    assert!(matches!(error, UpdateError::Io { .. }), "{debug}");
    assert_eq!(
        observer.stages(),
        vec![ApplyStage::Verifying, ApplyStage::RollingBack]
    );
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
}

#[test]
fn rolls_back_when_launch_fails() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    fs::write(target.join("settings.json"), b"user settings").unwrap();
    let staged = staged_package(temp.path());

    let error = run_with(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(false),
        &NoHooks,
    )
    .unwrap_err();
    assert!(matches!(error, UpdateError::LaunchTimeout(_)));
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
    assert_eq!(
        fs::read(target.join("settings.json")).unwrap(),
        b"user settings"
    );
    assert!(!backup_path(&target).exists());
    assert_state_cleared(&target);
}

#[test]
fn every_interruption_point_converges_without_losing_user_files() {
    let points = [
        InterruptPoint::Staged,
        InterruptPoint::Verified,
        InterruptPoint::Prepared,
        InterruptPoint::OldRenamed,
        InterruptPoint::NewInPlace,
        InterruptPoint::Launched,
        InterruptPoint::Confirmed,
    ];
    for point in points {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("VoxHold");
        install_package(&target, "1.0.0", b"old exe");
        fs::write(target.join("settings.json"), b"user settings").unwrap();
        let staged = staged_package(temp.path());

        // After launch but before confirmation there must be no handshake.
        let handshake = point != InterruptPoint::Launched;
        let handshake_path = temp.path().join("ready.handshake");
        let result = run_with(
            &target,
            &staged,
            &handshake_path,
            &launcher(handshake),
            &Interrupt(Some(point)),
        );
        assert!(result.is_err(), "{point:?} must stop the helper");

        // Prove the interruption happened where it claims: a test that stops
        // before the state write must not look like a later one.
        let saved = state::load(&target);
        match point {
            InterruptPoint::Staged | InterruptPoint::Verified => {
                assert!(matches!(saved, StateLoad::Missing), "{point:?}");
                assert!(
                    !staging_siblings(&target).is_empty(),
                    "{point:?} must leave the copied staging directory"
                );
                assert!(!backup_path(&target).exists(), "{point:?}");
                assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
            }
            InterruptPoint::Prepared => {
                let StateLoad::Valid(state) = &saved else {
                    panic!("{point:?} must persist the prepared state");
                };
                assert_eq!(state.stage, OperationStage::Prepared);
                assert!(state.staging_dir.exists(), "{point:?}");
                assert!(target.exists(), "{point:?}");
                assert!(!backup_path(&target).exists(), "{point:?}");
            }
            InterruptPoint::OldRenamed => {
                let StateLoad::Valid(state) = &saved else {
                    panic!("{point:?} must persist the renamed state");
                };
                assert_eq!(state.stage, OperationStage::OldRenamed);
                assert!(state.backup_dir.exists(), "{point:?}");
                assert!(!target.exists(), "{point:?}");
            }
            InterruptPoint::NewInPlace | InterruptPoint::Launched => {
                let StateLoad::Valid(state) = &saved else {
                    panic!("{point:?} must persist the in-place state");
                };
                assert_eq!(state.stage, OperationStage::NewInPlace);
                assert!(state.backup_dir.exists(), "{point:?}");
                assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");
                assert!(
                    !crate::handshake::confirms(&state.handshake_path, &state.operation_id),
                    "{point:?} must not have confirmation"
                );
            }
            InterruptPoint::Confirmed => {
                let StateLoad::Valid(state) = &saved else {
                    panic!("{point:?} must persist the in-place state");
                };
                assert_eq!(state.stage, OperationStage::NewInPlace);
                assert!(state.backup_dir.exists(), "{point:?}");
                assert!(
                    crate::handshake::confirms(&state.handshake_path, &state.operation_id),
                    "{point:?} must leave the handshake that interrupted it"
                );
                assert!(handshake_path.exists());
            }
        }

        let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
        assert!(
            !matches!(
                outcome,
                RecoveryOutcome::Ambiguous { .. }
                    | RecoveryOutcome::Corrupt { .. }
                    | RecoveryOutcome::NeedsUpdater { .. }
            ),
            "{point:?} did not converge: {outcome:?}"
        );
        assert_runnable(&target);
        assert_eq!(
            fs::read(target.join("settings.json")).unwrap(),
            b"user settings",
            "{point:?} lost the user file"
        );
        assert!(!backup_path(&target).exists(), "{point:?} left a backup");
        assert_state_cleared(&target);

        let exe = fs::read(target.join("Voxhold.exe")).unwrap();
        if point == InterruptPoint::Confirmed {
            assert_eq!(exe, b"new exe", "{point:?} should complete forward");
        } else {
            assert_eq!(exe, b"old exe", "{point:?} should roll back");
        }
    }
}

#[test]
fn an_interrupted_operation_resumes_before_a_new_update() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    fs::write(target.join("settings.json"), b"user settings").unwrap();
    let staged = staged_package(temp.path());
    run_with(
        &target,
        &staged,
        &temp.path().join("first.handshake"),
        &launcher(true),
        &Interrupt(Some(InterruptPoint::OldRenamed)),
    )
    .unwrap_err();

    // A later helper run resolves the interrupted state and can install again.
    let staged = staged_package_at(&temp.path().join("second-staged"));
    let outcome = apply_staged_with(
        &target,
        &staged,
        "Voxhold.exe",
        &temp.path().join("second.handshake"),
        &launcher(true),
        Duration::from_millis(400),
    )
    .unwrap();
    assert!(outcome.backup_removed);
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");
    assert_eq!(
        fs::read(target.join("settings.json")).unwrap(),
        b"user settings"
    );
    assert_state_cleared(&target);
}

#[test]
fn a_missing_state_keeps_an_unknown_backup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    let backup = backup_path(&target);
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("Voxhold.exe"), b"older exe").unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::Ambiguous { .. }));
    assert!(backup.exists(), "the unknown backup must stay");
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
}

#[test]
fn a_new_update_does_not_delete_an_unknown_legacy_backup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    fs::write(target.join("settings.json"), b"user settings").unwrap();
    let legacy = backup_path(&target);
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("Voxhold.exe"), b"even older exe").unwrap();
    let staged = staged_package(temp.path());

    let outcome = run_with(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(true),
        &NoHooks,
    )
    .unwrap();
    assert!(outcome.backup_removed);
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");
    assert_eq!(
        fs::read(target.join("settings.json")).unwrap(),
        b"user settings"
    );
    assert!(legacy.exists(), "the unknown legacy backup must stay");
    assert_eq!(
        fs::read(legacy.join("Voxhold.exe")).unwrap(),
        b"even older exe"
    );
    assert_state_cleared(&target);
}

#[test]
fn a_completed_state_without_the_confirmed_installation_keeps_the_backup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let backup = backup_path(&target);
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("Voxhold.exe"), b"older exe").unwrap();
    let state = InstallState {
        schema: state::STATE_SCHEMA,
        operation_id: "0123456789abcdef".into(),
        stage: OperationStage::Completed,
        install_dir: target.clone(),
        backup_dir: backup.clone(),
        staging_dir: state::operation_staging_path(&target, "0123456789abcdef"),
        handshake_path: temp.path().join("handshake.ok"),
        version: "1.1.0".into(),
        previous_version: Some("1.0.0".into()),
        had_install: true,
        created_at: 1,
    };
    state::save(&state).unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::Ambiguous { .. }));
    assert!(backup.exists(), "the only installation must stay");
    assert!(state::state_path(&target).exists());
}

#[test]
fn a_corrupt_state_keeps_the_backup() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    let backup = backup_path(&target);
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("Voxhold.exe"), b"older exe").unwrap();
    fs::write(state::state_path(&target), b"{ not json").unwrap();

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::Corrupt { .. }));
    assert!(backup.exists(), "the backup must stay");
    assert!(state::state_path(&target).exists());
}

#[test]
fn an_observer_never_replaces_directories() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let backup = backup_path(&target);
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("Voxhold.exe"), b"older exe").unwrap();

    let outcome = recover_install(&target, RecoveryMode::Observer).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::NeedsUpdater { .. }));
    assert!(!target.exists());
    assert!(backup.exists());

    let outcome = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::RolledBack { .. }));
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"older exe");
    assert!(!backup.exists());
}

#[test]
fn helper_in_the_installation_is_relocated_before_apply() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    let staging = temp.path().join("updates").join("0.2.0");
    let staged = staged_package_at(&staging);
    let helper = target.join("voxhold-updater.exe");
    fs::write(&helper, b"helper image").unwrap();

    let destination = helper_relocation_destination(&helper, &target, &staging).unwrap();
    assert_eq!(destination, staging.join(HELPER_FILE));
    relocate_helper(&helper, &destination).unwrap();
    fs::remove_file(&helper).unwrap();

    let outcome = run_with(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(true),
        &NoHooks,
    )
    .unwrap();
    assert!(outcome.backup_removed);
    assert!(!backup_path(&target).exists());
    assert_eq!(fs::read(&destination).unwrap(), b"helper image");
}

#[test]
fn helper_relocates_out_of_the_installation() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let staged = temp.path().join("updates").join("0.2.0");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&staged).unwrap();
    let helper = target.join(HELPER_FILE);
    fs::write(&helper, b"helper image").unwrap();

    let destination = helper_relocation_destination(&helper, &target, &staged).unwrap();
    assert_eq!(destination, staged.join(HELPER_FILE));
    relocate_helper(&helper, &destination).unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"helper image");
    assert!(helper_relocation_destination(&destination, &target, &staged).is_none());
}

#[test]
fn helper_outside_the_trees_is_not_relocated() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let staged = temp.path().join("updates").join("0.2.0");
    let elsewhere = temp.path().join("tools");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&staged).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    let helper = elsewhere.join(HELPER_FILE);
    fs::write(&helper, b"helper image").unwrap();
    assert!(helper_relocation_destination(&helper, &target, &staged).is_none());
}

#[cfg(windows)]
struct LockedFile(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for LockedFile {
    fn drop(&mut self) {
        // SAFETY: the handle was created by `CreateFileW` and is closed once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn lock_without_sharing(path: &Path) -> LockedFile {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };

    const GENERIC_READ: u32 = 0x8000_0000;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(handle, INVALID_HANDLE_VALUE, "CreateFileW failed");
    LockedFile(handle)
}

#[cfg(windows)]
#[test]
fn a_locked_backup_is_not_deleted_and_is_cleaned_later() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    install_package(&target, "1.0.0", b"old exe");
    let staged = staged_package(temp.path());
    let handshake = temp.path().join("ready.handshake");

    // Lock the old image after the swap. The fake launcher writes the
    // handshake, so the operation is confirmed but the backup cannot go.
    struct LockingLauncher {
        lock: RefCell<Option<LockedFile>>,
        version: String,
    }
    impl UpdateLauncher for LockingLauncher {
        fn launch(
            &self,
            exe: &Path,
            handshake: &Path,
            operation: &str,
        ) -> Result<Box<dyn LaunchedProcess>> {
            let image = backup_path(exe.parent().expect("installation directory"))
                .join(exe.file_name().expect("executable name"));
            *self.lock.borrow_mut() = Some(lock_without_sharing(&image));
            crate::handshake::write(handshake, &self.version, Some(operation)).unwrap();
            Ok(Box::new(FakeProcess))
        }
    }
    let locking = LockingLauncher {
        lock: RefCell::new(None),
        version: "1.1.0".into(),
    };
    let outcome = run_with(&target, &staged, &handshake, &locking, &NoHooks).unwrap();
    assert!(!outcome.backup_removed);
    let backup = backup_path(&target);
    assert!(backup.exists());
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");

    drop(locking.lock.borrow_mut().take());
    let recovered = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(
        recovered,
        RecoveryOutcome::Completed {
            backup_removed: true,
            ..
        }
    ));
    assert!(!backup.exists());
    assert_state_cleared(&target);
}

#[cfg(windows)]
#[test]
fn a_locked_completed_backup_does_not_block_the_next_update() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("VoxHold");
    let backup = backup_path(&target);
    // The previous operation was confirmed by the new version, but its backup
    // is still locked by another process.
    install_package(&target, "1.1.0", b"confirmed exe");
    install_package(&backup, "1.0.0", b"old exe");
    let completed = InstallState {
        schema: state::STATE_SCHEMA,
        operation_id: "previous-operation".into(),
        stage: OperationStage::Completed,
        install_dir: target.clone(),
        backup_dir: backup.clone(),
        staging_dir: state::operation_staging_path(&target, "previous-operation"),
        handshake_path: temp.path().join("previous.handshake"),
        version: "1.1.0".into(),
        previous_version: Some("1.0.0".into()),
        had_install: true,
        created_at: 1,
    };
    state::save(&completed).unwrap();
    let _lock = lock_without_sharing(&backup.join("Voxhold.exe"));

    let recovered = recover_install(&target, RecoveryMode::Updater).unwrap();
    assert!(matches!(
        recovered,
        RecoveryOutcome::Completed {
            backup_removed: false,
            ..
        }
    ));
    assert!(state::state_path(&target).exists());
    assert!(backup.exists());

    // A proven completion is not "unresolved": the next update must start and
    // leave the unknown backup untouched under its canonical name.
    let staged = staged_package(temp.path());
    let outcome = run_with(
        &target,
        &staged,
        &temp.path().join("ready.handshake"),
        &launcher(true),
        &NoHooks,
    )
    .unwrap();
    assert!(outcome.backup_removed);
    assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");
    assert!(backup.exists(), "the old completed backup is preserved");
    drop(_lock);
    assert_eq!(
        fs::read(backup.join("Voxhold.exe")).unwrap(),
        b"old exe",
        "the old completed backup is preserved"
    );
    assert_state_cleared(&target);
}
