//! Closing the main window to the Windows notification area.
//!
//! GPUI has no tray API, so the window is subclassed with `SetWindowSubclass`
//! (the same pattern as `power.rs`). The subclass only decodes shell messages
//! and forwards `TrayAction` values through one long-lived channel; GPUI work
//! always runs on the UI task. No timer or polling is involved.
//!
//! The permanent icon lives in `notifications::TrayIcon`; this module owns the
//! Win32 window procedure, the context menu and the hide/show decisions.

use super::*;
#[cfg(test)]
use crate::notifications::TrayIcon;
use crate::notifications::TRAY_CALLBACK_MESSAGE;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// What the tray wants the UI task to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrayAction {
    Open,
    Exit,
    /// Explorer restarted and broadcast `TaskbarCreated`.
    Restore,
    /// Re-registration failed; the hidden window must come back.
    RestoreFailed,
}

/// Result of the close handler before it touches the platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseDecision {
    Hide,
    Close,
    CloseWithError,
}

/// A quitting process must really close: `cx.quit()` and the installer both
/// go through the same window-close callback.
pub(crate) fn close_decision(tray_available: bool, quitting: bool) -> CloseDecision {
    if quitting {
        CloseDecision::Close
    } else if tray_available {
        CloseDecision::Hide
    } else {
        CloseDecision::CloseWithError
    }
}

const NIN_SELECT: usize = 0x0400;
const NIN_KEYSELECT: usize = 0x0401;
const WM_LBUTTONUP: usize = 0x0202;
const WM_LBUTTONDBLCLK: usize = 0x0203;
const MENU_OPEN: usize = 1;
const MENU_EXIT: usize = 2;

/// Left click, double click and keyboard activation all open the same window.
/// The low word of `lParam` carries the shell notification code.
pub(crate) fn callback_action(lparam: isize) -> Option<TrayAction> {
    match (lparam as usize) & 0xffff {
        WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT | NIN_KEYSELECT => Some(TrayAction::Open),
        _ => None,
    }
}

pub(crate) fn menu_action(command: usize) -> Option<TrayAction> {
    match command {
        MENU_OPEN => Some(TrayAction::Open),
        MENU_EXIT => Some(TrayAction::Exit),
        _ => None,
    }
}

static CHANNEL: OnceLock<UnboundedSender<TrayAction>> = OnceLock::new();
static RECEIVER: OnceLock<Mutex<Option<UnboundedReceiver<TrayAction>>>> = OnceLock::new();

fn channel() -> &'static UnboundedSender<TrayAction> {
    CHANNEL.get_or_init(|| {
        let (sender, receiver) = unbounded_channel();
        let slot = RECEIVER.get_or_init(|| Mutex::new(None));
        if let Ok(mut slot) = slot.lock() {
            *slot = Some(receiver);
        }
        sender
    })
}

fn take_receiver() -> Option<UnboundedReceiver<TrayAction>> {
    RECEIVER.get()?.lock().ok()?.take()
}

fn post(action: TrayAction) {
    if let Some(sender) = CHANNEL.get() {
        let _ = sender.send(action);
    }
}

/// The GPUI test window is not backed by a platform window, so asking for its
/// raw handle would panic. Production paths always go through the real HWND.
fn window_hwnd(window: &Window) -> isize {
    if cfg!(test) {
        0
    } else {
        super::personal_ops::hwnd(window)
    }
}

