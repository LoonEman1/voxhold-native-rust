//! One recovery algorithm for the updater and the application start.
//!
//! Both callers read the same [`InstallState`] and the same directories. The
//! application uses [`RecoveryMode::Observer`]: it may remove a backup whose
//! operation reached [`OperationStage::Completed`], but it never renames an
//! installation. The helper uses [`RecoveryMode::Updater`] after the
//! application exited and may finish a rollback.
//!
//! The default answer to every unknown is "keep the backup". A backup is
//! removed only when the state proves that the new version confirmed startup
//! (or after a rollback restored the previous installation and the backup was
//! consumed by the rename).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Result, UpdateError};
use crate::fsutil;
use crate::handshake;
use crate::package::read_package_json_from_dir;
use crate::state::{self, InstallState, OperationStage, StateLoad};

/// Time allowed for removing a backup before a locked file defers cleanup.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
/// Time allowed for removing an operation's staging copy.
const STAGING_TIMEOUT: Duration = Duration::from_secs(1);

/// Who is asking for recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryMode {
    /// The running application: read, complete a confirmed operation, never
    /// rename the installation or the backup.
    Observer,
    /// The helper after the application exited: may finish a rollback.
    Updater,
}

/// What recovery found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// Nothing to recover.
    Clean,
    /// The new version confirmed startup. `backup_removed` is `false` when the
    /// backup was locked; the state stays so a later run retries.
    Completed {
        operation_id: String,
        backup_removed: bool,
    },
    /// The previous installation is back in place.
    RolledBack { operation_id: String },
    /// An interrupted operation exists but only the helper may touch it.
    NeedsUpdater {
        operation_id: Option<String>,
        stage: Option<OperationStage>,
    },
    /// A backup exists without usable state. It is kept untouched.
    Ambiguous { backup: Option<PathBuf> },
    /// The state file or its paths cannot be trusted. The backup is kept.
    Corrupt { backup: Option<PathBuf> },
}

impl RecoveryOutcome {
    /// `true` when the caller should surface a diagnostic.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            Self::NeedsUpdater { .. } | Self::Ambiguous { .. } | Self::Corrupt { .. }
        )
    }

    /// `true` when a backup is known to remain.
    pub fn backup_kept(&self) -> bool {
        matches!(
            self,
            Self::Completed {
                backup_removed: false,
                ..
            } | Self::NeedsUpdater { .. }
                | Self::Ambiguous { .. }
                | Self::Corrupt { .. }
        )
    }
}

/// Resolve the installation state before a start or a new update.
pub fn recover_install(install_dir: &Path, mode: RecoveryMode) -> Result<RecoveryOutcome> {
    match state::load(install_dir) {
        StateLoad::Missing => recover_without_state(install_dir, mode),
        StateLoad::Corrupt(_) => Ok(RecoveryOutcome::Corrupt {
            backup: find_backup(install_dir),
        }),
        StateLoad::Valid(state) => recover_with_state(&state, mode),
    }
}

fn recover_without_state(install_dir: &Path, mode: RecoveryMode) -> Result<RecoveryOutcome> {
    let Some(backup) = find_backup(install_dir) else {
        return Ok(RecoveryOutcome::Clean);
    };
    if install_dir.exists() {
        // The old code deleted this directory as soon as the target existed.
        // Whether the target is complete cannot be proven, so keep it.
        return Ok(RecoveryOutcome::Ambiguous {
            backup: Some(backup),
        });
    }
    match mode {
        RecoveryMode::Observer => Ok(RecoveryOutcome::NeedsUpdater {
            operation_id: None,
            stage: None,
        }),
        RecoveryMode::Updater => {
            fsutil::rename_retry(&backup, install_dir)?;
            Ok(RecoveryOutcome::RolledBack {
                operation_id: "legacy".into(),
            })
        }
    }
}

fn recover_with_state(state: &InstallState, mode: RecoveryMode) -> Result<RecoveryOutcome> {
    match state.stage {
        OperationStage::Completed => finish_completed(state),
        OperationStage::Prepared => recover_prepared(state, mode),
        OperationStage::OldRenamed => recover_old_renamed(state, mode),
        OperationStage::NewInPlace => recover_new_in_place(state, mode),
        OperationStage::RollingBack => recover_rolling_back(state, mode),
    }
}

fn finish_completed(state: &InstallState) -> Result<RecoveryOutcome> {
    // Completion is only trustworthy while the installation it confirmed is
    // still the one in place. A missing or mismatched target means the state
    // and the disk disagree, so the backup stays.
    if target_version(&state.install_dir).as_deref() != Some(state.version.as_str()) {
        return Ok(RecoveryOutcome::Ambiguous {
            backup: find_backup(&state.install_dir),
        });
    }
    let _ = fsutil::remove_dir_with_timeout(&state.staging_dir, STAGING_TIMEOUT);
    if state.backup_dir.exists()
        && fsutil::remove_dir_with_timeout(&state.backup_dir, CLEANUP_TIMEOUT).is_err()
    {
        // A locked backup is not a failed update. Keep the state so the next
        // start retries the cleanup.
        return Ok(RecoveryOutcome::Completed {
            operation_id: state.operation_id.clone(),
            backup_removed: false,
        });
    }
    let _ = state::clear(&state.install_dir);
    Ok(RecoveryOutcome::Completed {
        operation_id: state.operation_id.clone(),
        backup_removed: true,
    })
}

