//! Windows tray notifications and their persisted preference.
//!
//! The Go client shows a `Shell_NotifyIconW` balloon, suppresses alerts while
//! the window has focus and remembers the last 128 shown keys. The native calls
//! are isolated here; the dedup/retry/enable rules are plain state and unit
//! tested with a fake sink.

use std::path::PathBuf;

use crate::settings::{notification_text, SeenNotifications};

pub fn default_path() -> Option<PathBuf> {
    crate::layout::Settings::default_path()
        .as_deref()
        .map(path_from_layout)
}

/// `<layout path>.notifications.json`, the file the Go client reads next to
/// its workspace settings. Tests pass a temporary layout path and therefore
/// never touch the user's real preference file.
pub fn path_from_layout(path: &std::path::Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".notifications.json");
    PathBuf::from(value)
}

#[derive(Debug)]
pub struct NotificationCenter {
    pub enabled: bool,
    pub error: String,
    path: Option<PathBuf>,
    seen: SeenNotifications,
    pub shown: u64,
    pub failed: u64,
}

impl Default for NotificationCenter {
    fn default() -> Self {
        Self {
            enabled: false,
            error: String::new(),
            path: default_path(),
            seen: SeenNotifications::default(),
            shown: 0,
            failed: 0,
        }
    }
}

impl NotificationCenter {
    pub fn with_path(path: Option<PathBuf>) -> Self {
        let enabled = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|value| serde_json::from_str::<bool>(&value).ok())
            .unwrap_or(false);
        Self {
            enabled,
            path,
            ..Self::default()
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        if self.enabled == enabled {
            return true;
        }
        let saved = if let Some(path) = self.path.as_deref() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(path, if enabled { "true" } else { "false" })
        } else {
            Ok(())
        };
        match saved {
            Ok(()) => {
                self.enabled = enabled;
                self.error.clear();
                if !enabled {
                    self.clear_seen();
                }
                true
            }
            Err(_) => {
                self.error = "Не удалось сохранить настройку уведомлений.".into();
                false
            }
        }
    }

    pub fn clear_seen(&mut self) {
        self.seen = SeenNotifications::default();
    }

    /// Decide whether this event should produce a balloon. `None` covers a
    /// disabled setting, an active window, an own message, a malformed event
    /// and a duplicate key. A key is remembered only after a successful show,
    /// so a failed native call can be retried by the next identical event.
    pub fn offer(
        &self,
        kind: &str,
        data: &serde_json::Value,
        current_user: &str,
        window_active: bool,
    ) -> Option<(String, String, String)> {
        if !self.enabled || window_active {
            return None;
        }
        let (key, title, body) = notification_text(kind, data, current_user)?;
        if self.seen.contains(&key) {
            return None;
        }
        let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if body.is_empty() {
            return None;
        }
        Some((key, title, body))
    }

    pub fn shown(&mut self, key: &str) {
        self.seen.remember(key);
        self.shown = self.shown.saturating_add(1);
        self.error.clear();
    }

    pub fn failed(&mut self, error: impl Into<String>) {
        self.error = error.into();
        self.failed = self.failed.saturating_add(1);
    }
}

/// Callback message the window procedure receives from `Shell_NotifyIconW`.
/// It sits in the `WM_APP` range so it cannot collide with a system message.
pub(crate) const TRAY_CALLBACK_MESSAGE: u32 = 0x8000 + 145;

const NIF_MESSAGE: u32 = 0x0000_0001;
const NIF_ICON: u32 = 0x0000_0002;
const NIF_TIP: u32 = 0x0000_0004;
#[cfg_attr(test, allow(dead_code))]
const NIF_INFO: u32 = 0x0000_0010;
#[cfg_attr(test, allow(dead_code))]
const NIM_ADD: u32 = 0;
#[cfg_attr(test, allow(dead_code))]
const NIM_MODIFY: u32 = 1;
#[cfg_attr(test, allow(dead_code))]
const NIM_DELETE: u32 = 2;
#[cfg_attr(test, allow(dead_code))]
const NIM_SETVERSION: u32 = 4;
/// `NOTIFYICON_VERSION_4`: the shell reports `NIN_SELECT`/`NIN_KEYSELECT` and
/// puts the icon id in the high word of `lParam` and the screen point in
/// `wParam`. Without this call keyboard activation never reaches the window.
#[cfg_attr(test, allow(dead_code))]
const NOTIFYICON_VERSION_4: u32 = 4;

