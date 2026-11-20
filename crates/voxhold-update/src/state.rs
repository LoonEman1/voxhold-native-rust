//! Durable operation state for an installation swap.
//!
//! The state file lives next to the installation
//! (`<install>.voxhold-state.json`) and is written atomically before the first
//! rename. It records the operation id, the exact backup and staging
//! directories and the last completed step. Recovery never guesses: a missing,
//! corrupt or foreign state keeps the backup and leaves the decision to the
//! helper after the application has exited.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Result, UpdateError};

/// Suffix of the backup directory next to the installation.
pub const BACKUP_SUFFIX: &str = ".voxhold-backup";
/// Suffix of the per-operation staging directory next to the installation.
pub const STAGING_SUFFIX: &str = ".voxhold-next-";
/// Suffix of the state file next to the installation.
pub const STATE_SUFFIX: &str = ".voxhold-state.json";
/// Schema accepted by this build.
pub const STATE_SCHEMA: u32 = 1;

/// Last durable step of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStage {
    /// The new installation is staged and verified, the old one is untouched.
    Prepared,
    /// The old installation was renamed to the backup.
    OldRenamed,
    /// The staged installation was renamed into place and may be started.
    NewInPlace,
    /// The new version confirmed startup; the backup may be removed.
    Completed,
    /// A rollback started and must be finished before anything else.
    RollingBack,
}

impl OperationStage {
    /// Short stable name for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::OldRenamed => "old-renamed",
            Self::NewInPlace => "new-in-place",
            Self::Completed => "completed",
            Self::RollingBack => "rolling-back",
        }
    }
}

/// One installation swap recorded on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallState {
    pub schema: u32,
    pub operation_id: String,
    pub stage: OperationStage,
    pub install_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub staging_dir: PathBuf,
    pub handshake_path: PathBuf,
    pub version: String,
    #[serde(default)]
    pub previous_version: Option<String>,
    pub had_install: bool,
    pub created_at: u64,
}

impl InstallState {
    /// `true` when the recorded paths belong to this installation.
    ///
    /// Recovery deletes and renames directories, so a state file that points
    /// anywhere else is treated as corrupt instead of being obeyed.
    pub fn is_valid_for(&self, install_dir: &Path) -> bool {
        if self.schema != STATE_SCHEMA || self.operation_id.trim().is_empty() {
            return false;
        }
        if !is_plain_operation_id(&self.operation_id) {
            return false;
        }
        if semver::Version::parse(&self.version).is_err() {
            return false;
        }
        if let Some(previous) = &self.previous_version {
            if semver::Version::parse(previous).is_err() {
                return false;
            }
        }
        if !same_path(&self.install_dir, install_dir) {
            return false;
        }
        if !is_sibling_artifact(&self.backup_dir, install_dir, BACKUP_SUFFIX) {
            return false;
        }
        if !is_sibling_artifact(&self.staging_dir, install_dir, STAGING_SUFFIX) {
            return false;
        }
        self.handshake_path.is_absolute()
    }
}

/// Result of reading the state file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateLoad {
    Missing,
    Valid(InstallState),
    /// The file exists but cannot be used safely.
    Corrupt(String),
}

/// Fresh operation id.
pub fn new_operation_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Path of the backup directory of an installation.
pub fn backup_path(install_dir: &Path) -> PathBuf {
    sibling(install_dir, BACKUP_SUFFIX)
}

/// Backup path used when the canonical one is already occupied by an
/// unrecoverable leftover from an older build.
pub fn operation_backup_path(install_dir: &Path, operation_id: &str) -> PathBuf {
    sibling(install_dir, &format!("{BACKUP_SUFFIX}-{operation_id}"))
}

/// Sibling staging directory for one operation.
pub fn operation_staging_path(install_dir: &Path, operation_id: &str) -> PathBuf {
    sibling(install_dir, &format!("{STAGING_SUFFIX}{operation_id}"))
}

/// Path of the state file of an installation.
pub fn state_path(install_dir: &Path) -> PathBuf {
    sibling(install_dir, STATE_SUFFIX)
}