impl Voxhold {
    /// Register the permanent icon, the window procedure and the one GPUI task
    /// that drains tray actions. A failure leaves the window fully usable; the
    /// close handler then reports the error instead of hiding it.
    pub(crate) fn install_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(test) {
            return;
        }
        #[cfg(windows)]
        {
            let hwnd = window_hwnd(window);
            if hwnd == 0 {
                return;
            }
            let _ = self.notification_tray.register(hwnd);
            install_window_proc(hwnd);
            let _ = channel();
            if self.tray_task.is_none() {
                if let Some(mut receiver) = take_receiver() {
                    self.tray_task = Some(cx.spawn_in(window, async move |this, window| {
                        while let Some(action) = receiver.recv().await {
                            let _ = this.update_in(window, |view, window, cx| {
                                view.tray_action(action, window, cx)
                            });
                        }
                    }));
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (window, cx);
        }
    }

    /// Route one action to the normal UI methods. Called only by the tray task.
    pub(super) fn tray_action(
        &mut self,
        action: TrayAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            TrayAction::Open => self.show_main_window(window, cx),
            TrayAction::Exit => self.quit_from_tray(window, cx),
            TrayAction::Restore => {
                let hwnd = window_hwnd(window);
                if self.notification_tray.restore(hwnd).is_err() {
                    // Keep one failure path for a failed Explorer restart and
                    // any future caller that reports it directly.
                    self.tray_action(TrayAction::RestoreFailed, window, cx);
                }
            }
            TrayAction::RestoreFailed => self.tray_restore_failed(window, cx),
        }
    }

    /// The window close button: hide to the tray when the icon is registered,
    /// otherwise let the normal close happen and report why it could not hide.
    pub(super) fn should_close_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match close_decision(self.notification_tray.is_registered(), self.quitting) {
            CloseDecision::Hide => {
                self.hide_to_tray(window, cx);
                false
            }
            CloseDecision::Close => true,
            CloseDecision::CloseWithError => {
                self.report_tray_error(
                    "Не удалось убрать окно в трей: значок Voxhold недоступен.",
                    cx,
                );
                true
            }
        }
    }

    fn hide_to_tray(&mut self, window: &Window, cx: &mut Context<Self>) {
        let hwnd = window_hwnd(window);
        hide_window(hwnd);
        // The window is no longer active, so notifications must not be
        // suppressed while it is hidden.
        self.window_active = false;
        cx.notify();
    }

    /// Bring back the same HWND. `SW_SHOW` keeps the previous position and
    /// maximized state; a minimized window is restored first.
    pub(super) fn show_main_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hwnd = window_hwnd(window);
        show_window(hwnd);
        if !cfg!(test) {
            window.activate_window();
        }
        self.window_active = true;
        cx.notify();
    }

    fn tray_restore_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.quitting {
            self.show_main_window(window, cx);
        }
        self.report_tray_error(
            "Не удалось восстановить значок Voxhold в системном трее.",
            cx,
        );
    }

    fn report_tray_error(&mut self, message: &str, cx: &mut Context<Self>) {
        self.error = message.to_string();
        self.auth.status = message.to_string();
        cx.notify();
    }

    /// "Выйти из Voxhold": stop voice/media/preview, persist settings, remove
    /// the icon and quit through the same close path as an update install.
    pub(super) fn quit_from_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.prepare_for_install(window, cx);
        self.finish_quit(window, cx);
    }

    /// Mark the process as quitting, remove the tray icon and ask GPUI to exit.
    /// The `quitting` flag makes `on_window_should_close` return `true`, so the
    /// tray handler cannot intercept the real close.
    pub(super) fn finish_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quitting = true;
        let hwnd = window_hwnd(window);
        self.notification_tray.unregister(hwnd);
        cx.quit();
    }
}

#[cfg(windows)]
const SUBCLASS_ID: usize = 0x5654_5259; // "VTRY"
#[cfg(windows)]
const WM_RBUTTONUP: usize = 0x0205;
#[cfg(windows)]
const WM_CONTEXTMENU: usize = 0x007B;
#[cfg(windows)]
const WM_NULL: u32 = 0x0000;
#[cfg(windows)]
const WM_NCDESTROY: u32 = 0x0082;

#[cfg(windows)]
trait WindowHwndExt {
    fn null(&self) -> *mut core::ffi::c_void;
}

