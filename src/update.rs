//! Application-side update state without any GPUI dependency.
//!
//! The network, signature, staging and apply work lives in `voxhold-update`.
//! This module adds the pieces the window needs and that must stay testable on
//! their own: the build version, the channel URLs, `UpdatePrefs` (skipped
//! version plus the `auto_download` and `install_on_next_start` switches),
//! startup cleanup/ready detection and short user-facing errors.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use voxhold_update::error::UpdateError;
use voxhold_update::manifest::{Manifest, Offer, WindowsVersion, BETA_CHANNEL};
use voxhold_update::recovery::{recover_install, RecoveryMode, RecoveryOutcome};
use voxhold_update::stage::{
    cleanup_legacy_layout, cleanup_stale_keeping, read_ready, updates_root_for,
    verify_staged_proof, verify_staged_proof_with, StagedUpdate, APP_DIR,
};
use voxhold_update::urls::validate_update_url;
use voxhold_update::verify::{embedded_public_key, UpdatePublicKey};

/// Public beta channel.
pub const CHANNEL: &str = BETA_CHANNEL;
/// Pinned release channel used by a normal build.
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/LoonEman1/voxhold-native-rust/releases/download/beta/latest.json";
/// Detached signature for the pinned manifest.
pub const DEFAULT_SIGNATURE_URL: &str =
    "https://github.com/LoonEman1/voxhold-native-rust/releases/download/beta/latest.json.sig";
/// Version installed by the running executable.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Name of the preference file with a manually skipped version.
pub const PREFS_FILE: &str = "update-prefs.json";

/// Version the build was published with.
///
/// The release workflow sets `VOXHOLD_BUILD_VERSION`; a developer build falls
/// back to `CARGO_PKG_VERSION`. An unparsable value is ignored instead of
/// making the update layer offer a bogus comparison.
pub fn current_version() -> Version {
    version_from_sources(option_env!("VOXHOLD_BUILD_VERSION"), PACKAGE_VERSION)
}

/// Pure version selection used by the tests.
pub fn version_from_sources(build: Option<&str>, package: &str) -> Version {
    build
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| Version::parse(value).ok())
        .or_else(|| Version::parse(package).ok())
        .unwrap_or_else(|| Version::new(0, 0, 0))
}

/// Manifest and signature URLs for this run.
///
/// `VOXHOLD_UPDATE_BASE` is a local-acceptance override. It may point at a
/// loopback HTTP server or an HTTPS host; the shared URL policy rejects every
/// other combination. The signature is still mandatory.
pub fn update_urls() -> Result<(String, String), UpdateError> {
    let base = std::env::var("VOXHOLD_UPDATE_BASE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    urls_from_base(base.as_deref())
}

/// Pure URL selection used by [`update_urls`] and the tests.
pub fn urls_from_base(base: Option<&str>) -> Result<(String, String), UpdateError> {
    let (manifest, signature) = match base {
        Some(base) => (
            join_url(base, "latest.json"),
            join_url(base, "latest.json.sig"),
        ),
        None => (
            DEFAULT_MANIFEST_URL.to_string(),
            DEFAULT_SIGNATURE_URL.to_string(),
        ),
    };
    validate_update_url(&manifest)?;
    validate_update_url(&signature)?;
    Ok((manifest, signature))
}

fn join_url(base: &str, asset: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), asset)
}

/// Path of `%APPDATA%\Voxhold\update-prefs.json`.
pub fn prefs_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("Voxhold").join(PREFS_FILE))
}

fn default_true() -> bool {
    true
}