fn recover_prepared(state: &InstallState, mode: RecoveryMode) -> Result<RecoveryOutcome> {
    if !state.had_install {
        // A fresh installation records `OldRenamed` before its only rename, so
        // `Prepared` proves that nothing was moved: there is no previous
        // installation to protect. Keeping this state would block every later
        // update (`apply` refuses a still-present state), so drop the staged
        // copy instead. No installation directory is renamed.
        clear_operation(state)?;
        return Ok(RecoveryOutcome::RolledBack {
            operation_id: state.operation_id.clone(),
        });
    }
    if state.install_dir.exists() {
        // The swap never started; the old installation is intact. Removing the
        // staged copy and the state is safe for the observer too: no directory
        // of the installation is renamed.
        clear_operation(state)?;
        return Ok(RecoveryOutcome::RolledBack {
            operation_id: state.operation_id.clone(),
        });
    }
    if state.backup_dir.exists() {
        if mode == RecoveryMode::Observer {
            return Ok(RecoveryOutcome::NeedsUpdater {
                operation_id: Some(state.operation_id.clone()),
                stage: Some(state.stage),
            });
        }
        // The rename happened but the state was not advanced: restore.
        fsutil::rename_retry(&state.backup_dir, &state.install_dir)?;
        clear_operation(state)?;
        return Ok(RecoveryOutcome::RolledBack {
            operation_id: state.operation_id.clone(),
        });
    }
    Ok(RecoveryOutcome::Ambiguous {
        backup: find_backup(&state.install_dir),
    })
}

fn recover_old_renamed(state: &InstallState, mode: RecoveryMode) -> Result<RecoveryOutcome> {
    if state.install_dir.exists() {
        let version = target_version(&state.install_dir);
        if version.as_deref() == Some(state.version.as_str()) {
            // The staged directory was renamed into place before the state
            // could be advanced. Treat it as a new installation.
            return recover_new_in_place(state, mode);
        }
        if version.is_some() && version.as_deref() == state.previous_version.as_deref() {
            // A rollback already restored the old installation.
            clear_operation(state)?;
            return Ok(RecoveryOutcome::RolledBack {
                operation_id: state.operation_id.clone(),
            });
        }
        return Ok(RecoveryOutcome::Ambiguous {
            backup: find_backup(&state.install_dir),
        });
    }
    if !state.backup_dir.exists() {
        return Ok(RecoveryOutcome::Ambiguous {
            backup: find_backup(&state.install_dir),
        });
    }
    if mode == RecoveryMode::Observer {
        return Ok(RecoveryOutcome::NeedsUpdater {
            operation_id: Some(state.operation_id.clone()),
            stage: Some(state.stage),
        });
    }
    fsutil::rename_retry(&state.backup_dir, &state.install_dir)?;
    clear_operation(state)?;
    Ok(RecoveryOutcome::RolledBack {
        operation_id: state.operation_id.clone(),
    })
}

fn recover_new_in_place(state: &InstallState, mode: RecoveryMode) -> Result<RecoveryOutcome> {
    if state.install_dir.exists() {
        if state.backup_dir.exists() {
            if target_version(&state.install_dir).as_deref() == Some(state.version.as_str())
                && handshake::confirms(&state.handshake_path, &state.operation_id)
            {
                return finish_confirmed(state);
            }
            if mode == RecoveryMode::Observer {
                return Ok(RecoveryOutcome::NeedsUpdater {
                    operation_id: Some(state.operation_id.clone()),
                    stage: Some(state.stage),
                });
            }
            if target_version(&state.install_dir).as_deref() != Some(state.version.as_str()) {
                // The directory in place is not the version this operation
                // wrote. Do not delete an installation we cannot identify.
                return Ok(RecoveryOutcome::Ambiguous {
                    backup: Some(state.backup_dir.clone()),
                });
            }
            return rollback_to_backup(state);
        }
        // No backup: the operation either completed already or the backup was
        // removed outside this protocol. Do not guess from existence alone.
        let version = target_version(&state.install_dir);
        if version.as_deref() == Some(state.version.as_str()) {
            let _ = state::clear(&state.install_dir);
            return Ok(RecoveryOutcome::Completed {
                operation_id: state.operation_id.clone(),
                backup_removed: true,
            });
        }
        if version.is_some() && version.as_deref() == state.previous_version.as_deref() {
            let _ = state::clear(&state.install_dir);
            return Ok(RecoveryOutcome::RolledBack {
                operation_id: state.operation_id.clone(),
            });
        }
        return Ok(RecoveryOutcome::Ambiguous { backup: None });
    }
    if state.backup_dir.exists() {
        if mode == RecoveryMode::Observer {
            return Ok(RecoveryOutcome::NeedsUpdater {
                operation_id: Some(state.operation_id.clone()),
                stage: Some(state.stage),
            });
        }
        return rollback_to_backup(state);
    }
    Ok(RecoveryOutcome::Ambiguous {
        backup: find_backup(&state.install_dir),
    })
}