#[cfg(windows)]
impl WindowHwndExt for isize {
    fn null(&self) -> *mut core::ffi::c_void {
        *self as *mut core::ffi::c_void
    }
}

#[cfg(windows)]
type SubclassProc =
    unsafe extern "system" fn(*mut core::ffi::c_void, u32, usize, isize, usize, usize) -> isize;

#[cfg(windows)]
#[link(name = "comctl32")]
unsafe extern "system" {
    fn SetWindowSubclass(
        hwnd: *mut core::ffi::c_void,
        proc_: Option<SubclassProc>,
        id: usize,
        data: usize,
    ) -> i32;
    fn RemoveWindowSubclass(
        hwnd: *mut core::ffi::c_void,
        proc_: Option<SubclassProc>,
        id: usize,
    ) -> i32;
    fn DefSubclassProc(
        hwnd: *mut core::ffi::c_void,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize;
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn ShowWindow(hwnd: *mut core::ffi::c_void, command: i32) -> i32;
    fn IsIconic(hwnd: *mut core::ffi::c_void) -> i32;
    fn BringWindowToTop(hwnd: *mut core::ffi::c_void) -> i32;
    fn SetForegroundWindow(hwnd: *mut core::ffi::c_void) -> i32;
    fn CreatePopupMenu() -> *mut core::ffi::c_void;
    fn DestroyMenu(menu: *mut core::ffi::c_void) -> i32;
    fn AppendMenuW(menu: *mut core::ffi::c_void, flags: u32, id: usize, item: *const u16) -> i32;
    fn TrackPopupMenu(
        menu: *mut core::ffi::c_void,
        flags: u32,
        x: i32,
        y: i32,
        reserved: i32,
        hwnd: *mut core::ffi::c_void,
        rect: *const core::ffi::c_void,
    ) -> i32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn PostMessageW(hwnd: *mut core::ffi::c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
}

#[cfg(windows)]
#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[cfg(windows)]
static PROC_INSTALLED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn install_window_proc(hwnd: isize) {
    if cfg!(test) || hwnd == 0 || PROC_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        if SetWindowSubclass(hwnd.null(), Some(tray_proc), SUBCLASS_ID, 0) == 0 {
            PROC_INSTALLED.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(not(windows))]
fn install_window_proc(_hwnd: isize) {}

#[cfg(windows)]
fn taskbar_created_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| {
        let name: Vec<u16> = "TaskbarCreated".encode_utf16().chain([0]).collect();
        unsafe { RegisterWindowMessageW(name.as_ptr()) }
    })
}

#[cfg(windows)]
static MENU_ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
struct MenuGuard;

#[cfg(windows)]
impl Drop for MenuGuard {
    fn drop(&mut self) {
        MENU_ACTIVE.store(false, Ordering::SeqCst);
    }
}

#[cfg(windows)]
unsafe extern "system" fn tray_proc(
    hwnd: *mut core::ffi::c_void,
    msg: u32,
    wparam: usize,
    lparam: isize,
    _id: usize,
    _data: usize,
) -> isize {
    if msg == TRAY_CALLBACK_MESSAGE {
        match (lparam as usize) & 0xffff {
            WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { show_menu(hwnd) },
            _ => {
                if let Some(action) = callback_action(lparam) {
                    post(action);
                }
            }
        }
        return 0;
    }
    let taskbar = taskbar_created_message();
    if taskbar != 0 && msg == taskbar {
        // Explorer restarted: the old icon is gone with it. Ask the UI task
        // to re-register; a failure there restores the hidden window.
        post(TrayAction::Restore);
        return 0;
    }
    if msg == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(tray_proc), SUBCLASS_ID);
        }
        PROC_INSTALLED.store(false, Ordering::SeqCst);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[cfg(windows)]
