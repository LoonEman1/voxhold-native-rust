//! Staging of an update under `%LOCALAPPDATA%\VoxHold\updates\<version>`.
//!
//! A staged update is never written into the installation directory. It is
//! prepared in its own directory, verified, unpacked into `app/` and marked
//! with `ready.json`. Any failure removes the whole staging directory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::download::{download, CancelToken, DownloadRequest, ProgressFn, SIZE_MARGIN};
use crate::error::{Result, UpdateError};
use crate::manifest::{Offer, VerifiedManifest, WindowsVersion};
use crate::package::{self, PackageIndex, PackedPackage};

/// Marker written after a successful stage.
pub const READY_FILE: &str = "ready.json";
/// Exact signed `latest.json` bytes kept next to the staged package.
pub const MANIFEST_FILE: &str = "manifest.json";
/// Detached signature kept next to the staged package.
pub const SIGNATURE_FILE: &str = "manifest.sig";
/// Helper log inside the staging directory.
pub const APPLY_LOG: &str = "apply.log";
/// Downloaded or reconstructed full package.
pub const PACKAGE_FILE: &str = "package.zip";
/// Downloaded delta package.
pub const DELTA_FILE: &str = "delta.zip";
/// Unpacked next version.
pub const APP_DIR: &str = "app";
/// Marker written while this process owns the staging directory.
///
/// It carries `{pid, created_at}`. `cleanup_stale*` skips a version directory
/// that contains it, so a second process of the same installation cannot delete
/// an active download. A PID alone is not enough: Windows reuses process ids,
/// so a claim older than [`ACTIVE_MAX_AGE`] is stale even when the id is alive
/// again. The marker is removed once `ready.json` exists.
pub const ACTIVE_FILE: &str = "downloading.lock";
/// Age after which a `downloading.lock` claim is stale regardless of its PID.
///
/// A download that runs longer than this is already beyond every timeout in
/// the update layer; treating the directory as abandoned recovers from a crash
/// and from a reused PID.
pub const ACTIVE_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);
/// A staging directory is assembled as `<version>.staging-<pid>` and renamed
/// into place after its owner marker exists. Cleanup leaves such a directory
/// alone while it is younger than this grace window, so the short creation
/// race cannot delete a download that has not written its marker yet.
pub const STAGING_GRACE: Duration = Duration::from_secs(5);
/// Suffix of the temporary directory name used by [`begin_staging`].
const STAGING_TEMP_MARKER: &str = ".staging-";
/// Distinguishes two staging attempts of the same process.
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Number of download attempts: the first try plus one retry.
pub const DOWNLOAD_ATTEMPTS: u32 = 2;
/// Pause between download attempts.
const RETRY_DELAY: Duration = Duration::from_millis(200);

/// Body of [`ACTIVE_FILE`].
#[derive(Debug, Serialize, Deserialize)]
struct ActiveMarker {
    pid: u32,
    created_at: u64,
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

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
    /// SHA-256 of `manifest.json`. Empty in a marker written before signed
    /// proofs were stored; such a marker is not installable.
    #[serde(default)]
    pub manifest_sha256: String,
    /// Base64 detached signature over `manifest.json`.
    #[serde(default)]
    pub manifest_signature: String,
}

/// A verified staged update.
#[derive(Debug, Clone)]
pub struct StagedUpdate {
    pub version: Version,
    pub method: StageMethod,
    pub staging_dir: PathBuf,
    pub app_dir: PathBuf,
    pub package_sha256: String,
    /// `true` when the staging directory carries a signed manifest proof.
    /// A marker without proof may be shown but is never installed.
    pub verified: bool,
}

/// Result of a staging attempt.
#[derive(Debug)]
pub enum StageOutcome {
    Ready(StagedUpdate),
    UpToDate,
    Failed(UpdateError),
}

/// Visible phase of a staging attempt.
///
/// The UI must not guess the phase from byte counters: a failed delta restarts
/// the whole download, so a full package can begin after the first progress
/// callback already reached its total. Staging reports every transition here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagePhase {
    /// A package body is being downloaded; `total` is its expected size.
    Downloading { total: u64 },
    /// The body is on disk and is being hashed, verified and unpacked.
    Preparing,
}