fn recover_rolling_back(state: &InstallState, mode: RecoveryMode) -> Result<RecoveryOutcome> {
    if state.backup_dir.exists() {
        if mode == RecoveryMode::Observer {
            return Ok(RecoveryOutcome::NeedsUpdater {
                operation_id: Some(state.operation_id.clone()),
                stage: Some(state.stage),
            });
        }
        return rollback_to_backup(state);
    }
    // The backup was consumed by the rollback rename.
    if let Some(version) = target_version(&state.install_dir) {
        let _ = fsutil::remove_dir_with_timeout(&state.staging_dir, STAGING_TIMEOUT);
        if Some(version.as_str()) == state.previous_version.as_deref() {
            let _ = state::clear(&state.install_dir);
            return Ok(RecoveryOutcome::RolledBack {
                operation_id: state.operation_id.clone(),
            });
        }
        if version == state.version {
            let _ = state::clear(&state.install_dir);
            return Ok(RecoveryOutcome::Completed {
                operation_id: state.operation_id.clone(),
                backup_removed: true,
            });
        }
    }
    Ok(RecoveryOutcome::Ambiguous {
        backup: find_backup(&state.install_dir),
    })
}

/// Mark a handshake-confirmed operation complete and try to remove the backup.
fn finish_confirmed(state: &InstallState) -> Result<RecoveryOutcome> {
    let mut completed = state.clone();
    completed.stage = OperationStage::Completed;
    // Persist completion before deleting anything: a crash between these
    // steps is retried by the next run instead of rolling back.
    state::save(&completed)?;
    finish_completed(&completed)
}

/// Move the backup back and remove the failed new installation.
fn rollback_to_backup(state: &InstallState) -> Result<RecoveryOutcome> {
    let mut rolling = state.clone();
    rolling.stage = OperationStage::RollingBack;
    let _ = state::save(&rolling);
    if state.install_dir.exists() {
        let version = target_version(&state.install_dir);
        if version.as_deref() != Some(state.version.as_str()) {
            return Err(UpdateError::Apply(
                "refusing to roll back an installation with an unexpected version".into(),
            ));
        }
        fsutil::remove_dir_retry(&state.install_dir)?;
    }
    fsutil::rename_retry(&state.backup_dir, &state.install_dir)?;
    clear_operation(&rolling)?;
    Ok(RecoveryOutcome::RolledBack {
        operation_id: state.operation_id.clone(),
    })
}

/// Remove the staging copy and the state after the old installation is safe.
fn clear_operation(state: &InstallState) -> Result<()> {
    let _ = fsutil::remove_dir_with_timeout(&state.staging_dir, STAGING_TIMEOUT);
    state::clear(&state.install_dir)
}

/// Roll back an operation that failed inside [`crate::apply`].
///
/// Unlike [`recover_install`] this starts from memory, but it performs the same
/// stage checks so a failed preparation never deletes an intact installation.
pub(crate) fn rollback_operation(state: &InstallState) -> Result<()> {
    match state.stage {
        OperationStage::Prepared => {
            if !state.install_dir.exists() && state.backup_dir.exists() {
                fsutil::rename_retry(&state.backup_dir, &state.install_dir)?;
            }
            let _ = fsutil::remove_dir_with_timeout(&state.staging_dir, STAGING_TIMEOUT);
            state::clear(&state.install_dir)
        }
        OperationStage::OldRenamed | OperationStage::NewInPlace | OperationStage::RollingBack => {
            let mut rolling = state.clone();
            rolling.stage = OperationStage::RollingBack;
            let _ = state::save(&rolling);
            if state.install_dir.exists() {
                if target_version(&state.install_dir).as_deref() == Some(state.version.as_str()) {
                    fsutil::remove_dir_retry(&state.install_dir)?;
                }
            }
            if state.install_dir.exists() {
                return Err(UpdateError::Apply(
                    "cannot roll back while an unidentified installation is in place".into(),
                ));
            }
            if state.backup_dir.exists() {
                fsutil::rename_retry(&state.backup_dir, &state.install_dir)?;
            }
            clear_operation(&rolling)
        }
        OperationStage::Completed => finish_completed(state).map(|_| ()),
    }
}

fn target_version(install_dir: &Path) -> Option<String> {
    read_package_json_from_dir(install_dir)
        .ok()
        .map(|index| index.version)
}

fn find_backup(install_dir: &Path) -> Option<PathBuf> {
    let canonical = state::backup_path(install_dir);
    if canonical.is_dir() {
        return Some(canonical);
    }
    let name = install_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{name}{}", state::BACKUP_SUFFIX).to_lowercase();
    let parent = install_dir.parent()?;
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            name == prefix
                || name
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| rest.starts_with('-'))
        })
        .map(|entry| entry.path())
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}
