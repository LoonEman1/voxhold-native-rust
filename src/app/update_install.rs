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
        let confirmed = confirmed || self.update.confirm_install;
        if !confirmed && self.voice_call_active_for_power(cx) {
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
            .unwrap_or_else(|| staging_dir.join("refusal.json"));
        let _ = std::fs::remove_file(&ready);
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
                "--ready".into(),
                ready.clone().into_os_string(),
                "--refusal".into(),
                refusal.into_os_string(),
                "--version".into(),
                OsString::from(staged.version.to_string()),
            ],
        };
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
            let deadline = HELPER_READY_TIMEOUT;
            let step = Duration::from_millis(100);
            let ticks = deadline.as_millis() / step.as_millis();
            for _ in 0..ticks {
                executor.timer(step).await;
                if ready.is_file() {
                    let _ = this.update_in(window, |view, window, cx| {
                        view.prepare_for_install(window, cx);
                        view.update.quit_requested = true;
                        cx.quit();
                    });
                    return;
                }
            }
            let _ = this.update_in(window, |view, _, cx| {
                view.update.phase = UpdatePhase::Error {
                    message: "Установщик не подтвердил готовность.".into(),
                    retry: RetryKind::Install,
                };
                cx.notify();
            });
        }));
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
