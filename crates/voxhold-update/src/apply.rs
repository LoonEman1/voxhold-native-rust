//! Applying a staged update from the helper process.
//!
//! The protocol is:
//!
//! 1. recover any earlier operation;
//! 2. copy the verified package into a sibling staging directory on the same
//!    volume and migrate user files there *before* anything is renamed;
//! 3. persist [`InstallState`] with a fresh operation id;
//! 4. rename the old installation to the backup and the staging directory into
//!    place, persisting each step;
//! 5. start the new version and wait for a handshake carrying that operation
//!    id;
//! 6. persist completion and only then remove the backup.
//!
//! A failure before the handshake rolls back. A failure after the handshake
//! never deletes the running installation; the durable state lets the next
//! helper run finish the cleanup.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::error::{Result, UpdateError};
use crate::fsutil;
use crate::handshake;
use crate::package::{read_package_json_from_dir, sha256_file, PackageIndex, PACKAGE_JSON};
use crate::recovery::{self, RecoveryMode, RecoveryOutcome};
use crate::state::{self, InstallState, OperationStage, StateLoad};

/// Default time the new version has to confirm startup.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Suffix of the backup directory next to the installation.
pub const BACKUP_SUFFIX: &str = state::BACKUP_SUFFIX;
/// Argument passed to the new executable so it can confirm startup.
pub const HANDSHAKE_ARG: &str = handshake::ARG;
/// Argument carrying the operation id to the new executable.
pub const OPERATION_ARG: &str = handshake::OPERATION_ARG;
/// Name of a helper image relocated into the staging directory.
pub const HELPER_FILE: &str = "helper.exe";
/// Time allowed for removing the backup after a successful handshake.
const BACKUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
/// Time allowed for removing the sibling staging copy.
const STAGING_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

/// Result of a successful apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// `true` when the previous installation backup is gone.
    ///
    /// `false` means the backup was still locked (for example a file held by
    /// another process); a later recovery run removes it.
    pub backup_removed: bool,
}

/// A process started after the swap.
pub trait LaunchedProcess {
    fn try_wait(&mut self) -> Result<Option<i32>>;
    fn kill(&mut self);
}

/// Starts the new executable.
pub trait UpdateLauncher {
    /// Start `exe`, ask it to confirm `operation` through `handshake`.
    fn launch(
        &self,
        exe: &Path,
        handshake: &Path,
        operation: &str,
    ) -> Result<Box<dyn LaunchedProcess>>;
}

/// Launcher used in production.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessLauncher;

impl UpdateLauncher for ProcessLauncher {
    fn launch(
        &self,
        exe: &Path,
        handshake: &Path,
        operation: &str,
    ) -> Result<Box<dyn LaunchedProcess>> {
        let mut command = Command::new(exe);
        command
            .arg(HANDSHAKE_ARG)
            .arg(handshake)
            .arg(OPERATION_ARG)
            .arg(operation);
        if let Some(parent) = exe.parent() {
            command.current_dir(parent);
        }
        let child = command
            .spawn()
            .map_err(|error| UpdateError::Apply(format!("cannot start new version: {error}")))?;
        Ok(Box::new(ChildProcess { child }))
    }
}

struct ChildProcess {
    child: Child,
}