/// A loaded `HICON`. Resource icons are shared by the system and must never be
/// destroyed; an icon loaded from `assets/voxhold.ico` belongs to this process
/// and is released when the cache is replaced or dropped.
#[cfg(windows)]
#[derive(Debug)]
struct IconHandle {
    handle: isize,
    /// Tests never destroy icons, so the flag is only read in the real build.
    #[cfg_attr(test, allow(dead_code))]
    owned: bool,
}

#[cfg(windows)]
impl Drop for IconHandle {
    fn drop(&mut self) {
        #[cfg(not(test))]
        if self.owned && self.handle != 0 {
            unsafe {
                DestroyIcon(self.handle as *mut core::ffi::c_void);
            }
        }
    }
}

/// The permanent Voxhold icon in the Windows notification area.
///
/// Registration is independent from balloon notifications: turning the
/// preference off only stops `notification` from being called, it never removes
/// the icon. The icon is registered once per window and survives as long as the
/// process does, except for an Explorer restart, which needs `restore`.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct TrayIcon {
    registered: bool,
    /// Loaded once and reused by every `register`/`restore`, so an Explorer
    /// restart does not leak a new `HICON` and does not touch the disk again.
    icon: Option<IconHandle>,
    /// Test seam: count and outcome of registration without Shell_NotifyIcon.
    #[cfg(test)]
    test_add_calls: u32,
    #[cfg(test)]
    test_add_result: Option<bool>,
    #[cfg(test)]
    test_delete_calls: u32,
    #[cfg(test)]
    test_version_calls: u32,
    #[cfg(test)]
    test_balloon_calls: u32,
    #[cfg(test)]
    test_icon_loads: u32,
}

#[cfg(windows)]
impl TrayIcon {
    /// Add the icon for `hwnd`. Idempotent: a second call while the icon is
    /// registered is a no-op. A zero handle is reported instead of silently
    /// pretending that the tray works.
    pub fn register(&mut self, hwnd: isize) -> Result<(), String> {
        if self.registered {
            return Ok(());
        }
        if hwnd == 0 {
            return Err("Не удалось добавить значок Voxhold в системный трей.".into());
        }
        let icon = self.icon();
        let mut data = tray_data(hwnd);
        fill_registration(&mut data, icon);
        if !self.add_icon(hwnd, &mut data) {
            return Err("Не удалось добавить значок Voxhold в системный трей.".into());
        }
        self.apply_version(hwnd);
        self.registered = true;
        Ok(())
    }

    /// Re-add the icon after Explorer restarted. Unlike `register`, this always
    /// talks to the shell because the previous registration was lost with the
    /// old taskbar window.
    pub fn restore(&mut self, hwnd: isize) -> Result<(), String> {
        if hwnd == 0 {
            self.registered = false;
            return Err("Не удалось восстановить значок Voxhold в системном трее.".into());
        }
        self.registered = false;
        let icon = self.icon();
        let mut data = tray_data(hwnd);
        fill_registration(&mut data, icon);
        if !self.add_icon(hwnd, &mut data) {
            return Err("Не удалось восстановить значок Voxhold в системном трее.".into());
        }
        self.apply_version(hwnd);
        self.registered = true;
        Ok(())
    }

    /// Remove the icon with `NIM_DELETE`. Safe to call twice and on a zero
    /// handle; used by the single quit path before `cx.quit()`.
    pub fn unregister(&mut self, hwnd: isize) {
        let was_registered = self.registered;
        self.registered = false;
        // A removed icon cannot be reused; an owned file icon is destroyed
        // here. The resource icon stays shared and needs no cleanup.
        let icon = self.icon.take();
        if !was_registered || hwnd == 0 {
            return;
        }
        #[cfg(test)]
        {
            self.test_delete_calls = self.test_delete_calls.saturating_add(1);
        }
        #[cfg(not(test))]
        {
            let mut data = tray_data(hwnd);
            unsafe {
                shell_notify_icon(NIM_DELETE, &mut data);
            }
        }
        drop(icon);
    }

    pub fn is_registered(&self) -> bool {
        self.registered
    }