/// Stored update preferences: the skipped version and the two background
/// switches.
///
/// The file is shared by every process build, so a missing field is not an
/// error: an older `update-prefs.json` that only carried `declined_version`
/// reads both switches as `true`. `Default` is written out explicitly instead
/// of derived, because a derived `bool` default would silently turn the
/// background features off.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePrefs {
    #[serde(default)]
    pub declined_version: Option<String>,
    #[serde(default = "default_true")]
    pub auto_download: bool,
    #[serde(default = "default_true")]
    pub install_on_next_start: bool,
    /// A version whose automatic install was refused after a rollback. Manual
    /// install stays available.
    #[serde(default)]
    pub install_refused_version: Option<String>,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            declined_version: None,
            auto_download: default_true(),
            install_on_next_start: default_true(),
            install_refused_version: None,
        }
    }
}

impl UpdatePrefs {
    /// Parsed skipped version; an invalid stored value is ignored.
    pub fn declined(&self) -> Option<Version> {
        self.declined_version
            .as_deref()
            .and_then(|value| Version::parse(value.trim()).ok())
    }

    pub fn set_declined(&mut self, version: &Version) {
        self.declined_version = Some(version.to_string());
    }

    /// Whether an automatic install of `version` was refused after a rollback.
    pub fn install_refused(&self, version: &str) -> bool {
        self.install_refused_version.as_deref() == Some(version)
    }
}

/// Read the preference file. A missing or malformed file is empty prefs.
pub fn load_prefs(path: Option<&Path>) -> UpdatePrefs {
    path.and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<UpdatePrefs>(&bytes).ok())
        .unwrap_or_default()
}

/// Atomic write of the preference file.
pub fn save_prefs(path: Option<&Path>, prefs: &UpdatePrefs) -> std::io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(prefs).map_err(std::io::Error::other)?;
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Which action a failed state retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryKind {
    Check,
    Download,
    Install,
}

/// Visible update state.
#[derive(Clone, Debug)]
pub enum UpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Available {
        version: String,
        notes: String,
    },
    Downloading {
        version: String,
        downloaded: u64,
        total: u64,
    },
    /// The download finished and the archive is being verified and unpacked.
    Preparing {
        version: String,
    },
    Ready {
        version: String,
        staged: StagedUpdate,
    },
    Error {
        message: String,
        retry: RetryKind,
    },
    Installing,
}

impl UpdatePhase {
    /// Icon resource for the header button.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Idle | Self::UpToDate | Self::Checking => "icons/refresh.svg",
            Self::Available { .. } => "icons/down.svg",
            Self::Downloading { .. } | Self::Preparing { .. } | Self::Installing => {
                "icons/refresh.svg"
            }
            Self::Ready { .. } => "icons/check.svg",
            Self::Error { .. } => "icons/close.svg",
        }
    }

    /// Short tooltip; the header has no native tooltip API.
    pub fn tooltip(&self, current: &Version) -> String {
        match self {
            Self::Idle => format!("Версия {current}"),
            Self::Checking => "Проверяем обновления…".into(),
            Self::UpToDate => format!("Версия {current} · обновлений нет"),
            Self::Available { version, .. } => format!("Версия {current} · доступна {version}"),
            Self::Downloading {
                version,
                downloaded,
                total,
            } => format!("Загружаем {version} · {}%", percent(*downloaded, *total)),
            Self::Preparing { version } => format!("Готовим {version} к установке"),
            Self::Ready { version, .. } => format!("Версия {version} готова к установке"),
            Self::Error { .. } => "Ошибка обновления".into(),
            Self::Installing => "Устанавливаем обновление…".into(),
        }
    }

    /// Version advertised by an available or ready state.
    pub fn offered_version(&self) -> Option<&str> {
        match self {
            Self::Available { version, .. } | Self::Ready { version, .. } => Some(version),
            _ => None,
        }
    }

    /// Version whose download owns the cancel token.
    ///
    /// `Preparing` still belongs to the download: a failed delta can restart
    /// the full package from there, and the user must be able to cancel it.
    pub fn downloading_version(&self) -> Option<&str> {
        match self {
            Self::Downloading { version, .. } | Self::Preparing { version } => Some(version),
            _ => None,
        }
    }
}

