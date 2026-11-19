//! Staging of an update under `%LOCALAPPDATA%\VoxHold\updates\<version>`.
//!
//! A staged update is never written into the installation directory. It is
//! prepared in its own directory, verified, unpacked into `app/` and marked
//! with `ready.json`. Any failure removes the whole staging directory.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::download::{download, CancelToken, DownloadRequest, ProgressFn, SIZE_MARGIN};
use crate::error::{Result, UpdateError};
use crate::manifest::{Offer, VerifiedManifest, WindowsVersion};
use crate::package::{self, PackageIndex, PackedPackage};

/// Marker written after a successful stage.
pub const READY_FILE: &str = "ready.json";
/// Helper log inside the staging directory.
pub const APPLY_LOG: &str = "apply.log";
/// Downloaded or reconstructed full package.
pub const PACKAGE_FILE: &str = "package.zip";
/// Downloaded delta package.
pub const DELTA_FILE: &str = "delta.zip";
/// Unpacked next version.
pub const APP_DIR: &str = "app";
/// Number of download attempts: the first try plus one retry.
pub const DOWNLOAD_ATTEMPTS: u32 = 2;
/// Pause between download attempts.
const RETRY_DELAY: Duration = Duration::from_millis(200);

/// How the staged package was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StageMethod {
    Delta,
    Full,
    FullFallback,
}

/// Contents of `ready.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyMarker {
    pub version: String,
    pub package_sha256: String,
    pub install_dir: String,
    pub created_at: u64,
    pub method: StageMethod,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fallback_reason: Option<String>,
}

/// A verified staged update.
#[derive(Debug, Clone)]
pub struct StagedUpdate {
    pub version: Version,
    pub method: StageMethod,
    pub staging_dir: PathBuf,
    pub app_dir: PathBuf,
    pub package_sha256: String,
}

/// Result of a staging attempt.
#[derive(Debug)]
pub enum StageOutcome {
    Ready(StagedUpdate),
    UpToDate,
    Failed(UpdateError),
}

/// Input for [`stage_update`].
///
/// `manifest` is a [`VerifiedManifest`]: staging accepts only a document whose
/// detached signature has already been checked.
pub struct StageRequest<'a> {
    pub manifest: &'a VerifiedManifest,
    pub channel: &'a str,
    pub current_version: &'a Version,
    pub install_dir: &'a Path,
    pub installed: Option<&'a PackageIndex>,
    pub updates_root: &'a Path,
    pub cancel: CancelToken,
    pub progress: Option<ProgressFn>,
}

/// Default updates root for this user.
///
/// The directory is per-user and never a shared temporary directory. Without
/// `LOCALAPPDATA` (Windows) or a usable cache/home directory elsewhere the
/// function returns [`UpdateError::InvalidArgument`] instead of falling back.
pub fn updates_root() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        updates_root_from_base(std::env::var_os("LOCALAPPDATA").as_deref())
    }
    #[cfg(not(windows))]
    {
        if let Some(base) = std::env::var_os("XDG_CACHE_HOME") {
            return Ok(PathBuf::from(base).join("voxhold").join("updates"));
        }
        match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => {
                Ok(PathBuf::from(home).join(".cache").join("voxhold").join("updates"))
            }
            _ => Err(UpdateError::InvalidArgument(
                "neither XDG_CACHE_HOME nor HOME is set; refusing to use a shared temporary directory for updates".into(),
            )),
        }
    }
}

#[cfg(windows)]
fn updates_root_from_base(base: Option<&std::ffi::OsStr>) -> Result<PathBuf> {
    match base {
        Some(base) if !base.is_empty() => Ok(PathBuf::from(base).join("VoxHold").join("updates")),
        _ => Err(UpdateError::InvalidArgument(
            "LOCALAPPDATA is not set; refusing to use a shared temporary directory for updates"
                .into(),
        )),
    }
}

/// Staging directory for a version.
pub fn staging_dir(root: &Path, version: &Version) -> PathBuf {
    root.join(version.to_string())
}

/// Read `ready.json` from a staging directory.
pub fn read_ready(staging: &Path) -> Result<ReadyMarker> {
    let path = staging.join(READY_FILE);
    let bytes = std::fs::read(&path).map_err(|error| UpdateError::io(&path, error))?;
    serde_json::from_slice(&bytes).map_err(|_| UpdateError::NotStaged(staging.to_path_buf()))
}

/// Stage an update for the next start or an explicit install command.
pub async fn stage_update(request: StageRequest<'_>) -> StageOutcome {
    match stage_inner(&request).await {
        Ok(Some(staged)) => StageOutcome::Ready(staged),
        Ok(None) => StageOutcome::UpToDate,
        Err(error) => {
            let _ = cleanup_version(request.updates_root, &request.manifest.version).await;
            StageOutcome::Failed(error)
        }
    }
}