    /// Show a balloon through the already registered icon (`NIF_INFO` +
    /// `NIM_MODIFY`). If the icon is missing, it is registered first; the
    /// notification never deletes it afterwards.
    pub fn notification(&mut self, hwnd: isize, title: &str, message: &str) -> Result<(), String> {
        if !self.registered {
            self.register(hwnd)?;
        }
        if hwnd == 0 {
            return Err("Не удалось показать системное уведомление.".into());
        }
        #[cfg(test)]
        {
            let _ = (title, message);
            self.test_balloon_calls = self.test_balloon_calls.saturating_add(1);
            return Ok(());
        }
        #[cfg(not(test))]
        {
            let mut data = tray_data(hwnd);
            data.flags = NIF_INFO;
            data.callback = TRAY_CALLBACK_MESSAGE;
            data.info_flags = 1 | 16 | 128; // information, silent, respect quiet time
            fill(&mut data.title, title);
            fill(&mut data.info, message);
            if data.info[0] == 0 {
                fill(&mut data.info, "Новое сообщение");
            }
            if unsafe { shell_notify_icon(NIM_MODIFY, &mut data) } == 0 {
                return Err("Не удалось показать системное уведомление.".into());
            }
            Ok(())
        }
    }

    /// Test seams for the close-hook tests outside this module.
    #[cfg(test)]
    pub(crate) fn set_test_add_result(&mut self, result: bool) {
        self.test_add_result = Some(result);
    }

    #[cfg(test)]
    pub(crate) fn test_delete_calls(&self) -> u32 {
        self.test_delete_calls
    }

    #[cfg(test)]
    pub(crate) fn test_version_calls(&self) -> u32 {
        self.test_version_calls
    }

    #[cfg(test)]
    pub(crate) fn test_balloon_calls(&self) -> u32 {
        self.test_balloon_calls
    }

    #[cfg(test)]
    pub(crate) fn test_icon_loads(&self) -> u32 {
        self.test_icon_loads
    }

    /// The icon is loaded once per process and reused after `NIM_DELETE` /
    /// Explorer restart. A failed load leaves a null handle; the shell then
    /// uses its own default instead of losing the whole registration.
    fn icon(&mut self) -> isize {
        if let Some(icon) = &self.icon {
            return icon.handle;
        }
        #[cfg(test)]
        {
            self.test_icon_loads = self.test_icon_loads.saturating_add(1);
            self.icon = Some(IconHandle {
                handle: 1,
                owned: false,
            });
            1
        }
        #[cfg(not(test))]
        {
            let icon = load_tray_icon();
            let handle = icon.handle;
            self.icon = Some(icon);
            handle
        }
    }

    /// Version 4 is what makes `NIN_SELECT`/`NIN_KEYSELECT` and the
    /// `wParam` screen point available to the window procedure. A failure
    /// keeps the legacy mouse messages working, so the registration stands.
    fn apply_version(&mut self, hwnd: isize) {
        #[cfg(test)]
        {
            let _ = hwnd;
            self.test_version_calls = self.test_version_calls.saturating_add(1);
        }
        #[cfg(not(test))]
        {
            let mut data = tray_data(hwnd);
            data.callback = TRAY_CALLBACK_MESSAGE;
            data.timeout_or_version = NOTIFYICON_VERSION_4;
            unsafe {
                shell_notify_icon(NIM_SETVERSION, &mut data);
            }
        }
    }

    /// Update the icon first, then add it. `NIM_MODIFY` succeeds when the icon
    /// survived (a repeated registration), while a real Explorer restart lost
    /// it and needs `NIM_ADD`.
    fn add_icon(&mut self, hwnd: isize, data: &mut NotifyIconData) -> bool {
        #[cfg(test)]
        {
            let _ = (hwnd, data);
            self.test_add_calls = self.test_add_calls.saturating_add(1);
            self.test_add_result.unwrap_or(true)
        }
        #[cfg(not(test))]
        {
            let _ = hwnd;
            unsafe {
                shell_notify_icon(NIM_MODIFY, data) != 0 || shell_notify_icon(NIM_ADD, data) != 0
            }
        }
    }
}