impl LaunchedProcess for ChildProcess {
    fn try_wait(&mut self) -> Result<Option<i32>> {
        let status = self
            .child
            .try_wait()
            .map_err(|error| UpdateError::Apply(format!("cannot poll new version: {error}")))?;
        Ok(status.map(|status| status.code().unwrap_or(-1)))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Apply a staged update.
pub fn apply_staged(
    target_dir: &Path,
    staged_app: &Path,
    exe_name: &str,
    handshake_path: &Path,
) -> Result<ApplyOutcome> {
    apply_staged_with(
        target_dir,
        staged_app,
        exe_name,
        handshake_path,
        &ProcessLauncher,
        HANDSHAKE_TIMEOUT,
    )
}

/// Apply with an explicit launcher and timeout (used by tests).
pub fn apply_staged_with(
    target_dir: &Path,
    staged_app: &Path,
    exe_name: &str,
    handshake_path: &Path,
    launcher: &dyn UpdateLauncher,
    timeout: Duration,
) -> Result<ApplyOutcome> {
    apply_staged_with_hooks(
        target_dir,
        staged_app,
        exe_name,
        handshake_path,
        launcher,
        timeout,
        &NoHooks,
    )
}

/// Steps a test can stop at to emulate a killed helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterruptPoint {
    Staged,
    Verified,
    Prepared,
    OldRenamed,
    NewInPlace,
    Launched,
    Confirmed,
}

/// Test seam. Production uses [`NoHooks`].
pub(crate) trait ApplyHooks {
    fn interrupt(&self, _point: InterruptPoint) -> bool {
        false
    }
}

pub(crate) struct NoHooks;

impl ApplyHooks for NoHooks {}

pub(crate) fn apply_staged_with_hooks(
    target_dir: &Path,
    staged_app: &Path,
    exe_name: &str,
    handshake_path: &Path,
    launcher: &dyn UpdateLauncher,
    timeout: Duration,
    hooks: &dyn ApplyHooks,
) -> Result<ApplyOutcome> {
    let new_index = read_package_json_from_dir(staged_app)?;
    let exe = staged_app.join(exe_name);
    if !exe.is_file() {
        return Err(UpdateError::NotStaged(exe));
    }

    // Resolve a previous operation first. A state that cannot be resolved is
    // never overwritten: overwriting it would destroy the only proof that the
    // backup is still needed.
    let recovery = recovery::recover_install(target_dir, RecoveryMode::Updater)?;
    match recovery {
        RecoveryOutcome::Clean
        | RecoveryOutcome::Completed { .. }
        | RecoveryOutcome::RolledBack { .. } => {}
        // Without a state file the backup is an unknown legacy leftover, not
        // an unfinished operation: there is no proof to preserve and the new
        // operation uses its own backup name. With a state, ambiguity refuses.
        RecoveryOutcome::Ambiguous { .. }
            if matches!(state::load(target_dir), StateLoad::Missing) => {}
        outcome => {
            return Err(UpdateError::InvalidState(format!(
                "a previous update is still unresolved ({outcome:?}); its backup was kept"
            )));
        }
    }
    match state::load(target_dir) {
        StateLoad::Missing => {}
        // A completed operation is proven, not unresolved. Its backup can
        // still be locked; clearing the state unblocks the next update. The
        // backup stays untouched on disk and becomes an unknown legacy backup,
        // so `choose_backup_path` gives the new operation its own name.
        StateLoad::Valid(state) if state.stage == OperationStage::Completed => {
            state::clear(target_dir)?;
        }
        StateLoad::Valid(_) | StateLoad::Corrupt(_) => {
            return Err(UpdateError::InvalidState(
                "a previous update is still unresolved; its backup was kept".into(),
            ));
        }
    }

    let operation_id = state::new_operation_id();
    let staging_dir = state::operation_staging_path(target_dir, &operation_id);
    let old_index = read_package_json_from_dir(target_dir).ok();
    let had_target = target_dir.exists();
    let backup_dir = choose_backup_path(target_dir, &operation_id);
    let mut state = InstallState {
        schema: state::STATE_SCHEMA,
        operation_id: operation_id.clone(),
        stage: OperationStage::Prepared,
        install_dir: target_dir.to_path_buf(),
        backup_dir,
        staging_dir: staging_dir.clone(),
        handshake_path: handshake_path.to_path_buf(),
        version: new_index.version.clone(),
        previous_version: old_index.as_ref().map(|index| index.version.clone()),
        had_install: had_target,
        created_at: state::now_unix(),
    };

    let mut interrupted = false;
    let mut applied: Option<InstallState> = None;

    macro_rules! checkpoint {
        ($point:expr) => {
            if hooks.interrupt($point) {
                interrupted = true;
                return Err(UpdateError::Apply("apply interrupted by test hook".into()));
            }
        };
    }

    let result = (|| -> Result<()> {
        copy_payload(staged_app, &staging_dir, &new_index)?;
        checkpoint!(InterruptPoint::Staged);
        if had_target {
            migrate_user_files(target_dir, &staging_dir, old_index.as_ref(), &new_index)?;
        }
        verify_payload(&staging_dir, &new_index)?;
        checkpoint!(InterruptPoint::Verified);

        // Durable before the first rename.
        state::save(&state)?;
        applied = Some(state.clone());
        checkpoint!(InterruptPoint::Prepared);

        if had_target {
            fsutil::rename_retry(target_dir, &state.backup_dir)?;
        }
        state.stage = OperationStage::OldRenamed;
        state::save(&state)?;
        applied = Some(state.clone());
        checkpoint!(InterruptPoint::OldRenamed);

        fsutil::rename_retry(&staging_dir, target_dir)?;
        state.stage = OperationStage::NewInPlace;
        state::save(&state)?;
        applied = Some(state.clone());
        checkpoint!(InterruptPoint::NewInPlace);

        let process = launch_new(
            target_dir,
            exe_name,
            handshake_path,
            &operation_id,
            launcher,
        )?;
        checkpoint!(InterruptPoint::Launched);
        wait_for_confirmation(process, handshake_path, &operation_id, timeout)?;
        checkpoint!(InterruptPoint::Confirmed);
        Ok(())
    })();

    match result {
        Ok(()) => {
            let applied = applied.expect("state is persisted before launch");
            finish_success(&applied, target_dir, staged_app)
        }
        Err(error) if interrupted => Err(error),
        Err(error) => {
            let rollback = match applied.as_ref() {
                Some(applied) => recovery::rollback_operation(applied),
                None => {
                    let _ = fsutil::remove_dir_with_timeout(&staging_dir, STAGING_CLEANUP_TIMEOUT);
                    Ok(())
                }
            };
            let _ = fsutil::remove_dir_with_timeout(staged_app, STAGING_CLEANUP_TIMEOUT);
            match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(UpdateError::Apply(format!(
                    "{error}; rollback failed: {rollback_error}"
                ))),
            }
        }
    }
}

