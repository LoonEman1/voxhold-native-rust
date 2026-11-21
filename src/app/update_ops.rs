//! Background update controller: start-up cleanup, periodic checks, download,
//! cancellation and launching the helper.
//!
//! The UI thread never waits on the network or on the filesystem. A generation
//! discards late answers of a superseded manual check or download. Logging out
//! and closing the panel only hide the surface: neither drops a check, a
//! download or a staged package, so the generation is not bumped by them.

use super::*;
use crate::update::{self, RetryKind, UpdatePhase, UpdatePrefs};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use voxhold_update::check::{check_signed_manifest, CheckRequest};
use voxhold_update::download::CancelToken;
use voxhold_update::error::UpdateError;
use voxhold_update::manifest::{VerifiedManifest, WindowsVersion};
use voxhold_update::package::read_package_json_from_dir;
use voxhold_update::stage::{
    stage_update, updates_root, StageOutcome, StagePhase, StageRequest, StagedUpdate,
};
use voxhold_update::verify::embedded_public_key;

const START_DELAY: Duration = Duration::from_secs(3);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const SECOND_CLEANUP_DELAY: Duration = Duration::from_secs(10);
const PROGRESS_TICK: Duration = Duration::from_millis(100);
const HELPER_NAME: &str = "voxhold-updater.exe";
const PACKAGE_EXE_NAME: &str = "Voxhold.exe";
const HANDSHAKE_NAME: &str = "handshake.ok";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// One helper launch request. Paths are absolute.
pub(super) struct LaunchRequest {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
}

#[cfg(test)]
impl LaunchRequest {
    pub fn has_arg(&self, value: &str) -> bool {
        self.args.iter().any(|arg| arg == value)
    }

    pub fn value_after(&self, flag: &str) -> Option<String> {
        self.args
            .iter()
            .position(|arg| arg == flag)
            .and_then(|index| self.args.get(index + 1))
            .map(|value| value.to_string_lossy().into_owned())
    }
}

/// Seam used by the tests to observe the helper command line.
pub(super) trait UpdateLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<(), String>;
}

/// Production launcher: a windowless helper that waits for this PID.
pub(super) struct ProcessUpdateLauncher;

impl UpdateLauncher for ProcessUpdateLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<(), String> {
        let mut command = std::process::Command::new(&request.exe);
        command.args(&request.args);
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        command
            .spawn()
            .map(|_| ())
            .map_err(|_| "Не удалось запустить установщик.".to_string())
    }
}

/// Progress and phase shared between staging and the UI poller.
///
/// Staging decides when a download starts again after a failed delta; the UI
/// must not guess that from byte counters, because a full package restarts at
/// zero after the delta already reported its total.
pub(super) struct ProgressCounters {
    state: Mutex<ProgressState>,
}

#[derive(Clone, Copy)]
struct ProgressState {
    downloaded: u64,
    total: u64,
    phase: StagePhase,
}

impl ProgressCounters {
    pub(super) fn new(total: u64) -> Self {
        Self {
            state: Mutex::new(ProgressState {
                downloaded: 0,
                total,
                phase: StagePhase::Downloading { total },
            }),
        }
    }

    /// Record download bytes without changing the phase.
    pub(super) fn set(&self, downloaded: u64, total: Option<u64>) {
        let mut state = self.lock();
        state.downloaded = downloaded;
        if let Some(total) = total.filter(|value| *value > 0) {
            state.total = total;
        }
    }

    /// Start a new download attempt: zero bytes and a new expected total.
    pub(super) fn restart(&self, total: u64) {
        *self.lock() = ProgressState {
            downloaded: 0,
            total,
            phase: StagePhase::Downloading { total },
        };
    }

    pub(super) fn set_phase(&self, phase: StagePhase) {
        self.lock().phase = phase;
    }

    pub(super) fn snapshot(&self) -> (u64, u64) {
        let state = self.lock();
        (state.downloaded, state.total)
    }