/// Phase callback for [`StageRequest::phase`].
pub type PhaseFn = Arc<dyn Fn(StagePhase) + Send + Sync>;

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
    /// Reported before every download and once its body is complete.
    pub phase: Option<PhaseFn>,
}

fn report_phase(request: &StageRequest<'_>, phase: StagePhase) {
    if let Some(callback) = &request.phase {
        callback(phase);
    }
}

/// Stop between stages, not only inside the download loop.
///
/// The UI can cancel while a finished body is hashed or unpacked. The blocking
/// unpack cannot be interrupted, so the check runs before it and once more
/// after it; either way the staging directory is removed and no update is
/// offered.
fn check_cancelled(request: &StageRequest<'_>) -> Result<()> {
    if request.cancel.is_cancelled() {
        Err(UpdateError::Cancelled)
    } else {
        Ok(())
    }
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

/// Stable per-installation key.
///
/// Two portable copies under the same Windows user share `%LOCALAPPDATA%`.
/// Binding staging to the canonical install path keeps their preparations (and
/// the helper's cleanup) apart.
pub fn installation_id(install_dir: &Path) -> String {
    let canonical =
        std::fs::canonicalize(install_dir).unwrap_or_else(|_| install_dir.to_path_buf());
    let text = canonical.to_string_lossy().to_lowercase();
    crate::package::sha256_hex(text.as_bytes())[..16].to_string()
}

/// Updates root for one installation.
pub fn updates_root_for(install_dir: &Path) -> Result<PathBuf> {
    Ok(updates_root()?.join(installation_id(install_dir)))
}

/// Staging directory for a version of one installation.
pub fn staging_dir(root: &Path, install_dir: &Path, version: &Version) -> PathBuf {
    root.join(installation_id(install_dir))
        .join(version.to_string())
}

/// Read `ready.json` from a staging directory.
pub fn read_ready(staging: &Path) -> Result<ReadyMarker> {
    let path = staging.join(READY_FILE);
    let bytes = std::fs::read(&path).map_err(|error| UpdateError::io(&path, error))?;
    serde_json::from_slice(&bytes).map_err(|_| UpdateError::NotStaged(staging.to_path_buf()))
}

/// Whether another process is currently downloading into `staging`.
///
/// The marker carries the owner's PID and creation time. A claim is stale when
/// it is older than [`ACTIVE_MAX_AGE`] or its process is gone, so a crashed
/// download and a reused PID cannot block a retry forever. A marker that
/// cannot be read or parsed is treated as active: cleanup never deletes a
/// claim it does not understand. A missing marker is not a claim.
pub fn staging_is_active(staging: &Path) -> bool {
    let marker = staging.join(ACTIVE_FILE);
    match std::fs::read_to_string(&marker) {
        Ok(text) => claim_is_active(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Interpret a claim body: the current JSON form or a legacy plain PID.
fn claim_is_active(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        // An empty claim is unreadable in effect: never clean it up.
        return true;
    }
    // Legacy markers before `created_at` carried only the PID. Such a claim
    // has no age, so only the process check can release it.
    if let Ok(pid) = text.parse::<u32>() {
        return process_alive(pid);
    }
    let Ok(marker) = serde_json::from_str::<ActiveMarker>(text) else {
        return true;
    };
    let age = now_seconds().saturating_sub(marker.created_at);
    if age > ACTIVE_MAX_AGE.as_secs() {
        return false;
    }
    process_alive(marker.pid)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    if pid == std::process::id() {
        return true;
    }
    // SAFETY: the handle is closed on both paths below.
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            // No process with this id (or no access): the claim is stale.
            return false;
        }
        let wait = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        wait == WAIT_TIMEOUT
    }
}

/// Outside Windows the client is not shipped; keeping an unclear claim is the
/// safe choice for the shared library tests.
#[cfg(not(windows))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// Whether cleanup must leave the directory alone.
///
/// The claim protects an unfinished download. If `ready.json` already exists,
/// the package is complete and an owner that crashed between the rename and
/// the marker removal must not leak the directory forever.
fn cleanup_protected(staging: &Path) -> bool {
    staging_is_active(staging) && !staging.join(READY_FILE).is_file()
}

/// Whether a directory is a staging tree that is still being assembled.
///
/// `begin_staging` creates the tree under a temporary name, writes the claim
/// and only then renames it to the version name. A cleanup that runs in the
/// short window before the rename sees the temporary name; it must not remove
/// it while it is fresh.
fn staging_temp_young(name: &str, path: &Path) -> bool {
    name.contains(STAGING_TEMP_MARKER) && modified_within(path, STAGING_GRACE)
}

/// Whether the directory was modified less than `window` ago.
///
/// A metadata or clock failure is handled conservatively: a tree is treated as
/// fresh when its age cannot be established.
fn modified_within(path: &Path, window: Duration) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age < window)
        .unwrap_or(true)
}