fn copy_payload(staged_app: &Path, staging: &Path, new_index: &PackageIndex) -> Result<()> {
    if staging.exists() {
        fsutil::remove_dir_retry(staging)?;
    }
    std::fs::create_dir_all(staging).map_err(|error| UpdateError::io(staging, error))?;
    for file in &new_index.files {
        let source = staged_app.join(Path::new(&file.path));
        let destination = staging.join(Path::new(&file.path));
        copy_file(&source, &destination)?;
    }
    copy_file(&staged_app.join(PACKAGE_JSON), &staging.join(PACKAGE_JSON))
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| UpdateError::io(parent, error))?;
    }
    std::fs::copy(source, destination).map_err(|error| UpdateError::io(destination, error))?;
    Ok(())
}

fn migrate_user_files(
    source: &Path,
    staging: &Path,
    old_index: Option<&PackageIndex>,
    new_index: &PackageIndex,
) -> Result<()> {
    let old_files: BTreeSet<String> = old_index
        .map(|index| {
            index
                .files
                .iter()
                .map(|file| file.path.to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let new_files: BTreeSet<String> = new_index
        .files
        .iter()
        .map(|file| file.path.to_lowercase())
        .collect();
    copy_user_files(source, staging, &old_files, &new_files)
}

fn copy_user_files(
    source: &Path,
    staging: &Path,
    old_files: &BTreeSet<String>,
    new_files: &BTreeSet<String>,
) -> Result<()> {
    let mut stack = vec![source.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in
            std::fs::read_dir(&current).map_err(|error| UpdateError::io(&current, error))?
        {
            let entry = entry.map_err(|error| UpdateError::io(&current, error))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| UpdateError::io(&path, error))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let relative = path
                .strip_prefix(source)
                .unwrap_or(&path)
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let key = relative.to_lowercase();
            if key == PACKAGE_JSON || old_files.contains(&key) || new_files.contains(&key) {
                continue;
            }
            copy_file(&path, &staging.join(Path::new(&relative)))?;
        }
    }
    Ok(())
}

fn verify_payload(staging: &Path, new_index: &PackageIndex) -> Result<()> {
    for file in &new_index.files {
        let path = staging.join(Path::new(&file.path));
        if sha256_file(&path)? != file.sha256 {
            return Err(UpdateError::HashMismatch { path });
        }
    }
    if !staging.join(PACKAGE_JSON).is_file() {
        return Err(UpdateError::NotStaged(staging.to_path_buf()));
    }
    Ok(())
}

fn launch_new(
    target_dir: &Path,
    exe_name: &str,
    handshake_path: &Path,
    operation_id: &str,
    launcher: &dyn UpdateLauncher,
) -> Result<Box<dyn LaunchedProcess>> {
    if let Some(parent) = handshake_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| UpdateError::io(parent, error))?;
    }
    let _ = std::fs::remove_file(handshake_path);
    let exe = target_dir.join(exe_name);
    launcher.launch(&exe, handshake_path, operation_id)
}

