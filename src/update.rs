//! Application-side update state without any GPUI dependency.
//!
//! The network, signature, staging and apply work lives in `voxhold-update`.
//! This module adds the pieces the window needs and that must stay testable on
//! their own: the build version, the channel URLs, the dismissed-version
//! preference, startup cleanup/ready detection and short user-facing errors.

use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use voxhold_update::error::UpdateError;
use voxhold_update::manifest::{Manifest, Offer, WindowsVersion, BETA_CHANNEL};
use voxhold_update::recovery::{recover_install, RecoveryMode, RecoveryOutcome};
use voxhold_update::stage::{
    cleanup_stale_keeping, read_ready, updates_root, StagedUpdate, APP_DIR, READY_FILE,
};
use voxhold_update::urls::validate_update_url;

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

/// Stored update preferences. Only a manually skipped version is kept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePrefs {
    #[serde(default)]
    pub declined_version: Option<String>,
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
            Self::Downloading { .. } | Self::Installing => "icons/refresh.svg",
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
}

/// Percentage for the progress label. A zero total stays at zero.
pub fn percent(downloaded: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    ((downloaded.min(total) as u128 * 100) / total as u128).min(100) as u8
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
pub fn startup_scan(
    install_dir: &Path,
    current: &Version,
    root_override: Option<&Path>,
    cleanup_backup: bool,
) -> StartupScan {
    let root = match root_override
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(updates_root)
    {
        Ok(root) => root,
        Err(error) => {
            return StartupScan {
                error: Some(message_for_error(&error)),
                ..Default::default()
            }
        }
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
    let ready = find_ready(&root, install_dir, current);
    let mut keep = vec![current.to_string()];
    if let Some(staged) = &ready {
        keep.push(staged.version.to_string());
    }
    // A directory that still holds the handshake or the relocated helper
    // belongs to the update that just started this process. Its version may
    // differ from `current_version` when a local build did not set
    // `VOXHOLD_BUILD_VERSION`, so protect it by content, not only by name.
    keep.extend(protected_staging_dirs(&root));
    keep.sort();
    keep.dedup();
    let keep_refs: Vec<&str> = keep.iter().map(String::as_str).collect();
    if let Err(error) = cleanup_stale_keeping(&root, &keep_refs) {
        scan.error = Some(message_for_error(&error));
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

fn find_ready(root: &Path, install_dir: &Path, current: &Version) -> Option<StagedUpdate> {
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
        if !staging.join(READY_FILE).is_file() {
            continue;
        }
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
        let candidate = StagedUpdate {
            version: version.clone(),
            method: marker.method,
            staging_dir: staging.clone(),
            app_dir: staging.join(APP_DIR),
            package_sha256: marker.package_sha256,
        };
        if best
            .as_ref()
            .map(|current| version > current.version)
            .unwrap_or(true)
        {
            best = Some(candidate);
        }
    }
    best
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
mod tests {
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
    fn preferences_round_trip_atomically() {
        let dir = std::env::temp_dir().join(format!(
            "voxhold-update-prefs-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let path = dir.join(PREFS_FILE);
        let mut prefs = UpdatePrefs::default();
        prefs.set_declined(&Version::new(0, 2, 0));
        save_prefs(Some(&path), &prefs).unwrap();
        let loaded = load_prefs(Some(&path));
        assert_eq!(loaded, prefs);
        assert_eq!(loaded.declined(), Some(Version::new(0, 2, 0)));
        assert!(load_prefs(Some(&dir.join("missing.json")))
            .declined()
            .is_none());
        std::fs::write(&path, b"not json").unwrap();
        assert!(load_prefs(Some(&path)).declined().is_none());
        std::fs::remove_dir_all(&dir).ok();
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

    #[test]
    fn startup_scan_reports_a_ready_update_and_keeps_it() {
        let temp = std::env::temp_dir().join(format!(
            "voxhold-startup-scan-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let install = temp.join("VoxHold");
        let root = temp.join("updates");
        let staging = root.join("0.2.0");
        std::fs::create_dir_all(install.join("assets")).unwrap();
        std::fs::create_dir_all(staging.join(APP_DIR)).unwrap();
        std::fs::create_dir_all(root.join("0.1.9")).unwrap();
        let marker = voxhold_update::stage::ReadyMarker {
            version: "0.2.0".into(),
            package_sha256: "a".repeat(64),
            install_dir: install.to_string_lossy().into_owned(),
            created_at: 1,
            method: voxhold_update::stage::StageMethod::Full,
            fallback_reason: None,
        };
        std::fs::write(
            staging.join(READY_FILE),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();

        let scan = startup_scan(&install, &Version::new(0, 1, 0), Some(root.as_path()), true);
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
}