async fn stage_inner(request: &StageRequest<'_>) -> Result<Option<StagedUpdate>> {
    request.manifest.check_channel(request.channel)?;
    let candidate = match request.manifest.offer(request.current_version)? {
        Offer::UpToDate => return Ok(None),
        Offer::Update(version) => version,
    };
    if let Some(current) = WindowsVersion::current() {
        request.manifest.check_windows(&current)?;
    }

    let version = candidate;
    let staging = staging_dir(request.updates_root, &version);
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging)
            .await
            .map_err(|error| UpdateError::io(&staging, error))?;
    }
    tokio::fs::create_dir_all(staging.join(APP_DIR))
        .await
        .map_err(|error| UpdateError::io(&staging, error))?;

    let base = base_package(request).await;
    let mut delta_reason = None;
    if let Some((base, delta)) = base {
        match stage_delta(request, &staging, &base, delta).await {
            Ok(()) => {
                return ready(request, version, staging, StageMethod::Delta, None).map(Some);
            }
            Err(UpdateError::Cancelled) => return Err(UpdateError::Cancelled),
            Err(error) => {
                delta_reason = Some(format!("{}: {error}", error.code()));
                let _ = tokio::fs::remove_dir_all(&staging).await;
                tokio::fs::create_dir_all(staging.join(APP_DIR))
                    .await
                    .map_err(|error| UpdateError::io(&staging, error))?;
            }
        }
    }

    stage_full(request, &staging).await?;
    let method = if delta_reason.is_some() {
        StageMethod::FullFallback
    } else {
        StageMethod::Full
    };
    ready(request, version, staging, method, delta_reason).map(Some)
}

async fn base_package(
    request: &StageRequest<'_>,
) -> Option<(PackedPackage, crate::manifest::DeltaEntry)> {
    let installed = request.installed?;
    let install_dir = request.install_dir.to_path_buf();
    let index = installed.clone();
    let packed =
        tokio::task::spawn_blocking(move || package::repack_installed(&install_dir, &index))
            .await
            .ok()?
            .ok()?;
    let delta = request
        .manifest
        .select_delta(&Version::parse(&installed.version).ok()?, &packed.sha256)?;
    Some((packed, delta.clone()))
}

async fn stage_delta(
    request: &StageRequest<'_>,
    staging: &Path,
    base: &PackedPackage,
    delta: crate::manifest::DeltaEntry,
) -> Result<()> {
    let delta_path = staging.join(DELTA_FILE);
    let mut attempt = 1;
    let outcome = loop {
        let result = download(DownloadRequest {
            url: &delta.url,
            dest: &delta_path,
            expected_size: delta.size,
            expected_sha256: &delta.sha256,
            max_bytes: delta.size.saturating_add(SIZE_MARGIN),
            cancel: request.cancel.clone(),
            progress: request.progress.clone(),
        })
        .await;
        match result {
            Err(UpdateError::Network(_)) if attempt < DOWNLOAD_ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
            other => break other?,
        }
    };
    if outcome.sha256 != delta.sha256 {
        return Err(UpdateError::HashMismatch { path: delta_path });
    }
    let bytes = tokio::fs::read(&delta_path)
        .await
        .map_err(|error| UpdateError::io(&delta_path, error))?;
    let base_bytes = base.bytes.clone();
    let target = tokio::task::spawn_blocking(move || crate::delta::apply(&base_bytes, &bytes))
        .await
        .map_err(|error| UpdateError::Apply(format!("delta task failed: {error}")))??;
    if package::sha256_hex(&target) != request.manifest.full.package_sha256 {
        return Err(UpdateError::DeltaTargetMismatch);
    }
    let package_path = staging.join(PACKAGE_FILE);
    tokio::fs::write(&package_path, &target)
        .await
        .map_err(|error| UpdateError::io(&package_path, error))?;
    unpack_to(staging, target).await
}

async fn stage_full(request: &StageRequest<'_>, staging: &Path) -> Result<()> {
    let package_path = staging.join(PACKAGE_FILE);
    let mut attempt = 1;
    let outcome = loop {
        let result = download(DownloadRequest {
            url: &request.manifest.full.url,
            dest: &package_path,
            expected_size: request.manifest.full.size,
            expected_sha256: &request.manifest.full.sha256,
            max_bytes: request.manifest.full.size.saturating_add(SIZE_MARGIN),
            cancel: request.cancel.clone(),
            progress: request.progress.clone(),
        })
        .await;
        match result {
            Err(UpdateError::Network(_)) if attempt < DOWNLOAD_ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
            other => break other?,
        }
    };
    if outcome.sha256 != request.manifest.full.package_sha256 {
        return Err(UpdateError::HashMismatch { path: package_path });
    }
    let bytes = tokio::fs::read(&package_path)
        .await
        .map_err(|error| UpdateError::io(&package_path, error))?;
    unpack_to(staging, bytes).await
}

async fn unpack_to(staging: &Path, bytes: Vec<u8>) -> Result<()> {
    let app = staging.join(APP_DIR);
    tokio::task::spawn_blocking(move || package::unpack(&bytes, &app))
        .await
        .map_err(|error| UpdateError::Apply(format!("unpack task failed: {error}")))??;
    Ok(())
}