/// Create the staging directory and claim it for this process.
///
/// The tree is built under `<version>.staging-<pid>` with the claim already in
/// place and renamed to its final name atomically. Cleanup therefore never
/// sees the final directory without a marker, and the temporary tree is
/// skipped by [`cleanup_stale_keeping`] while it is younger than
/// [`STAGING_GRACE`].
fn begin_staging(staging: &Path) -> Result<()> {
    let parent = staging.parent().ok_or_else(|| {
        UpdateError::InvalidArgument(format!("staging path {} has no parent", staging.display()))
    })?;
    let name = staging
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            UpdateError::InvalidArgument(format!("staging path {} has no name", staging.display()))
        })?;
    let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        "{name}{STAGING_TEMP_MARKER}{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&temporary);
    std::fs::create_dir_all(temporary.join(APP_DIR))
        .map_err(|error| UpdateError::io(&temporary, error))?;
    let marker = ActiveMarker {
        pid: std::process::id(),
        created_at: now_seconds(),
    };
    let body = serde_json::to_vec(&marker).expect("active marker serialization");
    let marker_path = temporary.join(ACTIVE_FILE);
    if let Err(error) = std::fs::write(&marker_path, body) {
        let _ = std::fs::remove_dir_all(&temporary);
        return Err(UpdateError::io(&marker_path, error));
    }
    match std::fs::rename(&temporary, staging) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temporary);
            // Another process may have completed the same directory between
            // our existence check and the rename. Do not let the caller clean
            // a tree this process never created.
            if staging.exists() {
                Err(UpdateError::AlreadyDownloading(staging.to_path_buf()))
            } else {
                Err(UpdateError::io(staging, error))
            }
        }
    }
}