    pub(super) fn phase(&self) -> StagePhase {
        self.lock().phase
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProgressState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Controller state owned by `Voxhold`.
pub(super) struct UpdateUi {
    pub phase: UpdatePhase,
    pub verified: Option<VerifiedManifest>,
    pub declined: Option<semver::Version>,
    /// Stored switches plus the skipped version, written as one document.
    pub prefs: UpdatePrefs,
    /// Invalidates late check/download answers.
    pub generation: u64,
    pub cancel: Option<CancelToken>,
    pub cancelling: bool,
    /// The version this process already auto-downloaded once.
    pub auto_started: Option<String>,
    /// The version whose automatic download the user cancelled this run.
    pub auto_suppressed: Option<String>,
    pub panel_open: bool,
    pub hover: bool,
    pub prefs_error: Option<String>,
    pub prefs_path: Option<PathBuf>,
    pub install_dir: PathBuf,
    pub exe_name: String,
    /// Test seam: use a temporary updates root instead of `LOCALAPPDATA`.
    pub root_override: Option<PathBuf>,
    /// Test seam: observe or fail the helper launch.
    pub launcher: Box<dyn UpdateLauncher>,
    /// Test seam: a successful launch asked the app to quit.
    pub quit_requested: bool,
    /// Test seam: how many download requests passed the phase check.
    pub download_starts: u32,
    /// Test seam: when false, a download records the start but does not spawn
    /// the staging task, so tests never touch a real network.
    pub download_task_enabled: bool,
    pub clock_task: Option<gpui::Task<()>>,
    pub work_task: Option<gpui::Task<()>>,
    pub cleanup_task: Option<gpui::Task<()>>,
    pub progress_task: Option<gpui::Task<()>>,
    pub progress: Option<Arc<ProgressCounters>>,
    pub ready: Option<StagedUpdate>,
    pub root: Option<PathBuf>,
    pub handshake_error: Option<String>,
}

impl UpdateUi {
    pub fn new() -> Self {
        let exe = std::env::current_exe().ok();
        let install_dir = exe
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let exe_name = exe
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Voxhold.exe".into());
        let prefs_path = update::prefs_path();
        let prefs = update::load_prefs(prefs_path.as_deref());
        let declined = prefs.declined();
        Self {
            phase: UpdatePhase::Idle,
            verified: None,
            declined,
            prefs,
            generation: 0,
            cancel: None,
            cancelling: false,
            auto_started: None,
            auto_suppressed: None,
            panel_open: false,
            hover: false,
            prefs_error: None,
            prefs_path,
            install_dir,
            exe_name,
            root_override: None,
            launcher: Box::new(ProcessUpdateLauncher),
            quit_requested: false,
            download_starts: 0,
            download_task_enabled: true,
            clock_task: None,
            work_task: None,
            cleanup_task: None,
            progress_task: None,
            progress: None,
            ready: None,
            root: None,
            handshake_error: None,
        }
    }
}

impl Voxhold {
    /// Start the startup cleanup and the 3 s / 6 h check clock.
    ///
    /// Called from `main` after the view exists, not from `Voxhold::new`, so
    /// tests never touch the real `%LOCALAPPDATA%`.
    ///
    /// `handshake_written` is `true` only when this process successfully wrote
    /// the handshake for the update it was started from. Once that file exists
    /// the operation is confirmed, so the observer must run its usual recovery
    /// even on an update start: it upgrades `new-in-place` to `completed`
    /// before the 10 s cleanup can remove the handshake. A failed handshake
    /// write leaves the operation unconfirmed and the backup in place for the
    /// helper to roll back.
    pub(crate) fn start_update(
        &mut self,
        after_update: bool,
        handshake_written: bool,
        cx: &mut Context<Self>,
    ) {
        let install = self.update.install_dir.clone();
        let root_override = self.update.root_override.clone();
        let current = update::current_version();
        let recovery_allowed = startup_recovery_allowed(after_update, handshake_written);
        let executor = cx.background_executor().clone();
        let scan_executor = cx.background_executor().clone();
        self.update.clock_task = Some(cx.spawn(async move |this, cx| {
            let scan = scan_executor
                .spawn(async move {
                    update::startup_scan(
                        &install,
                        &current,
                        root_override.as_deref(),
                        recovery_allowed,
                    )
                })
                .await;
            let _ = this.update(cx, |view, cx| view.apply_startup_scan(scan, cx));
            executor.timer(START_DELAY).await;
            let _ = this.update(cx, |view, cx| view.check_updates(cx));
            loop {
                executor.timer(CHECK_INTERVAL).await;
                let _ = this.update(cx, |view, cx| view.check_updates(cx));
            }
        }));

        let executor = cx.background_executor().clone();
        let cleanup_executor = cx.background_executor().clone();
        self.update.cleanup_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(SECOND_CLEANUP_DELAY).await;
            let Ok((root, ready, active)) = this.update(cx, |view, _| {
                let active = match &view.update.phase {
                    UpdatePhase::Downloading { version, .. }
                    | UpdatePhase::Preparing { version } => Some(version.clone()),
                    _ => None,
                };
                (
                    view.update.root.clone(),
                    view.update
                        .ready
                        .as_ref()
                        .map(|staged| staged.version.to_string()),
                    active,
                )
            }) else {
                return;
            };
            let Some(root) = root else {
                return;
            };
            let mut keep: Vec<String> = Vec::new();
            if let Some(ready) = ready {
                keep.push(ready);
            }
            if let Some(active) = active {
                keep.push(active);
            }
            let _ = cleanup_executor
                .spawn(async move {
                    let keep_refs: Vec<&str> = keep.iter().map(String::as_str).collect();
                    update::second_cleanup_keeping(&root, &keep_refs)
                })
                .await;
        }));
    }

    pub(super) fn apply_startup_scan(&mut self, scan: update::StartupScan, cx: &mut Context<Self>) {
        if let Some(root) = scan.root {
            self.update.root = Some(root);
        }
        if let Some(error) = scan.error {
            // A cleanup error is diagnostic only; the app must still start.
            eprintln!("voxhold-update: {error}");
        }
        if let Some(warning) = scan.warning {
            eprintln!("voxhold-update: {warning}");
        }
        if let Some(staged) = scan.ready {
            if matches!(
                &self.update.phase,
                UpdatePhase::Downloading { .. }
                    | UpdatePhase::Preparing { .. }
                    | UpdatePhase::Installing
            ) {
                return;
            }
            self.update.ready = Some(staged.clone());
            // A startup scan can finish after the 3 s check started. The
            // staged update wins; invalidate the older check answer.
            self.update.generation = self.update.generation.wrapping_add(1);
            if let Some(cancel) = &self.update.cancel {
                cancel.cancel();
            }
            self.update.cancel = None;
            self.update.phase = UpdatePhase::Ready {
                version: staged.version.to_string(),
                staged,
            };
            cx.notify();
        }
    }

    /// Fetch and verify the signed manifest, one check at a time.
    ///
    /// Both the 3 s / 6 h clock and the manual button call this entry point.
    /// An in-flight check or download is never duplicated: a second call
    /// leaves the visible phase untouched so the panel keeps showing it.
    pub(super) fn check_updates(&mut self, cx: &mut Context<Self>) {
        if matches!(
            &self.update.phase,
            UpdatePhase::Checking
                | UpdatePhase::Downloading { .. }
                | UpdatePhase::Preparing { .. }
                | UpdatePhase::Ready { .. }
                | UpdatePhase::Installing
        ) {
            return;
        }
        if matches!(
            &self.update.phase,
            UpdatePhase::Error {
                retry: RetryKind::Install,
                ..
            }
        ) {
            return;
        }
        let (manifest_url, signature_url) = match update::update_urls() {
            Ok(urls) => urls,
            Err(error) => {
                self.fail_check(error, cx);
                return;
            }
        };
        let key = match embedded_public_key() {
            Ok(key) => key,
            Err(error) => {
                self.fail_check(error, cx);
                return;
            }
        };
        self.update.generation = self.update.generation.wrapping_add(1);
        let generation = self.update.generation;
        let cancel = CancelToken::new();
        self.update.cancel = Some(cancel.clone());
        self.update.cancelling = false;
        self.update.phase = UpdatePhase::Checking;
        cx.notify();
        let runtime = self.runtime.clone();
        self.update.work_task = Some(cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn(async move {
                    check_signed_manifest(CheckRequest {
                        manifest_url: &manifest_url,
                        signature_url: &signature_url,
                        key: &key,
                        cancel,
                    })
                    .await
                })
                .await
                .unwrap_or_else(|_| Err(UpdateError::Network("update task failed".into())));
            let _ = this.update(cx, |view, cx| view.finish_check(generation, result, cx));
        }));
    }

    fn fail_check(&mut self, error: UpdateError, cx: &mut Context<Self>) {
        self.update.cancel = None;
        self.update.phase = UpdatePhase::Error {
            message: update::message_for_error(&error),
            retry: RetryKind::Check,
        };
        cx.notify();
    }

    pub(super) fn finish_check(
        &mut self,
        generation: u64,
        result: Result<VerifiedManifest, UpdateError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.update.generation {
            return;
        }
        self.update.cancel = None;
        match result {
            Ok(manifest) => {
                let current = update::current_version();
                let windows = WindowsVersion::current();
                match update::evaluate_manifest(
                    &manifest,
                    &current,
                    self.update.declined.as_ref(),
                    windows.as_ref(),
                ) {
                    Ok(phase) => {
                        self.update.verified = Some(manifest);
                        let auto = match &phase {
                            UpdatePhase::Available { version, .. }
                                if update::should_auto_download(
                                    &self.update.prefs,
                                    version,
                                    self.update.declined.as_ref(),
                                    self.update.auto_suppressed.as_deref(),
                                    self.update.auto_started.as_deref(),
                                ) =>
                            {
                                self.update.auto_started = Some(version.clone());
                                true
                            }
                            _ => false,
                        };
                        self.update.phase = phase;
                        if auto {
                            // The same `stage_update` path as the manual
                            // button: delta selection, hash checks and the
                            // fallback to the full package are not duplicated.
                            self.start_download(cx);
                            return;
                        }
                    }
                    Err(error) => {
                        self.update.phase = UpdatePhase::Error {
                            message: update::message_for_error(&error),
                            retry: RetryKind::Check,
                        };
                    }
                }
            }
            Err(UpdateError::Cancelled) => {
                self.update.phase = UpdatePhase::Idle;
            }
            Err(error) => {
                self.update.phase = UpdatePhase::Error {
                    message: update::message_for_error(&error),
                    retry: RetryKind::Check,
                };
            }
        }
        cx.notify();
    }

    /// Start the explicit download of the available version.
    pub(super) fn start_download(&mut self, cx: &mut Context<Self>) {
        let Some(manifest) = self.update.verified.clone() else {
            return;
        };
        let version = match &self.update.phase {
            UpdatePhase::Available { version, .. } => version.clone(),
            UpdatePhase::Error {
                retry: RetryKind::Download,
                ..
            } => match &self.update.verified {
                Some(manifest) => manifest.version.clone(),
                None => return,
            },
            _ => return,
        };
        let root = match self
            .update
            .root_override
            .clone()
            .map(Ok)
            .unwrap_or_else(updates_root)
        {
            Ok(root) => root,
            Err(error) => {
                self.update.phase = UpdatePhase::Error {
                    message: update::message_for_error(&error),
                    retry: RetryKind::Download,
                };
                cx.notify();
                return;
            }
        };
        self.update.root = Some(root.clone());
        let install_dir = self.update.install_dir.clone();
        let installed = read_package_json_from_dir(&install_dir).ok();
        let current = update::current_version();
        let cancel = CancelToken::new();
        self.update.cancel = Some(cancel.clone());
        self.update.cancelling = false;
        self.update.generation = self.update.generation.wrapping_add(1);
        let generation = self.update.generation;
        self.update.download_starts = self.update.download_starts.saturating_add(1);
        let progress = Arc::new(ProgressCounters::new(manifest.full.size));
        self.update.progress = Some(progress.clone());
        self.update.phase = UpdatePhase::Downloading {
            version,
            downloaded: 0,
            total: manifest.full.size,
        };
        cx.notify();
        if !self.update.download_task_enabled {
            return;
        }

        let runtime = self.runtime.clone();
        let phase_progress = progress.clone();
        self.update.work_task = Some(cx.spawn(async move |this, cx| {
            let outcome = runtime
                .spawn(async move {
                    stage_update(StageRequest {
                        manifest: &manifest,
                        channel: update::CHANNEL,
                        current_version: &current,
                        install_dir: &install_dir,
                        installed: installed.as_ref(),
                        updates_root: &root,
                        cancel,
                        progress: Some(Arc::new(move |downloaded, total| {
                            progress.set(downloaded, total);
                        })),
                        phase: Some(Arc::new(move |phase| match phase {
                            // A failed delta restarts the whole download; the
                            // new total must replace the delta counters.
                            StagePhase::Downloading { total } => phase_progress.restart(total),
                            StagePhase::Preparing => {
                                phase_progress.set_phase(StagePhase::Preparing)
                            }
                        })),
                    })
                    .await
                })
                .await;
            let _ = this.update(cx, |view, cx| match outcome {
                Ok(outcome) => view.finish_download(generation, outcome, cx),
                Err(_) => view.finish_download(
                    generation,
                    StageOutcome::Failed(UpdateError::Apply("download task failed".into())),
                    cx,
                ),
            });
        }));

        let progress = self.update.progress.clone().expect("progress");
        let executor = cx.background_executor().clone();
        self.update.progress_task = Some(cx.spawn(async move |this, cx| loop {
            executor.timer(PROGRESS_TICK).await;
            let keep = this
                .update(cx, |view, cx| view.poll_progress(generation, &progress, cx))
                .unwrap_or(false);
            if !keep {
                break;
            }
        }));
    }

    /// Mirror the staging phase and counters into the visible state.
    ///
    /// The poller lives through `Preparing`: a delta that fails after its body
    /// arrived restarts `stage_full`, and the next `Downloading` report has to
    /// bring back percentages and the cancel button for the full package.
    pub(super) fn poll_progress(
        &mut self,
        generation: u64,
        progress: &ProgressCounters,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.update.generation {
            return false;
        }
        let version = match &self.update.phase {
            UpdatePhase::Downloading { version, .. } | UpdatePhase::Preparing { version } => {
                version.clone()
            }
            _ => return false,
        };
        let (downloaded, total) = progress.snapshot();
        let next = match progress.phase() {
            StagePhase::Preparing => UpdatePhase::Preparing { version },
            StagePhase::Downloading { .. } => UpdatePhase::Downloading {
                version,
                downloaded: downloaded.min(total),
                total,
            },
        };
        if same_visible_phase(&self.update.phase, &next) {
            return true;
        }
        self.update.phase = next;
        cx.notify();
        true
    }

    pub(super) fn finish_download(
        &mut self,
        generation: u64,
        outcome: StageOutcome,
        cx: &mut Context<Self>,
    ) {
        if generation != self.update.generation {
            return;
        }
        self.update.cancel = None;
        self.update.cancelling = false;
        self.update.progress = None;
        self.update.progress_task = None;
        match outcome {
            StageOutcome::Ready(staged) => {
                self.update.ready = Some(staged.clone());
                self.update.phase = UpdatePhase::Ready {
                    version: staged.version.to_string(),
                    staged,
                };
            }
            StageOutcome::UpToDate => {
                self.update.phase = UpdatePhase::UpToDate;
            }
            StageOutcome::Failed(UpdateError::Cancelled) => {
                self.update.phase = self.available_again();
            }
            StageOutcome::Failed(error) => {
                self.update.phase = UpdatePhase::Error {
                    message: update::message_for_error(&error),
                    retry: RetryKind::Download,
                };
            }
        }
        cx.notify();
    }

    fn available_again(&self) -> UpdatePhase {
        match &self.update.verified {
            Some(manifest) => UpdatePhase::Available {
                version: manifest.version.clone(),
                notes: manifest.notes.clone(),
            },
            None => UpdatePhase::Idle,
        }
    }

    /// Ask the library to stop and remove the partial staging directory.
    ///
    /// This works while a body is downloading and while a finished body is
    /// being hashed/unpacked: a failed delta fallback still owns the cancel
    /// token during its full re-download. The cancelled version is remembered
    /// for the rest of this process: the 6 h clock must not start the same
    /// automatic download again. The manual button stays available and is not
    /// affected.
    pub(super) fn cancel_download(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.update.cancel {
            if let Some(version) = self.update.phase.downloading_version() {
                self.update.auto_suppressed = Some(version.to_string());
            }
            cancel.cancel();
            self.update.cancelling = true;
            cx.notify();
        }
    }

    /// Remember the skipped version in `%APPDATA%` atomically.
    pub(super) fn skip_version(&mut self, cx: &mut Context<Self>) {
        let Some(version) = self
            .update
            .phase
            .offered_version()
            .and_then(|value| semver::Version::parse(value).ok())
        else {
            return;
        };
        let mut prefs = self.update.prefs.clone();
        prefs.set_declined(&version);
        if update::save_prefs(self.update.prefs_path.as_deref(), &prefs).is_err() {
            self.update.phase = UpdatePhase::Error {
                message: "Не удалось сохранить выбор версии.".into(),
                retry: RetryKind::Check,
            };
        } else {
            self.update.prefs = prefs;
            self.update.declined = Some(version);
            self.update.phase = UpdatePhase::UpToDate;
        }
        cx.notify();
    }

    /// Flip the background download switch and write the whole document.
    pub(super) fn toggle_auto_download(&mut self, cx: &mut Context<Self>) {
        let mut prefs = self.update.prefs.clone();
        prefs.auto_download = !prefs.auto_download;
        self.persist_prefs(prefs, cx);
    }

    /// Flip the install-on-next-start switch and write the whole document.
    pub(super) fn toggle_install_on_next_start(&mut self, cx: &mut Context<Self>) {
        let mut prefs = self.update.prefs.clone();
        prefs.install_on_next_start = !prefs.install_on_next_start;
        self.persist_prefs(prefs, cx);
    }

    /// Save preferences without losing the skipped version or the other switch.
    fn persist_prefs(&mut self, prefs: UpdatePrefs, cx: &mut Context<Self>) {
        match update::save_prefs(self.update.prefs_path.as_deref(), &prefs) {
            Ok(()) => {
                self.update.declined = prefs.declined();
                self.update.prefs = prefs;
                self.update.prefs_error = None;
            }
            Err(_) => {
                self.update.prefs_error = Some("Не удалось сохранить настройки обновлений.".into())
            }
        }
        cx.notify();
    }

    /// Retry the action that failed.
    pub(super) fn retry_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let retry = match &self.update.phase {
            UpdatePhase::Error { retry, .. } => *retry,
            _ => return,
        };
        match retry {
            RetryKind::Check => self.check_updates(cx),
            RetryKind::Download => self.start_download(cx),
            RetryKind::Install => self.install_ready(window, cx),
        }
    }

    /// Stop voice, media and preview work and persist the current settings.
    fn prepare_for_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "voxhold-media")]
        self.abandon_voice(window, cx);
        self.stop_voice_preview();
        self.stop_voice_meter();
        if self.recordings_dialog.is_some() {
            self.close_recordings(false, window, cx);
        }
        if self.media.viewer.is_some() {
            self.close_viewer(window);
        }
        self.preview_wakeup = None;
        self.preview_wake_at = None;
        self.save_shell();
        // Atomic and synchronous: the helper may replace the installation as
        // soon as this process exits.
        self.voice_settings.flush();
    }

    /// Launch the helper for a ready staged update.
    pub(super) fn install_ready(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let staged = match &self.update.phase {
            UpdatePhase::Ready { staged, .. } => staged.clone(),
            _ => match &self.update.ready {
                Some(staged) => staged.clone(),
                None => return,
            },
        };
        let helper = staged.app_dir.join(HELPER_NAME);
        if !helper.is_file() {
            self.update.phase = UpdatePhase::Error {
                message: "В обновлении нет установщика.".into(),
                retry: RetryKind::Install,
            };
            cx.notify();
            return;
        }
        if staged.version <= update::current_version() {
            self.update.phase = UpdatePhase::Error {
                message: "Обновление не новее установленной версии.".into(),
                retry: RetryKind::Check,
            };
            cx.notify();
            return;
        }
        let install_dir = self.update.install_dir.clone();
        let staging_dir = staged.staging_dir.clone();
        let handshake = staging_dir.join(HANDSHAKE_NAME);
        if !install_dir.is_absolute() || !staging_dir.is_absolute() || !handshake.is_absolute() {
            self.update.phase = UpdatePhase::Error {
                message: "Путь установки не абсолютный.".into(),
                retry: RetryKind::Install,
            };
            cx.notify();
            return;
        }
        // The helper copies the running image back into the installation by
        // this name. A build under a different name (`voxhold-native-rust.exe`)
        // must not make it fail after this process has already quit.
        let Some(exe_name) = packaged_exe_name(&staged.app_dir, &self.update.exe_name) else {
            self.update.phase = UpdatePhase::Error {
                message: format!(
                    "В обновлении нет исполняемого файла приложения ({}).",
                    self.update.exe_name
                ),
                retry: RetryKind::Install,
            };
            cx.notify();
            return;
        };
        self.prepare_for_install(window, cx);
        let pid = std::process::id();
        let request = LaunchRequest {
            exe: helper,
            args: vec![
                "--pid".into(),
                OsString::from(pid.to_string()),
                "--target".into(),
                install_dir.into_os_string(),
                "--staged".into(),
                staging_dir.into_os_string(),
                "--exe".into(),
                OsString::from(exe_name),
                "--handshake".into(),
                handshake.into_os_string(),
            ],
        };
        self.update.quit_requested = false;
        self.update.phase = UpdatePhase::Installing;
        cx.notify();
        match self.update.launcher.launch(&request) {
            Ok(()) => {
                self.update.quit_requested = true;
                cx.quit();
            }
            Err(message) => {
                self.update.phase = UpdatePhase::Error {
                    message,
                    retry: RetryKind::Install,
                };
                cx.notify();
            }
        }
    }

    pub(super) fn toggle_update_panel(&mut self, cx: &mut Context<Self>) {
        if self.update.panel_open {
            self.close_update_panel(cx);
        } else {
            self.open_update_panel(cx);
        }
    }

    pub(super) fn open_update_panel(&mut self, cx: &mut Context<Self>) {
        // Opening the popup only shows the process-wide state. The 3 s / 6 h
        // clock already owns checking; the panel never starts another timer
        // or an implicit check.
        self.update.panel_open = true;
        self.update.hover = false;
        cx.notify();
    }

    /// Close the popup. Checks and downloads belong to the process, not to
    /// this surface, so closing it changes nothing else.
    pub(super) fn close_update_panel(&mut self, cx: &mut Context<Self>) {
        self.update.panel_open = false;
        self.update.hover = false;
        cx.notify();
    }

    /// A logout only hides the popup.
    ///
    /// The update lifecycle is process-wide: a background download and a
    /// staged package must survive a session change. The generation still
    /// discards late answers of a newer manual check, but logout must not
    /// invalidate it.
    pub(super) fn end_update_session(&mut self) {
        self.update.panel_open = false;
        self.update.hover = false;
    }

    pub(crate) fn report_handshake_failure(&mut self, message: String) {
        self.update.handshake_error = Some(message.clone());
        self.error = format!("Не удалось подтвердить запуск обновления: {message}");
        self.auth.status = self.error.clone();
    }
}

