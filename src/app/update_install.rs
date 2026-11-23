//! Helper handoff: stop media, launch the installed helper and wait for readiness.

use super::update_ops::{
    packaged_exe_name, LaunchRequest, UpdateUi, HANDSHAKE_NAME, HELPER_NAME, HELPER_READY_NAME,
    HELPER_READY_TIMEOUT,
};
use super::*;
use crate::update::{self, RetryKind, UpdatePhase};
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

impl Voxhold {
    /// Whether installing now would interrupt a live call or screen share.
    ///
    /// Both stop working when this process exits, so the manual path asks for
    /// a second click in either case.
    fn transmission_active_for_install(&self, cx: &Context<Self>) -> bool {
        self.voice.as_ref().is_some_and(|panel| {
            let panel = panel.read(cx);
            panel.call_active() || panel.share_active()
        })
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
        self.install_ready_confirmed(window, cx, false);
    }

    /// Manual entry point: during a call or stream the first click only asks
    /// for confirmation, the second one installs.
    pub(super) fn install_ready_confirmed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        confirmed: bool,
    ) {
        // A second process of the same installation does not own the staging
        // tree or the installation. Starting a helper here would make it fight
        // the running instance and destroy the shared staging directory.
        if !self.update.single_instance {
            self.update.confirm_install = false;
            self.update.phase = UpdatePhase::Error {
                message: "Обновление уже устанавливается в другом экземпляре Voxhold.".into(),
                retry: RetryKind::Install,
            };
            cx.notify();
            return;
        }
        // A helper was already started by this process. After a readiness
        // timeout it may still be waiting for this PID or already copying, so
        // a retry must not spawn a second one until the app restarts.
        if self.update.helper_pending {
            self.update.confirm_install = false;
            self.update.phase = UpdatePhase::Error {
                message: "Установщик уже запущен, дождитесь завершения или перезапустите Voxhold."
                    .into(),
                retry: RetryKind::Install,
            };
            cx.notify();
            return;
        }
        let confirmed = confirmed || self.update.confirm_install;
        if !confirmed && self.transmission_active_for_install(cx) {
            self.update.confirm_install = true;
            cx.notify();
            return;
        }
        self.update.confirm_install = false;
        let staged = match &self.update.phase {
            UpdatePhase::Ready { staged, .. } => staged.clone(),
            _ => match &self.update.ready {
                Some(staged) => staged.clone(),
                None => return,
            },
        };
        if !staged.verified {
            self.update.phase = UpdatePhase::Error {
                message: "Обновление не прошло проверку подписи.".into(),
                retry: RetryKind::Check,
            };
            cx.notify();
            return;
        }
        let install_dir = self.update.install_dir.clone();
        // The startup scan verified the package when this process started; a
        // long session or a local writer can still change the staging tree.
        // Repeat every offline check immediately before handing over control.
        let proof_valid = match &self.update.proof_key {
            Some(key) => update::reverify_staged_with(&staged, &install_dir, key),
            None => update::reverify_staged(&staged, &install_dir),
        };
        if !proof_valid {
            self.update.phase = UpdatePhase::Error {
                message:
                    "Подготовленное обновление изменилось после проверки. Скачайте его заново."
                        .into(),
                retry: RetryKind::Check,
            };
            cx.notify();
            return;
        }
        // Run the helper shipped with the installed version: it knows its own
        // relocation rules. A package-local helper is only a fallback for a
        // development install that has not shipped one yet.
        let helper = if install_dir.join(HELPER_NAME).is_file() {
            install_dir.join(HELPER_NAME)
        } else {
            staged.app_dir.join(HELPER_NAME)
        };
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
        let staging_dir = staged.staging_dir.clone();
        let handshake = staging_dir.join(HANDSHAKE_NAME);
        let ready = staging_dir.join(HELPER_READY_NAME);
        let refusal = self
            .update
            .refusal_path
            .clone()
            .unwrap_or_else(|| update::refusal_path(None, &install_dir));
        // A marker left by an earlier attempt would make the poll think the
        // helper is ready before it really is. A marker that cannot be removed
        // must stop the launch: the wait could otherwise finish immediately.
        match std::fs::remove_file(&ready) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                self.update.phase = UpdatePhase::Error {
                    message: "Не удалось подготовить установщик: файл готовности занят.".into(),
                    retry: RetryKind::Install,
                };
                cx.notify();
                return;
            }
        }
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
        let pid = std::process::id();
        let mut args = vec![
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
            "--ready".into(),
            ready.clone().into_os_string(),
            "--refusal".into(),
            refusal.into_os_string(),
            "--version".into(),
            OsString::from(staged.version.to_string()),
        ];
        // The helper centres its own window in the work area of the monitor
        // that showed Voxhold, so the two windows do not jump between screens.
        if let Some(monitor) = monitor_work_area_argument(window) {
            args.push("--monitor".into());
            args.push(OsString::from(monitor));
        }
        let request = LaunchRequest { exe: helper, args };
        self.update.quit_requested = false;
        self.update.phase = UpdatePhase::Installing;
        cx.notify();
        if let Err(message) = self.update.launcher.launch(&request) {
            self.update.phase = UpdatePhase::Error {
                message,
                retry: RetryKind::Install,
            };
            cx.notify();
            return;
        }
        // The process exists now. Even after a readiness timeout it may still
        // take over, so this app must not start another helper.
        self.update.helper_pending = true;
        if !self.update.wait_for_helper_ready {
            self.prepare_for_install(window, cx);
            self.update.quit_requested = true;
            cx.quit();
            return;
        }
        // The helper writes the marker after it has relocated, prepared its
        // window and is ready to take over. Only then may this process exit.
        let executor = cx.background_executor().clone();
        self.update.work_task = Some(cx.spawn_in(window, async move |this, window| {
            let step = Duration::from_millis(100);
            let ticks = HELPER_READY_TIMEOUT.as_millis() / step.as_millis();
            for tick in 0..ticks {
                executor.timer(step).await;
                let expired = tick + 1 == ticks;
                let done = this
                    .update_in(window, |view, window, cx| {
                        view.helper_ready_tick(window, cx, &ready, expired)
                    })
                    .unwrap_or(true);
                if done {
                    return;
                }
            }
        }));
    }

    /// One poll of `helper-ready.ok`, extracted so a test can drive it without
    /// waiting for the 15 s timer.
    ///
    /// Returns `true` when the wait is over: either the helper confirmed (the
    /// app stops media and quits) or the timeout expired (the app stays open
    /// with a retryable error).
    pub(super) fn helper_ready_tick(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        ready: &Path,
        expired: bool,
    ) -> bool {
        if ready.is_file() {
            self.prepare_for_install(window, cx);
            self.update.quit_requested = true;
            cx.quit();
            return true;
        }
        if expired {
            self.update.phase = UpdatePhase::Error {
                message: "Установщик не подтвердил готовность.".into(),
                retry: RetryKind::Install,
            };
            cx.notify();
            return true;
        }
        false
    }

    /// Whether the helper is taking over, so the check clock must stop.
    pub(super) fn install_started(&self) -> bool {
        self.update.quit_requested || matches!(self.update.phase, UpdatePhase::Installing)
    }

    /// Merge a helper refusal into the preferences before the startup decision.
    ///
    /// The in-memory value is set even when the preference file cannot be
    /// written, so this launch cannot retry the same automatic install; the
    /// sidecar is removed only after a successful save.
    pub(super) fn merge_install_refusal(&mut self) {
        let prefs_path = self.update.prefs_path.clone();
        let refusal = self.update.refusal_path.clone();
        update::merge_refusal(
            &mut self.update.prefs,
            prefs_path.as_deref(),
            refusal.as_deref(),
        );
    }
}