/// Stage an update for the next start or an explicit install command.
pub async fn stage_update(request: StageRequest<'_>) -> StageOutcome {
    match stage_inner(&request).await {
        Ok(Some(staged)) => StageOutcome::Ready(staged),
        Ok(None) => StageOutcome::UpToDate,
        // The directory belongs to another process; deleting it here would
        // destroy a download this process never started.
        Err(error @ UpdateError::AlreadyDownloading(_)) => StageOutcome::Failed(error),
        Err(error) => {
            let _ = cleanup_version(
                request.updates_root,
                request.install_dir,
                &request.manifest.version,
            )
            .await;
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
    let staging = staging_dir(request.updates_root, request.install_dir, &version);
    if staging.exists() {
        // A directory claimed by another process is never removed: the second
        // client may be mid-download. The UI already refuses to start a
        // download without the per-installation mutex.
        if staging_is_active(&staging) {
            return Err(UpdateError::AlreadyDownloading(staging));
        }
        tokio::fs::remove_dir_all(&staging)
            .await
            .map_err(|error| UpdateError::io(&staging, error))?;
    }
    begin_staging(&staging)?;

    check_cancelled(request)?;
    let base = base_package(request).await;
    let mut delta_reason = None;
    if let Some((base, delta)) = base {
        match stage_delta(request, &staging, base, delta).await {
            Ok(()) => {
                return ready(request, version, staging, StageMethod::Delta, None).map(Some);
            }
            Err(UpdateError::Cancelled) => return Err(UpdateError::Cancelled),
            Err(error) => {
                delta_reason = Some(format!("{}: {error}", error.code()));
                let _ = tokio::fs::remove_dir_all(&staging).await;
                begin_staging(&staging)?;
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
    base: PackedPackage,
    delta: crate::manifest::DeltaEntry,
) -> Result<()> {
    let delta_path = staging.join(DELTA_FILE);
    let mut attempt = 1;
    report_phase(request, StagePhase::Downloading { total: delta.size });
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
    report_phase(request, StagePhase::Preparing);
    check_cancelled(request)?;
    if outcome.sha256 != delta.sha256 {
        return Err(UpdateError::HashMismatch { path: delta_path });
    }
    let bytes = tokio::fs::read(&delta_path)
        .await
        .map_err(|error| UpdateError::io(&delta_path, error))?;
    // The base package is moved into the blocking task instead of being
    // cloned: the archive is the largest buffer in the delta path.
    let target = tokio::task::spawn_blocking(move || crate::delta::apply(&base.bytes, &bytes))
        .await
        .map_err(|error| UpdateError::Apply(format!("delta task failed: {error}")))??;
    if package::sha256_hex(&target) != request.manifest.full.package_sha256 {
        return Err(UpdateError::DeltaTargetMismatch);
    }
    let package_path = staging.join(PACKAGE_FILE);
    tokio::fs::write(&package_path, &target)
        .await
        .map_err(|error| UpdateError::io(&package_path, error))?;
    unpack_bytes_to(staging, target, &request.manifest.version).await?;
    check_cancelled(request)
}

async fn stage_full(request: &StageRequest<'_>, staging: &Path) -> Result<()> {
    let package_path = staging.join(PACKAGE_FILE);
    let mut attempt = 1;
    report_phase(
        request,
        StagePhase::Downloading {
            total: request.manifest.full.size,
        },
    );
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
    report_phase(request, StagePhase::Preparing);
    check_cancelled(request)?;
    if outcome.sha256 != request.manifest.full.package_sha256 {
        return Err(UpdateError::HashMismatch { path: package_path });
    }
    // Hash and unpack one opened file: a different file cannot slip between
    // the digest check and the ZIP reader.
    unpack_file_to(
        staging,
        &package_path,
        &request.manifest.full.package_sha256,
        &request.manifest.version,
    )
    .await?;
    check_cancelled(request)
}

async fn unpack_bytes_to(staging: &Path, bytes: Vec<u8>, version: &str) -> Result<()> {
    let app = staging.join(APP_DIR);
    let version = version.to_owned();
    tokio::task::spawn_blocking(move || {
        let index = package::unpack(&bytes, &app)?;
        if index.version != version {
            return Err(UpdateError::InvalidPackage(format!(
                "package version {} does not match manifest version {version}",
                index.version
            )));
        }
        Ok(())
    })
    .await
    .map_err(|error| UpdateError::Apply(format!("unpack task failed: {error}")))??;
    Ok(())
}

async fn unpack_file_to(
    staging: &Path,
    archive: &Path,
    expected_sha256: &str,
    version: &str,
) -> Result<()> {
    let app = staging.join(APP_DIR);
    let archive = archive.to_path_buf();
    let expected_sha256 = expected_sha256.to_owned();
    let version = version.to_owned();
    tokio::task::spawn_blocking(move || {
        let index = package::unpack_file_verified(&archive, &app, &expected_sha256)?;
        if index.version != version {
            return Err(UpdateError::InvalidPackage(format!(
                "package version {} does not match manifest version {version}",
                index.version
            )));
        }
        Ok(())
    })
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
    // A cancel that arrived after the unpack must not leave `ready.json`
    // behind: the next start would offer an update the user stopped.
    check_cancelled(request)?;
    let package_sha256 = request.manifest.full.package_sha256.clone();
    // Keep the exact bytes and signature that were verified. A value built
    // only in memory has no signature; its marker stays displayable but the
    // startup check refuses to install it.
    let manifest_bytes = if request.manifest.signed_bytes().is_empty() {
        serde_json::to_vec(request.manifest.manifest()).expect("manifest serialization")
    } else {
        request.manifest.signed_bytes().to_vec()
    };
    let manifest_path = staging.join(MANIFEST_FILE);
    std::fs::write(&manifest_path, &manifest_bytes)
        .map_err(|error| UpdateError::io(&manifest_path, error))?;
    let signature = request.manifest.signature().to_string();
    if !signature.is_empty() {
        let signature_path = staging.join(SIGNATURE_FILE);
        std::fs::write(&signature_path, signature.as_bytes())
            .map_err(|error| UpdateError::io(&signature_path, error))?;
    }
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
        manifest_sha256: package::sha256_hex(&manifest_bytes),
        manifest_signature: signature,
    };
    let path = staging.join(READY_FILE);
    let temporary = staging.join("ready.json.tmp");
    let bytes = serde_json::to_vec_pretty(&marker).expect("ready marker serialization");
    // The marker is small; a synchronous write keeps the function total.
    std::fs::write(&temporary, bytes).map_err(|error| UpdateError::io(&temporary, error))?;
    std::fs::rename(&temporary, &path).map_err(|error| UpdateError::io(&path, error))?;
    // The package is complete once `ready.json` exists. Releasing the claim
    // afterwards means a cleanup never sees a half-finished directory.
    let _ = std::fs::remove_file(staging.join(ACTIVE_FILE));
    Ok(StagedUpdate {
        version,
        method,
        app_dir: staging.join(APP_DIR),
        staging_dir: staging,
        package_sha256,
        verified: !marker.manifest_signature.is_empty(),
    })
}

/// Verify the signed proof stored with a staged package, offline.
///
/// The startup scan and the app's pre-launch check use this via
/// [`verify_staged_proof_with`]; the helper calls this immediately before
/// [`crate::apply::apply_staged`]. Between the app's check and the helper's
/// copy there is a window in which `app/package.json` could be replaced, so the
/// helper must repeat the check itself instead of trusting the app.
///
/// Uses the embedded release key. Checks the detached signature, channel,
/// version, install directory, full ZIP hash, the index inside the ZIP and the
/// unpacked payload (including `app/package.json`).
pub fn verify_staged_proof(staging: &Path, install_dir: &Path) -> Result<StagedUpdate> {
    let key = crate::verify::embedded_public_key()?;
    verify_staged_proof_with(staging, install_dir, &key)
}

/// [`verify_staged_proof`] with an explicit key for tests and local acceptance.
pub fn verify_staged_proof_with(
    staging: &Path,
    install_dir: &Path,
    key: &crate::verify::UpdatePublicKey,
) -> Result<StagedUpdate> {
    let marker = read_ready(staging)?;
    let mismatch = || {
        UpdateError::InvalidPackage(format!(
            "staged proof at {} does not match the staged package",
            staging.display()
        ))
    };
    if marker.manifest_sha256.is_empty() || marker.manifest_signature.is_empty() {
        return Err(mismatch());
    }
    let manifest_path = staging.join(MANIFEST_FILE);
    let raw =
        std::fs::read(&manifest_path).map_err(|error| UpdateError::io(&manifest_path, error))?;
    if package::sha256_hex(&raw) != marker.manifest_sha256 {
        return Err(mismatch());
    }
    let signature_path = staging.join(SIGNATURE_FILE);
    let signature = match std::fs::read_to_string(&signature_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            marker.manifest_signature.clone()
        }
        Err(error) => return Err(UpdateError::io(&signature_path, error)),
    };
    if signature.trim() != marker.manifest_signature.trim() {
        return Err(mismatch());
    }
    let manifest = crate::verify::verify_and_parse(key, &raw, &signature)?;
    if manifest.channel != crate::manifest::BETA_CHANNEL || manifest.version != marker.version {
        return Err(mismatch());
    }
    let version = Version::parse(&marker.version)
        .map_err(|_| UpdateError::InvalidVersion(marker.version.clone()))?;
    if let Some(windows) = WindowsVersion::current() {
        manifest.check_windows(&windows)?;
    }
    if marker.install_dir.is_empty() || !paths_equal(Path::new(&marker.install_dir), install_dir) {
        return Err(mismatch());
    }
    if manifest.full.package_sha256 != marker.package_sha256 {
        return Err(mismatch());
    }
    let package_path = staging.join(PACKAGE_FILE);
    if package::sha256_file(&package_path)? != marker.package_sha256 {
        return Err(mismatch());
    }
    let index = package::read_package_json_from_file(&package_path)?;
    if index.version != marker.version {
        return Err(mismatch());
    }
    // `package.json` is not part of its own file index, so a replaced copy
    // would pass `verify_dir`. Compare it with the verified ZIP index and with
    // the canonical bytes; the helper reads this file when it copies the tree.
    let app = staging.join(APP_DIR);
    let app_index = package::read_package_json_from_dir(&app)?;
    if app_index != index {
        return Err(mismatch());
    }
    let app_json_path = app.join(package::PACKAGE_JSON);
    let app_json =
        std::fs::read(&app_json_path).map_err(|error| UpdateError::io(&app_json_path, error))?;
    if app_json != app_index.to_json() {
        return Err(mismatch());
    }
    package::verify_dir(&app, &index)?;
    Ok(StagedUpdate {
        version,
        method: marker.method,
        staging_dir: staging.to_path_buf(),
        app_dir: app,
        package_sha256: marker.package_sha256,
        verified: true,
    })
}

/// Compare two paths the way Windows does: canonical when possible, then
/// case-insensitively.
fn paths_equal(left: &Path, right: &Path) -> bool {
    let canonical_left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let canonical_right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    canonical_left
        .to_string_lossy()
        .eq_ignore_ascii_case(&canonical_right.to_string_lossy())
}

/// Remove one version's staging directory for an installation.
pub async fn cleanup_version(root: &Path, install_dir: &Path, version: &str) -> Result<()> {
    let path = root.join(installation_id(install_dir)).join(version);
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
///
/// The function is best-effort per entry: an entry that cannot be removed (a
/// relocated `helper.exe` that is still running, a file held by another
/// process, a directory with such a file) is skipped so that the remaining
/// entries are still cleaned. The first start after an update therefore does
/// not fail because the helper is still waiting for its handshake. The
/// returned count is the number of entries that were actually removed; the
/// next start retries the skipped ones. Only a failure to enumerate `root`
/// itself is reported as [`UpdateError::Io`].
pub fn cleanup_stale(root: &Path, keep_version: Option<&str>) -> Result<usize> {
    match keep_version {
        Some(version) => cleanup_stale_keeping(root, &[version]),
        None => cleanup_stale_keeping(root, &[]),
    }
}

/// Remove leftovers while keeping several version directories.
///
/// The application needs this on the first start after an update: the version
/// directory of the running build still holds `helper.exe` and the handshake
/// file, while a different directory may hold a staged update that must stay
/// available to the user. Keeping both cannot be expressed with a single
/// `keep_version`. A directory claimed by an active download
/// ([`ACTIVE_FILE`]) is skipped as well, even when it is not in
/// `keep_versions`.
pub fn cleanup_stale_keeping(root: &Path, keep_versions: &[&str]) -> Result<usize> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(UpdateError::io(root, error)),
    };
    let mut removed = 0;
    for entry in entries {
        // A failed entry has no usable path to report; keep cleaning the rest.
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if keep_versions.iter().any(|keep| *keep == name.as_ref()) {
            continue;
        }
        let path = entry.path();
        let removed_ok = match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {
                // The directory may be a staging tree that is still being
                // assembled under its temporary name; its marker is written
                // immediately after creation, but the rename comes last.
                if staging_temp_young(&name, &path) {
                    continue;
                }
                // Another process claimed this directory for an active
                // download; deleting it would break that update.
                if cleanup_protected(&path) {
                    continue;
                }
                std::fs::remove_dir_all(&path).is_ok()
            }
            Ok(_) => std::fs::remove_file(&path).is_ok(),
            Err(_) => false,
        };
        if removed_ok {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Remove the pre-per-install layout `updates\<version>` from the shared base.
///
/// Older builds staged directly under `%LOCALAPPDATA%\VoxHold\updates`. The
/// current layout uses `updates\<installation-id>\<version>`, so an old
/// version directory under the base is never used again. Only entries that
/// look like that layout are removed: a directory whose name parses as a
/// version, and stray files. Every other directory (an installation id) is
/// left untouched, as is a version directory that another process is actively
/// downloading into. Best-effort per entry, like [`cleanup_stale`].
pub fn cleanup_legacy_layout(base: &Path) -> Result<usize> {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(UpdateError::io(base, error)),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let legacy_dir =
            file_type.is_dir() && Version::parse(&name).is_ok() && !cleanup_protected(&path);
        let removed_ok = if legacy_dir {
            std::fs::remove_dir_all(&path).is_ok()
        } else if file_type.is_file() {
            std::fs::remove_file(&path).is_ok()
        } else {
            false
        };
        if removed_ok {
            removed += 1;
        }
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
/// `apply.log` stay behind until the next start calls [`cleanup_stale`]. The
/// helper additionally calls [`cleanup_stale`] for the other version
/// directories before it exits.
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
#[path = "stage_tests.rs"]
mod tests;