#[cfg(windows)]
fn fill_registration(data: &mut NotifyIconData, icon: isize) {
    data.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.callback = TRAY_CALLBACK_MESSAGE;
    data.icon = icon;
    fill(&mut data.tip, "Voxhold");
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
#[repr(C)]
struct NotifyIconData {
    size: u32,
    window: isize,
    id: u32,
    flags: u32,
    callback: u32,
    icon: isize,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    timeout_or_version: u32,
    title: [u16; 64],
    info_flags: u32,
    guid: Guid,
    balloon_icon: isize,
}

#[cfg(windows)]
fn tray_data(hwnd: isize) -> NotifyIconData {
    NotifyIconData {
        size: std::mem::size_of::<NotifyIconData>() as u32,
        window: hwnd,
        id: 1,
        flags: 0,
        callback: 0,
        icon: 0,
        tip: [0; 128],
        state: 0,
        state_mask: 0,
        info: [0; 256],
        timeout_or_version: 0,
        title: [0; 64],
        info_flags: 0,
        guid: Guid {
            data1: 0,
            data2: 0,
            data3: 0,
            data4: [0; 8],
        },
        balloon_icon: 0,
    }
}

#[cfg(windows)]
fn fill(dst: &mut [u16], text: &str) {
    let text = text.replace('\0', "");
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut length = units.len().min(dst.len().saturating_sub(1));
    // Never split a surrogate pair.
    if length > 0 && length < units.len() && (0xd800..=0xdbff).contains(&units[length - 1]) {
        length -= 1;
    }
    dst[..length].copy_from_slice(&units[..length]);
}

#[cfg(all(windows, not(test)))]
#[link(name = "user32")]
unsafe extern "system" {
    fn DestroyIcon(icon: *mut core::ffi::c_void) -> i32;
}

/// The brand icon for the tray: resource id 1 from the EXE when the icon was
/// embedded by the build, then `assets/voxhold.ico`, then the stock
/// application icon so the tray still works before the embedding PR lands.
///
/// The caller caches the returned handle. Resource icons (`LoadIconW`) are
/// shared and `owned == false`; a file loaded with `LoadImageW` belongs to this
/// process and is destroyed by `IconHandle`.
#[cfg(all(windows, not(test)))]
fn load_tray_icon() -> IconHandle {
    const IDI_APPLICATION: usize = 32512;
    const IMAGE_ICON: u32 = 1;
    const LR_LOADFROMFILE: u32 = 0x0000_0010;
    const LR_DEFAULTSIZE: u32 = 0x0000_0040;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut core::ffi::c_void;
        fn LoadIconW(instance: *mut core::ffi::c_void, name: *const u16) -> *mut core::ffi::c_void;
        fn LoadImageW(
            instance: *mut core::ffi::c_void,
            name: *const u16,
            kind: u32,
            width: i32,
            height: i32,
            flags: u32,
        ) -> *mut core::ffi::c_void;
    }

    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        // MAKEINTRESOURCE(1): the resource id is encoded in the pointer.
        let embedded = LoadIconW(module, 1_usize as *const u16);
        if !embedded.is_null() {
            return IconHandle {
                handle: embedded as isize,
                owned: false,
            };
        }
    }
    if let Some(path) = crate::window_chrome::icon_path() {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        unsafe {
            let icon = LoadImageW(
                std::ptr::null_mut(),
                wide.as_ptr(),
                IMAGE_ICON,
                0,
                0,
                LR_LOADFROMFILE | LR_DEFAULTSIZE,
            );
            if !icon.is_null() {
                return IconHandle {
                    handle: icon as isize,
                    owned: true,
                };
            }
        }
    }
    IconHandle {
        handle: unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION as *const u16) as isize },
        owned: false,
    }
}

#[cfg(windows)]
#[cfg_attr(test, allow(dead_code))]
unsafe fn shell_notify_icon(message: u32, data: &mut NotifyIconData) -> i32 {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn Shell_NotifyIconW(message: u32, data: *mut core::ffi::c_void) -> i32;
    }
    unsafe { Shell_NotifyIconW(message, (data as *mut NotifyIconData).cast()) }
}

#[cfg(not(windows))]
#[derive(Debug, Default)]
pub struct TrayIcon {
    registered: bool,
}

#[cfg(not(windows))]
impl TrayIcon {
    pub fn register(&mut self, _hwnd: isize) -> Result<(), String> {
        Err("Системный трей доступен в версии для Windows.".into())
    }

    pub fn restore(&mut self, _hwnd: isize) -> Result<(), String> {
        Err("Системный трей доступен в версии для Windows.".into())
    }

    pub fn unregister(&mut self, _hwnd: isize) {
        self.registered = false;
    }

    pub fn is_registered(&self) -> bool {
        self.registered
    }