/// Percentage for the progress label. A zero total stays at zero.
pub fn percent(downloaded: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    ((downloaded.min(total) as u128 * 100) / total as u128).min(100) as u8
}

/// Versions remembered for the whole process: auto-started or user-cancelled.
pub type VersionSet = BTreeSet<String>;

/// Whether a successful background check may start the download itself.
///
/// Auto-download is skipped when the switch is off, when the version is the
/// one the user skipped, when another process of the same installation owns
/// the update mutex (`single_instance == false`), and when this process
/// already started (or the user cancelled) that version. A manual button never
/// uses this decision. Both sets live as long as the process, so re-offering
/// an older version later does not start it again and cancelling a newer one
/// does not forget it.
pub fn should_auto_download(
    prefs: &UpdatePrefs,
    version: &str,
    declined: Option<&Version>,
    suppressed: &VersionSet,
    auto_started: &VersionSet,
    single_instance: bool,
) -> bool {
    if !single_instance || !prefs.auto_download {
        return false;
    }
    let Ok(candidate) = Version::parse(version) else {
        return false;
    };
    if declined.is_some_and(|value| value == &candidate) {
        return false;
    }
    if suppressed.contains(version) || auto_started.contains(version) {
        return false;
    }
    true
}

/// Whether a staged package found at startup may be installed automatically.
///
/// A package without a re-verified signed manifest is never installed: an old
/// `ready.json` has to be prepared again by a background download. A version
/// whose automatic install was refused after a rollback stays manual.
pub fn should_auto_install(prefs: &UpdatePrefs, staged: &StagedUpdate, current: &Version) -> bool {
    let version = staged.version.to_string();
    prefs.install_on_next_start
        && staged.verified
        && staged.version > *current
        && !prefs.install_refused(&version)
}

/// Sidecar the helper writes after a failed apply. It is read at the next
/// start and merged into `UpdatePrefs`.
///
/// Without `%APPDATA%` the preference file has no path, so the fallback lives
/// next to the installation. That is the only location this client can read
/// back without a per-user configuration directory.
pub fn refusal_path(prefs_path: Option<&Path>, install_dir: &Path) -> PathBuf {
    match prefs_path {
        Some(path) => path.with_extension("refusal.json"),
        None => install_dir.join("update-prefs.refusal.json"),
    }
}

/// Read a refusal left by the helper without removing it.
///
/// Removal is separate ([`clear_refusal`]) so a failed preference write cannot
/// lose the refusal: the in-memory value still blocks the install for this
/// run and the file is retried on the next start.
pub fn load_refusal(path: Option<&Path>) -> Option<String> {
    let path = path?;
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("version")?.as_str().map(str::to_string)
}

/// Remove a consumed refusal sidecar. A missing file is not an error.
pub fn clear_refusal(path: Option<&Path>) {
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
}

/// Merge a helper refusal into preferences.
///
/// The in-memory value is always updated so this launch cannot install the
/// same version again. The sidecar is removed only after the preference file
/// was written; otherwise it stays for the next start. Returns `true` when a
/// refusal was found.
pub fn merge_refusal(
    prefs: &mut UpdatePrefs,
    prefs_path: Option<&Path>,
    refusal: Option<&Path>,
) -> bool {
    let Some(version) = load_refusal(refusal) else {
        return false;
    };
    prefs.install_refused_version = Some(version);
    if save_prefs(prefs_path, prefs).is_ok() {
        clear_refusal(refusal);
    }
    true
}

/// Text under a ready version.
///
/// It only promises an automatic install when the user left the switch on and
/// this version was not refused after a rollback.
pub fn ready_note(install_on_next_start: bool, refused: bool) -> &'static str {
    if refused {
        "Готово к установке · автоустановка отключена"
    } else if install_on_next_start {
        "Установится при следующем запуске"
    } else {
        "Готово к установке"
    }
}

