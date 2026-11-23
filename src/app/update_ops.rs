//! Background update controller: start-up cleanup, periodic checks, download,
//! cancellation and launching the helper.
//!
//! The UI thread never waits on the network or on the filesystem. A generation
//! discards late answers of a superseded manual check or download. Logging out
//! and closing the panel only hide the surface: neither drops a check, a
//! download or a staged package, so the generation is not bumped by them.

use super::update_install::acquire_install_lock;
use super::update_progress::{phase_counter, ProgressCounters};
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
    installation_id, stage_update, updates_root, updates_root_for, StageOutcome, StagePhase,
    StageRequest, StagedUpdate,
};
use voxhold_update::verify::{embedded_public_key, UpdatePublicKey};

const START_DELAY: Duration = Duration::from_secs(3);
/// Startup budget for finding and verifying a ready package. A slower disk
/// continues the normal start; the late result only updates the panel.
const STARTUP_SCAN_BUDGET: Duration = Duration::from_millis(500);
/// Upper bound for showing a scan that missed the 500 ms budget. A hung disk
/// must not delay the 3 s check clock forever.
const LATE_SCAN_WAIT: Duration = Duration::from_secs(5);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const SECOND_CLEANUP_DELAY: Duration = Duration::from_secs(10);
const PROGRESS_TICK: Duration = Duration::from_millis(100);
pub(super) const HELPER_NAME: &str = "voxhold-updater.exe";
pub(super) const HELPER_READY_NAME: &str = "helper-ready.ok";
pub(super) const HELPER_READY_TIMEOUT: Duration = Duration::from_secs(15);
const PACKAGE_EXE_NAME: &str = "Voxhold.exe";
pub(super) const HANDSHAKE_NAME: &str = "handshake.ok";
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
    /// Versions this process already auto-downloaded once.
    pub auto_started: update::VersionSet,
    /// Versions whose automatic download the user cancelled this run.
    pub auto_suppressed: update::VersionSet,
    pub panel_open: bool,
    pub hover: bool,
    pub prefs_error: Option<String>,
    pub prefs_path: Option<PathBuf>,
    /// Helper sidecar that records an automatic-install refusal after a
    /// failed apply. It falls back to the installation directory when no
    /// per-user preference path exists.
    pub refusal_path: Option<PathBuf>,
    /// Named mutex held for this installation. `false` means another process
    /// owns the same installation, so this one must not clean staging, start a
    /// download or install automatically.
    pub single_instance: bool,
    #[cfg(windows)]
    pub install_lock: Option<isize>,
    pub install_dir: PathBuf,
    pub exe_name: String,
    /// Test seam: use a temporary per-install updates root instead of
    /// `LOCALAPPDATA\VoxHold\updates\<installation-id>`.
    pub root_override: Option<PathBuf>,
    /// Test seam: verification key for the staged proof. `None` uses the
    /// embedded release key.
    pub proof_key: Option<UpdatePublicKey>,
    /// Test seam: observe or fail the helper launch.
    pub launcher: Box<dyn UpdateLauncher>,
    /// Test seam: a successful launch asked the app to quit.
    pub quit_requested: bool,
    /// Test seam: how many download requests passed the phase check.
    pub download_starts: u32,
    /// Test seam: when false, a download records the start but does not spawn
    /// the staging task, so tests never touch a real network.
    pub download_task_enabled: bool,
    /// Test seam: production waits for `helper-ready.ok` before quitting.
    pub wait_for_helper_ready: bool,
    /// A manual install during a call needs one explicit confirmation.
    pub confirm_install: bool,
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
            auto_started: update::VersionSet::new(),
            auto_suppressed: update::VersionSet::new(),
            panel_open: false,
            hover: false,
            prefs_error: None,
            refusal_path: Some(update::refusal_path(prefs_path.as_deref(), &install_dir)),
            single_instance: true,
            #[cfg(windows)]
            install_lock: None,
            prefs_path,
            install_dir,
            exe_name,
            root_override: None,
            proof_key: None,
            launcher: Box::new(ProcessUpdateLauncher),
            quit_requested: false,
            download_starts: 0,
            download_task_enabled: true,
            wait_for_helper_ready: !cfg!(test),
            confirm_install: false,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The helper can leave a refusal marker after a failed apply. Only the
        // process that owns the installation mutex merges it; a second
        // process leaves the shared preference file alone.
        let install = self.update.install_dir.clone();
        #[cfg(windows)]
        if self.update.install_lock.is_none() {
            match acquire_install_lock(&install) {
                Some(handle) => self.update.install_lock = Some(handle),
                None => self.update.single_instance = false,
            }
        }
        let single_instance = self.update.single_instance;
        if single_instance {
            self.merge_install_refusal();
        }
        let root_override = self
            .update
            .root_override
            .clone()
            .or_else(|| updates_root_for(&install).ok());
        let current = update::current_version();
        // A second process of the same installation must not finish or clean
        // an operation the owner is running.
        let recovery_allowed =
            startup_recovery_allowed(after_update, handshake_written) && single_instance;
        let executor = cx.background_executor().clone();
        // Run the disk scan off the UI thread. The first 500 ms are the only
        // window for an automatic install decision; a late answer may still
        // update the panel but cannot make this launch quit.
        let slot = Arc::new(Mutex::new(None::<update::StartupScan>));
        let thread_slot = slot.clone();
        std::thread::spawn(move || {
            let scan = update::startup_scan(
                &install,
                &current,
                root_override.as_deref(),
                recovery_allowed,
                single_instance,
            );
            if let Ok(mut slot) = thread_slot.lock() {
                *slot = Some(scan);
            }
        });
        self.update.clock_task = Some(cx.spawn_in(window, async move |this, window| {
            executor.timer(STARTUP_SCAN_BUDGET).await;
            let early = slot.lock().ok().and_then(|mut slot| slot.take());
            let mut auto = false;
            if let Some(scan) = early {
                auto = this
                    .update_in(window, |view, window, cx| {
                        let auto = view.apply_startup_scan_auto(scan, true, cx);
                        if auto {
                            view.install_ready_confirmed(window, cx, true);
                        }
                        // The clock must stop only when the helper really
                        // started; a launch error keeps the app running and
                        // the 3 s / 6 h checks alive.
                        auto && view.install_started()
                    })
                    .unwrap_or(false);
            } else {
                // The decision is closed now. Keep a short, bounded wait for
                // the late scan so its result can still show "Ready" in the
                // panel; a hung disk must not keep the clock from starting.
                let deadline = std::time::Instant::now() + LATE_SCAN_WAIT;
                loop {
                    executor.timer(Duration::from_millis(100)).await;
                    let late = slot.lock().ok().and_then(|mut slot| slot.take());
                    if let Some(scan) = late {
                        let _ = this.update_in(window, |view, window, cx| {
                            view.apply_startup_scan_auto(scan, false, cx);
                            let _ = window;
                        });
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                }
            }
            if auto {
                // The helper owns the next step; the normal clock would only
                // race the process exit.
                return;
            }
            executor.timer(START_DELAY).await;
            let _ = this.update_in(window, |view, _, cx| view.check_updates(cx));
            loop {
                executor.timer(CHECK_INTERVAL).await;
                let _ = this.update_in(window, |view, _, cx| view.check_updates(cx));
            }
        }));

        let executor = cx.background_executor().clone();
        let cleanup_executor = cx.background_executor().clone();
        self.update.cleanup_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(SECOND_CLEANUP_DELAY).await;
            let Ok((root, ready, active, single_instance)) = this.update(cx, |view, _| {
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
                    view.update.single_instance,
                )
            }) else {
                return;
            };
            // A second process only observes; it never deletes the staging
            // tree of the instance that owns the update mutex.
            if !single_instance {
                return;
            }
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

    #[cfg(test)]
    pub(super) fn apply_startup_scan(&mut self, scan: update::StartupScan, cx: &mut Context<Self>) {
        self.apply_startup_scan_auto(scan, false, cx);
    }

    /// Apply the startup scan and optionally decide that this launch installs.
    ///
    /// `allow_auto` is false for a result that arrives after the 500 ms budget:
    /// the package is still shown, but the launch must continue normally.
    pub(super) fn apply_startup_scan_auto(
        &mut self,
        scan: update::StartupScan,
        allow_auto: bool,
        cx: &mut Context<Self>,
    ) -> bool {
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
        let Some(staged) = scan.ready else {
            return false;
        };
        if matches!(
            &self.update.phase,
            UpdatePhase::Downloading { .. }
                | UpdatePhase::Preparing { .. }
                | UpdatePhase::Installing
        ) {
            return false;
        }
        self.update.ready = Some(staged.clone());
        // A startup scan can finish after the 3 s check started. The
        // staged update wins; invalidate the older check answer.
        self.update.generation = self.update.generation.wrapping_add(1);
        if let Some(cancel) = &self.update.cancel {
            cancel.cancel();
        }
        self.update.cancel = None;
        let auto = allow_auto
            && self.update.single_instance
            && update::should_auto_install(&self.update.prefs, &staged, &update::current_version());
        self.update.phase = UpdatePhase::Ready {
            version: staged.version.to_string(),
            staged,
        };
        cx.notify();
        auto
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
                                    &self.update.auto_suppressed,
                                    &self.update.auto_started,
                                    self.update.single_instance,
                                ) =>
                            {
                                self.update.auto_started.insert(version.clone());
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
    ///
    /// The manual button is refused for a process that does not own the
    /// per-installation mutex: the staging directory belongs to the instance
    /// that does, and `stage_update` must not remove it. Nothing on disk is
    /// touched in that case.
    pub(super) fn start_download(&mut self, cx: &mut Context<Self>) {
        if !self.update.single_instance {
            self.update.phase = UpdatePhase::Error {
                message: "Обновление уже скачивает другая копия приложения.".into(),
                retry: RetryKind::Download,
            };
            cx.notify();
            return;
        }
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
        let install_dir = self.update.install_dir.clone();
        // `stage_update` takes the shared base and adds the installation id
        // itself. The cleanup task needs the per-install root.
        // `root_override` is therefore a *base* here, while `start_update`
        // treats it as the per-install root that directly holds versions.
        let base_root = match self
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
        self.update.root = Some(base_root.join(installation_id(&install_dir)));
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
                        updates_root: &base_root,
                        cancel,
                        progress: Some(Arc::new(move |downloaded, total| {
                            progress.set(downloaded, total);
                        })),
                        phase: Some(phase_counter(phase_progress)),
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
    /// token during its full re-download. Cancelled versions are remembered in
    /// a process-wide set: the 6 h clock must not start the same automatic
    /// download again, and a newer cancel keeps the older memory. The manual
    /// button stays available and is not affected.
    pub(super) fn cancel_download(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.update.cancel {
            if let Some(version) = self.update.phase.downloading_version() {
                self.update.auto_suppressed.insert(version.to_string());
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
            self.update.prefs_error = None;
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
pub(super) fn packaged_exe_name(app_dir: &Path, running: &str) -> Option<String> {
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
    fn the_phase_callback_restarts_and_reaches_preparing() {
        let progress = Arc::new(ProgressCounters::new(771));
        let report = phase_counter(progress.clone());
        progress.set(512, Some(771));
        report(StagePhase::Preparing);
        assert_eq!(progress.phase(), StagePhase::Preparing);
        assert_eq!(progress.snapshot(), (512, 771));
        report(StagePhase::Downloading { total: 4_000 });
        assert_eq!(progress.phase(), StagePhase::Downloading { total: 4_000 });
        assert_eq!(progress.snapshot(), (0, 4_000));
    }
}