fn ready(
    request: &StageRequest<'_>,
    version: Version,
    staging: PathBuf,
    method: StageMethod,
    fallback_reason: Option<String>,
) -> Result<StagedUpdate> {
    let package_sha256 = request.manifest.full.package_sha256.clone();
    let marker = ReadyMarker {
        version: version.to_string(),
        package_sha256: package_sha256.clone(),
        install_dir: request.install_dir.to_string_lossy().into_owned(),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
        method,
        fallback_reason,
    };
    let path = staging.join(READY_FILE);
    let temporary = staging.join("ready.json.tmp");
    let bytes = serde_json::to_vec_pretty(&marker).expect("ready marker serialization");
    // The marker is small; a synchronous write keeps the function total.
    std::fs::write(&temporary, bytes).map_err(|error| UpdateError::io(&temporary, error))?;
    std::fs::rename(&temporary, &path).map_err(|error| UpdateError::io(&path, error))?;
    Ok(StagedUpdate {
        version,
        method,
        app_dir: staging.join(APP_DIR),
        staging_dir: staging,
        package_sha256,
    })
}

/// Remove one version's staging directory.
pub async fn cleanup_version(root: &Path, version: &str) -> Result<()> {
    let path = root.join(version);
    match tokio::fs::remove_dir_all(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(UpdateError::io(path, error)),
    }
}

/// Remove leftovers from older attempts on startup.
///
/// `keep_version` keeps the staging directory of an update that is ready to
/// install. Every other directory and stray file under `root` is removed.
pub fn cleanup_stale(root: &Path, keep_version: Option<&str>) -> Result<usize> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(UpdateError::io(root, error)),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|error| UpdateError::io(root, error))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if keep_version == Some(name.as_ref()) {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| UpdateError::io(&path, error))?;
        if file_type.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|error| UpdateError::io(&path, error))?;
        } else {
            std::fs::remove_file(&path).map_err(|error| UpdateError::io(&path, error))?;
        }
        removed += 1;
    }
    Ok(removed)
}

/// Remove a staging directory after a successful install.
pub fn cleanup_after_install(staging: &Path) -> Result<()> {
    match std::fs::remove_dir_all(staging) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(UpdateError::io(staging, error)),
    }
}

/// Remove the staged payload but keep the relocated helper image and its log.
///
/// Windows cannot delete a running executable. When the helper was copied into
/// the staging directory it removes everything else here; `helper.exe` and
/// `apply.log` stay behind until the next start calls [`cleanup_stale`].
pub fn cleanup_after_install_payload(staging: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(staging) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(UpdateError::io(staging, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| UpdateError::io(staging, error))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.eq_ignore_ascii_case(crate::apply::HELPER_FILE) || name == APPLY_LOG {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| UpdateError::io(&path, error))?;
        if file_type.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|error| UpdateError::io(&path, error))?;
        } else {
            std::fs::remove_file(&path).map_err(|error| UpdateError::io(&path, error))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_removes_old_attempts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("updates");
        std::fs::create_dir_all(root.join("0.1.0").join(APP_DIR)).unwrap();
        std::fs::create_dir_all(root.join("0.2.0").join(APP_DIR)).unwrap();
        std::fs::write(root.join("stray.part"), b"partial").unwrap();
        assert_eq!(cleanup_stale(&root, Some("0.2.0")).unwrap(), 2);
        assert!(!root.join("0.1.0").exists());
        assert!(root.join("0.2.0").exists());
        assert!(!root.join("stray.part").exists());
        assert_eq!(cleanup_stale(&root, None).unwrap(), 1);
        assert!(root.exists());
    }

    #[test]
    fn payload_cleanup_keeps_the_relocated_helper() {
        let temp = tempfile::tempdir().unwrap();
        let staging = temp.path().join("0.2.0");
        std::fs::create_dir_all(staging.join(APP_DIR)).unwrap();
        std::fs::write(staging.join(APP_DIR).join("Voxhold.exe"), b"exe").unwrap();
        std::fs::write(staging.join(PACKAGE_FILE), b"zip").unwrap();
        std::fs::write(staging.join(READY_FILE), b"{}").unwrap();
        std::fs::write(staging.join(crate::apply::HELPER_FILE), b"helper").unwrap();
        std::fs::write(staging.join(APPLY_LOG), b"log").unwrap();

        cleanup_after_install_payload(&staging).unwrap();
        assert!(!staging.join(APP_DIR).exists());
        assert!(!staging.join(PACKAGE_FILE).exists());
        assert!(!staging.join(READY_FILE).exists());
        assert!(staging.join(crate::apply::HELPER_FILE).exists());
        assert!(staging.join(APPLY_LOG).exists());
    }

    #[cfg(windows)]
    #[test]
    fn updates_root_requires_local_appdata() {
        assert!(matches!(
            updates_root_from_base(None),
            Err(UpdateError::InvalidArgument(_))
        ));
        assert!(matches!(
            updates_root_from_base(Some(std::ffi::OsStr::new(""))),
            Err(UpdateError::InvalidArgument(_))
        ));
        let root =
            updates_root_from_base(Some(std::ffi::OsStr::new(r"C:\Users\test\AppData\Local")))
                .unwrap();
        assert!(root.ends_with(Path::new("VoxHold").join("updates")));
    }
}