/// Compare a verified manifest with the installed version.
///
/// `windows` is injectable so the unit tests do not depend on the host build.
pub fn evaluate_manifest(
    manifest: &Manifest,
    current: &Version,
    declined: Option<&Version>,
    windows: Option<&WindowsVersion>,
) -> Result<UpdatePhase, UpdateError> {
    manifest.check_channel(CHANNEL)?;
    if let Some(windows) = windows {
        manifest.check_windows(windows)?;
    }
    match manifest.offer_with_dismissal(current, declined)? {
        Offer::UpToDate => Ok(UpdatePhase::UpToDate),
        Offer::Update(version) => Ok(UpdatePhase::Available {
            version: version.to_string(),
            notes: manifest.notes.clone(),
        }),
    }
}

/// Short Russian message for an update error.
///
/// No URL, query string or server text reaches the UI: only the typed error
/// and an HTTP status are used.
pub fn message_for_error(error: &UpdateError) -> String {
    match error {
        UpdateError::HttpStatus { status, .. } => match *status {
            404 => "Сервер обновлений не нашёл манифест (404).".into(),
            401 | 403 => "Сервер обновлений отклонил запрос.".into(),
            500..=599 => "Сервер обновлений временно недоступен.".into(),
            status => format!("Сервер обновлений ответил ошибкой {status}."),
        },
        UpdateError::BadSignature | UpdateError::BadSignatureEncoding => {
            "Подпись обновления не совпала.".into()
        }
        UpdateError::BadPublicKey => "Встроенный ключ обновлений повреждён.".into(),
        UpdateError::Network(_) => "Не удалось связаться с сервером обновлений.".into(),
        UpdateError::TooLarge { .. } | UpdateError::SizeMismatch { .. } => {
            "Размер файла обновления не совпал.".into()
        }
        UpdateError::HashMismatch { .. } | UpdateError::DeltaTargetMismatch => {
            "Контрольная сумма обновления не совпала.".into()
        }
        UpdateError::UnsupportedWindows { .. } => "Эта версия требует более новой Windows.".into(),
        UpdateError::Downgrade { .. } => "Канал предлагает более старую версию.".into(),
        UpdateError::ChannelMismatch { .. } => "Канал обновлений не совпал.".into(),
        UpdateError::UnsupportedSchema(_)
        | UpdateError::InvalidManifest(_)
        | UpdateError::InvalidVersion(_) => "Манифест обновления повреждён.".into(),
        UpdateError::Cancelled => "Загрузка отменена.".into(),
        UpdateError::AlreadyDownloading(_) => {
            "Эту версию уже скачивает другая копия приложения.".into()
        }
        UpdateError::Io { .. } => "Не удалось сохранить обновление на диске.".into(),
        UpdateError::InsecureUrl { .. } => "Адрес обновления небезопасен.".into(),
        UpdateError::InvalidArgument(_) => "Обновление настроено неверно.".into(),
        UpdateError::DeltaBaseMismatch | UpdateError::InvalidDelta(_) => {
            "Дельта обновления повреждена.".into()
        }
        UpdateError::InvalidPackage(_) => "Пакет обновления повреждён.".into(),
        UpdateError::InvalidState(_) => "Состояние обновления повреждено.".into(),
        UpdateError::NotStaged(_) | UpdateError::Apply(_) | UpdateError::LaunchTimeout(_) => {
            "Не удалось подготовить обновление.".into()
        }
    }
}

/// Outcome of the startup pass over `%LOCALAPPDATA%\VoxHold\updates`.
#[derive(Clone, Debug, Default)]
pub struct StartupScan {
    pub root: Option<PathBuf>,
    /// A staged newer version found on disk. It is shown, never applied.
    pub ready: Option<StagedUpdate>,
    /// Best-effort diagnostic; startup continues regardless.
    pub error: Option<String>,
    /// A non-fatal recovery note (an interrupted operation or a kept backup).
    pub warning: Option<String>,
}