#[cfg(windows)]
impl Drop for UpdateUi {
    fn drop(&mut self) {
        if let Some(handle) = self.install_lock.take() {
            use windows_sys::Win32::Foundation::CloseHandle;
            unsafe {
                CloseHandle(handle as *mut core::ffi::c_void);
            }
        }
    }
}

/// Work area of the monitor that currently shows the client, formatted for the
/// helper's `--monitor` argument. `None` lets the helper use the primary work
/// area (tests and platforms without a native handle).
#[cfg(windows)]
fn monitor_work_area_argument(window: &Window) -> Option<String> {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    // The GPUI test window has no native handle.
    if cfg!(test) {
        return None;
    }
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return None;
    };
    let hwnd = win32.hwnd.get() as *mut core::ffi::c_void;
    if hwnd.is_null() {
        return None;
    }
    // SAFETY: a live native window handle; the monitor info struct is local.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let work = info.rcWork;
        Some(format!(
            "{},{},{},{}",
            work.left, work.top, work.right, work.bottom
        ))
    }
}

#[cfg(not(windows))]
fn monitor_work_area_argument(_window: &Window) -> Option<String> {
    None
}

/// Acquire the per-installation update mutex. `None` means another process of
/// the same portable copy is already running.
#[cfg(windows)]
pub(super) fn acquire_install_lock(install_dir: &Path) -> Option<isize> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name = format!(
        "Local\\VoxholdUpdate-{}",
        voxhold_update::stage::installation_id(install_dir)
    );
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
    if handle.is_null() {
        return None;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(handle);
        }
        return None;
    }
    Some(handle as isize)
}