fn wait_for_confirmation(
    mut process: Box<dyn LaunchedProcess>,
    handshake_path: &Path,
    operation_id: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if handshake::confirms(handshake_path, operation_id) {
            return Ok(());
        }
        match process.try_wait() {
            Ok(Some(code)) => {
                // The child may have written the handshake after the first check.
                if handshake::confirms(handshake_path, operation_id) {
                    return Ok(());
                }
                return Err(UpdateError::Apply(format!(
                    "new version exited with code {code} before confirming startup"
                )));
            }
            Ok(None) => {}
            Err(error) => {
                process.kill();
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            process.kill();
            return Err(UpdateError::LaunchTimeout(timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn finish_success(
    applied: &InstallState,
    target_dir: &Path,
    staged_app: &Path,
) -> Result<ApplyOutcome> {
    let mut completed = applied.clone();
    completed.stage = OperationStage::Completed;
    // The new version is already running. If completion cannot be persisted,
    // leave the handshake and the previous state for the next recovery run
    // instead of rolling back a working installation.
    state::save(&completed)?;
    let backup_removed =
        fsutil::remove_dir_with_timeout(&completed.backup_dir, BACKUP_CLEANUP_TIMEOUT).is_ok();
    if backup_removed {
        let _ = state::clear(target_dir);
    }
    let _ = fsutil::remove_dir_with_timeout(staged_app, STAGING_CLEANUP_TIMEOUT);
    Ok(ApplyOutcome { backup_removed })
}

fn choose_backup_path(target_dir: &Path, operation_id: &str) -> PathBuf {
    let canonical = state::backup_path(target_dir);
    if canonical.exists() {
        // A backup without usable state is kept untouched; this operation uses
        // its own sibling name so it cannot overwrite it.
        state::operation_backup_path(target_dir, operation_id)
    } else {
        canonical
    }
}

/// Path of the backup directory next to the installation.
pub fn backup_path(target_dir: &Path) -> PathBuf {
    state::backup_path(target_dir)
}

/// Where the helper must move itself before touching the installation.
///
/// Windows cannot rename or delete a directory that contains a running image,
/// so a helper started from `<install>` or from the staged `app` directory has
/// to be copied into `<staged>\helper.exe` and restarted with `--detached`.
/// Returns `None` when the helper already runs from a neutral location (or is
/// already the relocated copy).
pub fn helper_relocation_destination(
    current_exe: &Path,
    install_dir: &Path,
    staged_dir: &Path,
) -> Option<PathBuf> {
    let exe = canonical_or(current_exe);
    let install = canonical_or(install_dir);
    let staged = canonical_or(staged_dir);
    let destination = staged_dir.join(HELPER_FILE);
    if canonical_or(&destination) == exe {
        return None;
    }
    (exe.starts_with(&install) || exe.starts_with(&staged)).then_some(destination)
}

/// Copy the running helper image to `destination`.
pub fn relocate_helper(current_exe: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| UpdateError::io(parent, error))?;
    }
    std::fs::copy(current_exe, destination)
        .map(|_| ())
        .map_err(|error| UpdateError::io(destination, error))
}

/// `true` when `exe` is the relocated helper image inside `staged_dir`.
pub fn is_relocated_helper(exe: &Path, staged_dir: &Path) -> bool {
    canonical_or(exe) == canonical_or(&staged_dir.join(HELPER_FILE))
}

fn canonical_or(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