/// Clean old staging attempts and find a staged newer version.
///
/// Runs off the UI thread. A locked helper image is skipped by the library's
/// best-effort cleanup, so a start that races the helper still succeeds.
///
/// `root_override` is a per-installation root (the directory that directly
/// contains version directories), used by tests. Without it the root is
/// `updates_root_for(install_dir)`, never the shared base: a second portable
/// copy must not see or clean the staging of another installation.
///
/// `cleanup_staging` is `false` for a process that does not own the
/// per-installation update mutex. Such a process only *reads* the staging
/// tree: it must not delete a package another instance is downloading or has
/// already prepared. Recovery and the legacy-layout cleanup are gated the same
/// way by the caller.
pub fn startup_scan(
    install_dir: &Path,
    current: &Version,
    root_override: Option<&Path>,
    cleanup_backup: bool,
    cleanup_staging: bool,
) -> StartupScan {
    match embedded_public_key() {
        Ok(key) => startup_scan_with_key(
            install_dir,
            current,
            root_override,
            cleanup_backup,
            cleanup_staging,
            &key,
        ),
        Err(error) => StartupScan {
            error: Some(message_for_error(&error)),
            ..Default::default()
        },
    }
}

/// Startup scan with an explicit verification key, used by local acceptance
/// and tests that sign their own manifest.
pub fn startup_scan_with_key(
    install_dir: &Path,
    current: &Version,
    root_override: Option<&Path>,
    cleanup_backup: bool,
    cleanup_staging: bool,
    key: &UpdatePublicKey,
) -> StartupScan {
    let (root, legacy_base) = match root_override {
        Some(root) => (root.to_path_buf(), None),
        None => match updates_root_for(install_dir) {
            Ok(root) => {
                // Both paths are derived from the same base; the legacy pass
                // needs the base, the scan needs the per-install directory.
                let base = root.parent().map(Path::to_path_buf);
                (root, base)
            }
            Err(error) => {
                return StartupScan {
                    error: Some(message_for_error(&error)),
                    ..Default::default()
                }
            }
        },
    };
    let mut scan = StartupScan {
        root: Some(root.clone()),
        ..Default::default()
    };
    if cleanup_backup {
        match recover_install(install_dir, RecoveryMode::Observer) {
            Ok(outcome) => {
                if let Some(message) = recovery_note(&outcome) {
                    scan.warning = Some(message);
                }
            }
            Err(error) => scan.error = Some(message_for_error(&error)),
        }
    }
    let ready = find_ready(&root, install_dir, current, key);
    if cleanup_staging {
        let mut keep = vec![current.to_string()];
        if let Some(staged) = &ready {
            keep.push(staged.version.to_string());
        }
        // A directory that still holds the handshake or the relocated helper
        // belongs to the update that just started this process. Its version
        // may differ from `current_version` when a local build did not set
        // `VOXHOLD_BUILD_VERSION`, so protect it by content, not only by name.
        keep.extend(protected_staging_dirs(&root));
        keep.sort();
        keep.dedup();
        let keep_refs: Vec<&str> = keep.iter().map(String::as_str).collect();
        if let Err(error) = cleanup_stale_keeping(&root, &keep_refs) {
            scan.error = Some(message_for_error(&error));
        }
        if let Some(base) = legacy_base {
            // Best-effort: the old layout is unused but harmless.
            let _ = cleanup_legacy_layout(&base);
        }
    }
    scan.ready = ready;
    scan
}

/// Repeat the cleanup after the helper had time to release its own image.
///
/// A ready update is preserved; every other directory, including the handshake
/// directory of the just-started build, is best-effort removed.
#[cfg(test)]
pub fn second_cleanup(root: &Path, ready: Option<&Version>) -> Result<usize, UpdateError> {
    let keep: Vec<String> = ready.map(Version::to_string).into_iter().collect();
    let keep_refs: Vec<&str> = keep.iter().map(String::as_str).collect();
    cleanup_stale_keeping(root, &keep_refs)
}