/// Whether two visible phases are indistinguishable for the poller.
fn same_visible_phase(left: &UpdatePhase, right: &UpdatePhase) -> bool {
    match (left, right) {
        (
            UpdatePhase::Downloading {
                version: left_version,
                downloaded: left_downloaded,
                total: left_total,
            },
            UpdatePhase::Downloading {
                version: right_version,
                downloaded: right_downloaded,
                total: right_total,
            },
        ) => {
            left_version == right_version
                && left_downloaded == right_downloaded
                && left_total == right_total
        }
        (UpdatePhase::Preparing { version: left }, UpdatePhase::Preparing { version: right }) => {
            left == right
        }
        _ => false,
    }
}

/// Whether the startup observer may finish the operation it finds on disk.
///
/// A normal start has nothing to prove and always recovers. An update start
/// may only complete the operation once its handshake file exists; otherwise
/// the state must stay unconfirmed and keep the backup for the helper.
pub(super) fn startup_recovery_allowed(after_update: bool, handshake_written: bool) -> bool {
    !after_update || handshake_written
}

/// The image name the helper must copy back into the installation.
///
/// Prefers the name this process was started with, but only when the package
/// actually contains it. A development build (`voxhold-native-rust.exe`) must
/// fall back to the release name instead of failing after the app quit.
fn packaged_exe_name(app_dir: &Path, running: &str) -> Option<String> {
    if !running.is_empty() && app_dir.join(running).is_file() {
        return Some(running.to_string());
    }
    if running != PACKAGE_EXE_NAME && app_dir.join(PACKAGE_EXE_NAME).is_file() {
        return Some(PACKAGE_EXE_NAME.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_checks_after_three_seconds_and_then_every_six_hours() {
        assert_eq!(START_DELAY, Duration::from_secs(3));
        assert_eq!(CHECK_INTERVAL, Duration::from_secs(6 * 60 * 60));
    }

    #[test]
    fn a_full_progress_reaches_preparing_and_zero_total_does_not() {
        assert!(!update::download_complete(0, 0));
        assert!(!update::download_complete(512, 1024));
        assert!(update::download_complete(1024, 1024));
    }
}