unsafe fn show_menu(hwnd: *mut core::ffi::c_void) {
    if MENU_ACTIVE.swap(true, Ordering::SeqCst) {
        // TrackPopupMenu dispatches messages; never open a nested menu from
        // inside the previous one.
        return;
    }
    let _guard = MenuGuard;
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    let open: Vec<u16> = "Открыть Voxhold".encode_utf16().chain([0]).collect();
    let exit: Vec<u16> = "Выйти из Voxhold".encode_utf16().chain([0]).collect();
    unsafe {
        let _ = AppendMenuW(menu, 0, MENU_OPEN, open.as_ptr()); // MF_STRING
        let _ = AppendMenuW(menu, 0x800, 0, std::ptr::null()); // MF_SEPARATOR
        let _ = AppendMenuW(menu, 0, MENU_EXIT, exit.as_ptr());
        let mut point = Point { x: 0, y: 0 };
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            0x0100 | 0x0002, // TPM_RETURNCMD | TPM_RIGHTBUTTON
            point.x,
            point.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        // Required by the shell documentation so the menu closes on an
        // outside click instead of staying on screen.
        let _ = PostMessageW(hwnd, WM_NULL, 0, 0);
        let _ = DestroyMenu(menu);
        if let Some(action) = menu_action(command.max(0) as usize) {
            post(action);
        }
    }
}

#[cfg(windows)]
fn hide_window(hwnd: isize) {
    if cfg!(test) || hwnd == 0 {
        return;
    }
    unsafe {
        let _ = ShowWindow(hwnd.null(), 0); // SW_HIDE
    }
}

#[cfg(not(windows))]
fn hide_window(_hwnd: isize) {}

#[cfg(windows)]
fn show_window(hwnd: isize) {
    if cfg!(test) || hwnd == 0 {
        return;
    }
    unsafe {
        let handle = hwnd.null();
        if IsIconic(handle) != 0 {
            let _ = ShowWindow(handle, 9); // SW_RESTORE
        } else {
            let _ = ShowWindow(handle, 5); // SW_SHOW keeps maximized state
        }
        let _ = BringWindowToTop(handle);
        let _ = SetForegroundWindow(handle);
    }
}

#[cfg(not(windows))]
fn show_window(_hwnd: isize) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_hides_only_when_the_tray_is_available() {
        assert_eq!(close_decision(true, false), CloseDecision::Hide);
        assert_eq!(close_decision(false, false), CloseDecision::CloseWithError);
    }

    #[test]
    fn quitting_passes_the_real_close_through() {
        assert_eq!(close_decision(true, true), CloseDecision::Close);
        assert_eq!(close_decision(false, true), CloseDecision::Close);
    }

    #[test]
    fn left_clicks_double_clicks_and_keyboard_open_the_window() {
        assert_eq!(
            callback_action(WM_LBUTTONUP as isize),
            Some(TrayAction::Open)
        );
        assert_eq!(
            callback_action(WM_LBUTTONDBLCLK as isize),
            Some(TrayAction::Open)
        );
        assert_eq!(callback_action(NIN_SELECT as isize), Some(TrayAction::Open));
        assert_eq!(
            callback_action(NIN_KEYSELECT as isize),
            Some(TrayAction::Open)
        );
        assert_eq!(callback_action(WM_RBUTTONUP as isize), None);
    }

    #[test]
    fn menu_commands_map_to_open_and_exit() {
        assert_eq!(menu_action(MENU_OPEN), Some(TrayAction::Open));
        assert_eq!(menu_action(MENU_EXIT), Some(TrayAction::Exit));
        assert_eq!(menu_action(0), None);
        assert_eq!(menu_action(99), None);
    }

    #[test]
    fn notification_codes_use_only_the_low_word() {
        // Some shell versions set the high word; it must not hide the click.
        assert_eq!(
            callback_action((0x1234_0000 | WM_LBUTTONUP as isize) as isize),
            Some(TrayAction::Open)
        );
    }

    #[test]
    fn a_tray_icon_type_is_available_for_the_close_decision() {
        let tray = TrayIcon::default();
        assert!(!tray.is_registered());
    }
}