/// Like [`second_cleanup`] but also keeps an in-flight download directory.
pub fn second_cleanup_keeping(root: &Path, keep: &[&str]) -> Result<usize, UpdateError> {
    cleanup_stale_keeping(root, keep)
}

/// Version directories that still contain the handshake or the helper image.
fn protected_staging_dirs(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter(|entry| {
            let path = entry.path();
            path.join("handshake.ok").is_file()
                || path.join(voxhold_update::apply::HELPER_FILE).is_file()
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn find_ready(
    root: &Path,
    install_dir: &Path,
    current: &Version,
    key: &UpdatePublicKey,
) -> Option<StagedUpdate> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut best: Option<StagedUpdate> = None;
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let staging = entry.path();
        let Ok(marker) = read_ready(&staging) else {
            continue;
        };
        let Ok(version) = Version::parse(&marker.version) else {
            continue;
        };
        if version <= *current {
            continue;
        }
        if !staging.join(APP_DIR).is_dir() {
            continue;
        }
        if !marker.install_dir.is_empty()
            && !paths_equal(Path::new(&marker.install_dir), install_dir)
        {
            continue;
        }
        // The library repeats every offline proof check, including the
        // unpacked payload and `app/package.json`.
        let Ok(staged) = verify_staged_proof_with(&staging, install_dir, key) else {
            continue;
        };
        if staged.version != version {
            continue;
        }
        if best
            .as_ref()
            .map(|current| staged.version > current.version)
            .unwrap_or(true)
        {
            best = Some(staged);
        }
    }
    best
}

/// Re-verify a staged package right before the helper starts.
///
/// The startup scan verified the package when the process started; a long
/// session or another local writer can still change the staging directory
/// afterwards. Every offline check is repeated here with the embedded key.
pub fn reverify_staged(staged: &StagedUpdate, install_dir: &Path) -> bool {
    if !staged.verified {
        return false;
    }
    match verify_staged_proof(&staged.staging_dir, install_dir) {
        Ok(verified) => verified.version == staged.version,
        Err(_) => false,
    }
}

/// [`reverify_staged`] with an explicit verification key.
pub fn reverify_staged_with(
    staged: &StagedUpdate,
    install_dir: &Path,
    key: &UpdatePublicKey,
) -> bool {
    if !staged.verified {
        return false;
    }
    match verify_staged_proof_with(&staged.staging_dir, install_dir, key) {
        Ok(verified) => verified.version == staged.version,
        Err(_) => false,
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let canonical_left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let canonical_right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    canonical_left
        .to_string_lossy()
        .eq_ignore_ascii_case(&canonical_right.to_string_lossy())
}

/// User-facing note for a recovery result that kept data for a later run.
fn recovery_note(outcome: &RecoveryOutcome) -> Option<String> {
    match outcome {
        RecoveryOutcome::NeedsUpdater { .. } => {
            Some("Незавершённое обновление будет восстановлено установщиком.".into())
        }
        RecoveryOutcome::Ambiguous { .. } => {
            Some("Резервная копия обновления сохранена: состояние установки не определено.".into())
        }
        RecoveryOutcome::Corrupt { .. } => {
            Some("Файл состояния обновления повреждён; резервная копия сохранена.".into())
        }
        RecoveryOutcome::Clean
        | RecoveryOutcome::Completed { .. }
        | RecoveryOutcome::RolledBack { .. } => None,
    }
}

/// Confirm a successful start of the new version to the helper.
///
/// `operation` comes from `--update-operation`. An older helper does not pass
/// it and only checks that the file exists; the file still carries the version.
pub fn write_handshake(path: &Path, operation: Option<&str>) -> std::io::Result<()> {
    let version = current_version().to_string();
    voxhold_update::handshake::write(path, &version, operation)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "update_prefs_tests.rs"]
mod update_prefs_tests;