    pub fn notification(
        &mut self,
        _hwnd: isize,
        _title: &str,
        _message: &str,
    ) -> Result<(), String> {
        Err("Системные уведомления доступны в версии для Windows.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: i64, author: &str) -> serde_json::Value {
        serde_json::json!({
            "channel_name": "qa-chat",
            "server_name": "QA",
            "message": {"id": id, "content": "hello", "author": {"username": author}}
        })
    }

    #[test]
    fn disabled_or_active_window_produces_nothing() {
        let mut center = NotificationCenter::default();
        assert!(center
            .offer("notification.message", &message(1, "neva"), "owner", false)
            .is_none());
        center.enabled = true;
        assert!(center
            .offer("notification.message", &message(1, "neva"), "owner", true)
            .is_none());
        assert!(center
            .offer("notification.message", &message(1, "neva"), "owner", false)
            .is_some());
    }

    #[test]
    fn a_failed_show_is_retried_and_a_successful_one_is_deduplicated() {
        let mut center = NotificationCenter::default();
        center.enabled = true;
        let (key, _, body) = center
            .offer("notification.message", &message(2, "neva"), "owner", false)
            .unwrap();
        assert_eq!(body, "neva: hello");
        // Native show failed: the key stays unseen.
        assert!(center
            .offer("notification.message", &message(2, "neva"), "owner", false)
            .is_some());
        center.shown(&key);
        assert!(center
            .offer("notification.message", &message(2, "neva"), "owner", false)
            .is_none());
        assert_eq!(center.shown, 1);
    }

    #[test]
    fn own_messages_are_ignored() {
        let mut center = NotificationCenter::default();
        center.enabled = true;
        assert!(center
            .offer("notification.message", &message(3, "owner"), "owner", false)
            .is_none());
    }

    #[test]
    fn enable_is_persisted_and_disable_clears_remembered_keys() {
        let dir = std::env::temp_dir().join(format!(
            "voxhold-notifications-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let path = path_from_layout(&dir.join("workspace-layout.json"));
        let mut center = NotificationCenter::with_path(Some(path.clone()));
        assert!(!center.enabled);
        assert!(center.set_enabled(true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "true");
        center.shown("channel:1");
        assert!(center.set_enabled(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "false");
        assert!(center
            .offer("notification.message", &message(4, "neva"), "owner", false)
            .is_none());
        assert!(center.set_enabled(true));
        assert!(center
            .offer("notification.message", &message(4, "neva"), "owner", false)
            .is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn tray_registration_is_idempotent_and_removal_is_safe() {
        let mut tray = TrayIcon::default();
        tray.test_add_result = Some(true);
        assert!(tray.register(1).is_ok());
        assert!(tray.register(1).is_ok());
        assert_eq!(tray.test_add_calls, 1);
        assert_eq!(tray.test_version_calls(), 1);
        assert_eq!(tray.test_icon_loads(), 1);
        assert!(tray.is_registered());
        tray.unregister(1);
        assert!(!tray.is_registered());
        assert_eq!(tray.test_delete_calls(), 1);
        // NIM_DELETE of a missing icon is not attempted again.
        tray.unregister(1);
        assert_eq!(tray.test_add_calls, 1);
        assert_eq!(tray.test_delete_calls(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn restore_reuses_the_loaded_icon_and_reapplies_version_four() {
        let mut tray = TrayIcon::default();
        tray.test_add_result = Some(true);
        tray.register(1).unwrap();
        tray.restore(1).unwrap();
        tray.restore(1).unwrap();
        assert_eq!(tray.test_add_calls, 3);
        assert_eq!(tray.test_icon_loads(), 1);
        assert_eq!(tray.test_version_calls(), 3);
    }

    #[cfg(windows)]
    #[test]
    fn a_balloon_uses_the_test_seam_without_the_shell() {
        let mut tray = TrayIcon::default();
        tray.register(1).unwrap();
        assert!(tray.notification(1, "title", "body").is_ok());
        assert_eq!(tray.test_balloon_calls, 1);
    }

    #[cfg(windows)]
    #[test]
    fn a_failed_restore_leaves_the_icon_unregistered() {
        let mut tray = TrayIcon::default();
        tray.test_add_result = Some(true);
        assert!(tray.register(1).is_ok());
        tray.test_add_result = Some(false);
        assert!(tray.restore(1).is_err());
        assert!(!tray.is_registered());
    }

    #[cfg(windows)]
    #[test]
    fn disabling_notifications_does_not_remove_the_icon() {
        let mut tray = TrayIcon::default();
        tray.test_add_result = Some(true);
        tray.register(1).unwrap();
        let mut center = NotificationCenter::default();
        assert!(center.set_enabled(true));
        assert!(center.set_enabled(false));
        assert!(!center.enabled);
        assert!(tray.is_registered());
    }

    #[test]
    fn whitespace_collapses_like_go() {
        let mut center = NotificationCenter::default();
        center.enabled = true;
        let data = serde_json::json!({
            "channel_name": "c", "server_name": "s",
            "message": {"id": 5, "content": "a\n\n  b", "author": {"username": "n"}}
        });
        let (_, _, body) = center
            .offer("notification.message", &data, "owner", false)
            .unwrap();
        assert_eq!(body, "n: a b");
    }
}
