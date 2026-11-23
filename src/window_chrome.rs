//! Native window chrome that GPUI does not expose: the DWM dark titlebar and
//! the `assets/voxhold.ico` window/taskbar icon.
//!
//! Go sets both from `titlebar_windows.go` (`setWindowTheme`, `setWindowIcon`).
//! Failures are ignored: a light titlebar or the default icon must never stop
//! the client from starting.

#[cfg(windows)]
pub fn apply(window: &gpui::Window, light: bool) {
    if let Some(hwnd) = window_hwnd(window) {
        unsafe {
            set_dark_titlebar(hwnd, !light);
            set_icon(hwnd);
        }
    }
}

#[cfg(windows)]
pub fn set_theme(window: &gpui::Window, light: bool) {
    if let Some(hwnd) = window_hwnd(window) {
        unsafe { set_dark_titlebar(hwnd, !light) }
    }
}

#[cfg(windows)]
fn window_hwnd(window: &gpui::Window) -> Option<*mut core::ffi::c_void> {
    use raw_window_handle::RawWindowHandle;

    // GPUI's test window is not backed by a platform window and its
    // `window_handle` is explicitly unimplemented.
    if cfg!(test) {
        return None;
    }
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return None;
    };
    Some(win32.hwnd.get() as *mut core::ffi::c_void)
}

#[cfg(not(windows))]
pub fn apply(_window: &gpui::Window, _light: bool) {}

#[cfg(not(windows))]
pub fn set_theme(_window: &gpui::Window, _light: bool) {}

#[cfg(windows)]
unsafe fn set_dark_titlebar(hwnd: *mut core::ffi::c_void, dark: bool) {
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1: u32 = 19;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut core::ffi::c_void,
            attribute: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    let value: i32 = i32::from(dark);
    let size = std::mem::size_of::<i32>() as u32;
    let value = (&value as *const i32).cast();
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, value, size);
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1, value, size);
}

#[cfg(windows)]
unsafe fn set_icon(hwnd: *mut core::ffi::c_void) {
    use std::os::windows::ffi::OsStrExt;

    const IMAGE_ICON: u32 = 1;
    const LR_LOADFROMFILE: u32 = 0x0000_0010;
    const LR_DEFAULTSIZE: u32 = 0x0000_0040;
    const WM_SETICON: u32 = 0x0080;
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn LoadImageW(
            instance: *mut core::ffi::c_void,
            name: *const u16,
            kind: u32,
            width: i32,
            height: i32,
            flags: u32,
        ) -> *mut core::ffi::c_void;
        fn SendMessageW(
            hwnd: *mut core::ffi::c_void,
            message: u32,
            wparam: usize,
            lparam: isize,
        ) -> isize;
    }

    let Some(path) = icon_path() else { return };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let icon = LoadImageW(
        std::ptr::null_mut(),
        wide.as_ptr(),
        IMAGE_ICON,
        0,
        0,
        LR_LOADFROMFILE | LR_DEFAULTSIZE,
    );
    if icon.is_null() {
        return;
    }
    // Owned by the window after WM_SETICON; do not destroy it here.
    SendMessageW(hwnd, WM_SETICON, ICON_SMALL, icon as isize);
    SendMessageW(hwnd, WM_SETICON, ICON_BIG, icon as isize);
}

/// Search the same locations as Go `findAssets`: next to the executable, one
/// level up in a build output tree, then the current working directory.
#[cfg(windows)]
pub(crate) fn icon_path() -> Option<std::path::PathBuf> {
    crate::assets::Assets::discover().path("voxhold.ico")
}