/// Read the state file. I/O failures other than "not found" are reported as
/// corrupt so callers stay conservative.
pub fn load(install_dir: &Path) -> StateLoad {
    let path = state_path(install_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return StateLoad::Missing,
        Err(error) => return StateLoad::Corrupt(format!("cannot read state: {}", error.kind())),
    };
    match serde_json::from_slice::<InstallState>(&bytes) {
        Ok(state) if state.is_valid_for(install_dir) => StateLoad::Valid(state),
        Ok(_) => StateLoad::Corrupt("state does not belong to this installation".into()),
        Err(error) => StateLoad::Corrupt(format!("state is not valid JSON: {error}")),
    }
}

/// Atomically persist the state before the next rename.
pub fn save(state: &InstallState) -> Result<()> {
    let path = state_path(&state.install_dir);
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state).expect("install state serialization");
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| UpdateError::io(&path, error))
}

/// Remove the state file after the operation reached a stable state.
pub fn clear(install_dir: &Path) -> Result<()> {
    let path = state_path(install_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(UpdateError::io(&path, error)),
    }
}

/// Current UNIX time in seconds for diagnostics.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn sibling(install_dir: &Path, suffix: &str) -> PathBuf {
    let name = install_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "VoxHold".into());
    install_dir.with_file_name(format!("{name}{suffix}"))
}

fn is_sibling_artifact(candidate: &Path, install_dir: &Path, suffix: &str) -> bool {
    let name = install_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(candidate_name) = candidate.file_name() else {
        return false;
    };
    if !same_path(
        candidate.parent().unwrap_or_else(|| Path::new("")),
        install_dir.parent().unwrap_or_else(|| Path::new("")),
    ) {
        return false;
    }
    let prefix = format!("{name}{suffix}");
    // `STAGING_SUFFIX` already ends with the separator; the backup suffix gets
    // the operation id appended.
    candidate_name
        .to_string_lossy()
        .to_lowercase()
        .starts_with(&prefix.to_lowercase())
}

fn is_plain_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn same_path(left: &Path, right: &Path) -> bool {
    let canonical_left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let canonical_right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    #[cfg(windows)]
    {
        canonical_left
            .to_string_lossy()
            .eq_ignore_ascii_case(&canonical_right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        canonical_left == canonical_right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(root: &Path) -> InstallState {
        InstallState {
            schema: STATE_SCHEMA,
            operation_id: "0123456789abcdef".into(),
            stage: OperationStage::Prepared,
            install_dir: root.join("VoxHold"),
            backup_dir: backup_path(&root.join("VoxHold")),
            staging_dir: operation_staging_path(&root.join("VoxHold"), "0123456789abcdef"),
            handshake_path: root.join("updates").join("handshake.ok"),
            version: "1.1.0".into(),
            previous_version: Some("1.0.0".into()),
            had_install: true,
            created_at: 1,
        }
    }

    #[test]
    fn saves_and_loads_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("VoxHold");
        std::fs::create_dir_all(&install).unwrap();
        let state = sample(temp.path());
        save(&state).unwrap();
        assert_eq!(load(&install), StateLoad::Valid(state.clone()));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 2);
        clear(&install).unwrap();
        assert_eq!(load(&install), StateLoad::Missing);
    }

    #[test]
    fn corrupt_and_foreign_state_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("VoxHold");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(state_path(&install), b"not json").unwrap();
        assert!(matches!(load(&install), StateLoad::Corrupt(_)));

        let mut state = sample(temp.path());
        state.backup_dir = temp.path().join("elsewhere");
        save(&state).unwrap();
        assert!(matches!(load(&install), StateLoad::Corrupt(_)));
    }

    #[test]
    fn operation_paths_stay_next_to_the_installation() {
        let install = Path::new(r"C:\Users\test\Programs\VoxHold");
        assert_eq!(
            operation_staging_path(install, "abc"),
            Path::new(r"C:\Users\test\Programs\VoxHold.voxhold-next-abc")
        );
        assert_eq!(
            operation_backup_path(install, "abc"),
            Path::new(r"C:\Users\test\Programs\VoxHold.voxhold-backup-abc")
        );
    }
}
