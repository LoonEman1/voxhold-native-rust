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
use crate::notifications::TrayIcon;
use crate::notifications::TRAY_CALLBACK_MESSAGE;
#[cfg(windows)]
use std::sync::atomic::AtomicIsize;
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
    /// The subclassed HWND was destroyed; the icon bound to it is gone.
    WindowGone(isize),
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

/// A visible, active window must not be re-shown or focused again; a hidden or
/// minimized one is brought forward.
pub(crate) fn needs_show(hidden_to_tray: bool, window_active: bool) -> bool {
    hidden_to_tray || !window_active
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

/// Short close message. The window stays open, so the banner also offers the
/// normal exit path instead of trapping the user in an app whose tray is dead.
pub(super) const TRAY_CLOSE_ERROR: &str =
    "Не удалось убрать окно в трей: значок Voxhold недоступен.";

/// The GPUI test window is not backed by a platform window, so asking for its
/// raw handle would panic. Every Win32 call in this module and in the tray
/// notification adapter is replaced by a seam under `cfg(test)`, so a fake
/// handle is enough and no real shell/window API runs in tests.
#[cfg(test)]
pub(super) const TEST_WINDOW_HANDLE: isize = 1;

fn window_hwnd(window: &Window) -> isize {
    #[cfg(test)]
    {
        let _ = window;
        TEST_WINDOW_HANDLE
    }
    #[cfg(not(test))]
    {
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
            // The icon must not appear until the window procedure is live:
            // otherwise the cross would hide the window with no way back.
            let proc_installed = install_window_proc(hwnd);
            self.notification_tray.set_proc_available(proc_installed);
            register_tray_after_proc(&mut self.notification_tray, hwnd, proc_installed);
            if self.notification_tray.is_registered() {
                self.clear_tray_error();
            }
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
                } else {
                    self.clear_tray_error();
                    cx.notify();
                }
            }
            TrayAction::RestoreFailed => self.tray_restore_failed(window, cx),
            TrayAction::WindowGone(hwnd) => {
                // The shell entry died with its HWND; a stale `registered`
                // flag must not let the close hook hide into nothing.
                self.notification_tray.invalidate(hwnd);
            }
        }
    }

    /// The window close button: hide to the tray when the icon is registered.
    /// A failed registration keeps the window visible and explains why; GPUI
    /// must not close it, because the error frame would never be painted.
    pub(super) fn should_close_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // A first install can race the platform window. Retry on the close
        // gesture instead of leaving the tray unavailable until a restart.
        if !self.quitting && !self.notification_tray.is_available_for(window_hwnd(window)) {
            self.install_tray(window, cx);
        }
        let available = self.notification_tray.is_available_for(window_hwnd(window));
        match close_decision(available, self.quitting) {
            CloseDecision::Hide => {
                self.hide_to_tray(window, cx);
                false
            }
            CloseDecision::Close => true,
            CloseDecision::CloseWithError => {
                self.report_tray_error(TRAY_CLOSE_ERROR, cx);
                false
            }
        }
    }

    /// The banner shows a normal exit button whenever the close button is
    /// blocked, not only for one exact error text: another message must not
    /// hide the only way out of a window whose close hook cannot hide it.
    pub(super) fn tray_exit_offered(&self) -> bool {
        self.tray_close_blocked
    }

    /// The tray banner text, independent of the general `error` field.
    pub(super) fn tray_error_text(&self) -> &str {
        if self.tray_notice.is_empty() {
            TRAY_CLOSE_ERROR
        } else {
            &self.tray_notice
        }
    }

    /// `WM_ACTIVATE` from `SetForegroundWindow` on a hidden window does not
    /// make it visible. Notifications and read marks must keep using the
    /// hidden state, otherwise the tray menu silences the app until it is
    /// opened again.
    pub(super) fn window_activation_changed(&mut self, window_is_active: bool) {
        self.window_active = !self.hidden_to_tray && window_is_active;
    }

    pub(super) fn clear_tray_error(&mut self) {
        if !self.tray_notice.is_empty() && self.error == self.tray_notice {
            self.error.clear();
        }
        self.tray_notice.clear();
        self.tray_close_blocked = false;
    }

    fn hide_to_tray(&mut self, window: &Window, cx: &mut Context<Self>) {
        let hwnd = window_hwnd(window);
        hide_window(hwnd);
        // The window is no longer active, so notifications must not be
        // suppressed while it is hidden. `SW_HIDE` does not send `WM_ACTIVATE`,
        // so this flag, not `Window::is_window_active`, is the source of truth.
        self.window_active = false;
        self.hidden_to_tray = true;
        // The icon is live (this path is only reached for a registered tray),
        // so no tray error can still be blocking the close hook.
        self.clear_tray_error();
        // A hidden window gets no activation event: release a held PTT now so
        // the microphone cannot stay open behind the tray.
        #[cfg(feature = "voxhold-media")]
        if let Some(voice) = self.voice.clone() {
            voice.update(cx, |panel, _| panel.release_push_to_talk());
        }
        cx.notify();
    }

    /// Bring back the same HWND. `SW_SHOW` keeps the previous position and
    /// maximized state; a minimized window is restored first. Calling this for
    /// an already visible and active window is a no-op, so a duplicate tray
    /// click cannot pull focus from another application.
    pub(super) fn show_main_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !needs_show(self.hidden_to_tray, self.window_active) {
            return;
        }
        let hwnd = window_hwnd(window);
        show_window(hwnd);
        if !cfg!(test) {
            window.activate_window();
        }
        self.hidden_to_tray = false;
        // Windows can show the window while denying the foreground request.
        // Keep read receipts and notification suppression tied to actual focus.
        self.window_active = is_foreground_window(hwnd);
        // The menu or the window is visible again, so the tray works; a stale
        // registration error must not keep the redundant exit banner around.
        self.clear_tray_error();
        cx.notify();
    }

    fn tray_restore_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Explorer restart failure while the window is visible must not steal
        // focus; only a genuinely hidden window comes back.
        if !self.quitting && self.hidden_to_tray {
            self.show_main_window(window, cx);
        }
        self.report_tray_error(
            "Не удалось восстановить значок Voxhold в системном трее.",
            cx,
        );
    }

    fn report_tray_error(&mut self, message: &str, cx: &mut Context<Self>) {
        self.error = message.to_string();
        // Kept separately from the general error and from `auth.status`: a
        // later message must not hide the banner or the exit button, and a
        // login error must stay the login error.
        self.tray_notice = message.to_string();
        self.tray_close_blocked = true;
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
        // Retirement is permanent: a late balloon or `TaskbarCreated` queued
        // behind the close must not recreate an icon after `NIM_DELETE`.
        self.notification_tray.shut_down(hwnd);
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
static PROC_HWND: AtomicIsize = AtomicIsize::new(0);

/// Whether this exact HWND still needs `SetWindowSubclass`. A single global
/// flag would skip installation for a second window and would be cleared by
/// the destruction of an unrelated HWND.
#[cfg(windows)]
fn proc_slot_needs_install(slot: Option<isize>, hwnd: isize) -> bool {
    slot != Some(hwnd)
}

#[cfg(windows)]
fn proc_slot_after_destroy(slot: Option<isize>, hwnd: isize) -> Option<isize> {
    if slot == Some(hwnd) {
        None
    } else {
        slot
    }
}

#[cfg(windows)]
fn stored_proc_hwnd() -> Option<isize> {
    let hwnd = PROC_HWND.load(Ordering::SeqCst);
    (hwnd != 0).then_some(hwnd)
}

#[cfg(windows)]
fn store_proc_hwnd(hwnd: isize) {
    PROC_HWND.store(hwnd, Ordering::SeqCst);
}

#[cfg(windows)]
fn clear_proc_hwnd(hwnd: isize) {
    let current = stored_proc_hwnd();
    if proc_slot_after_destroy(current, hwnd) == current {
        // Destruction of another HWND must not forget our subclass.
        return;
    }
    let _ = PROC_HWND.compare_exchange(hwnd, 0, Ordering::SeqCst, Ordering::SeqCst);
}

/// Returns whether the window procedure is (now) installed for this HWND. The
/// icon is only registered on success; a dead subclass would make the cross
/// hide the window with no way to bring it back.
#[cfg(windows)]
fn install_window_proc(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    #[cfg(test)]
    {
        // Tests never call the real `SetWindowSubclass`.
        return true;
    }
    #[cfg(not(test))]
    {
        if !proc_slot_needs_install(stored_proc_hwnd(), hwnd) {
            return true;
        }
        unsafe {
            if SetWindowSubclass(hwnd.null(), Some(tray_proc), SUBCLASS_ID, 0) == 0 {
                return false;
            }
        }
        store_proc_hwnd(hwnd);
        true
    }
}

#[cfg(not(windows))]
fn install_window_proc(_hwnd: isize) -> bool {
    false
}

/// Register the permanent icon only when the window procedure is live. On a
/// failure any stale icon is removed as well, so `is_registered` stays false
/// and the close hook reports the error instead of hiding.
#[cfg(windows)]
fn register_tray_after_proc(tray: &mut TrayIcon, hwnd: isize, proc_installed: bool) {
    if !proc_installed || hwnd == 0 {
        tray.unregister(hwnd);
        return;
    }
    let _ = tray.register(hwnd);
}

#[cfg(not(windows))]
fn register_tray_after_proc(_tray: &mut crate::notifications::TrayIcon, _hwnd: isize, _ok: bool) {}

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
        // Version 4 keeps the notification code in the low word of `lParam`
        // and carries the screen point in `wParam`; older shell versions send
        // `WM_RBUTTONUP` with no point, so `GetCursorPos` remains the fallback.
        let code = (lparam as usize) & 0xffff;
        if code == WM_CONTEXTMENU {
            // A keyboard invocation packs (-1, -1): neither half is a screen
            // point, so fall back to the cursor or the icon position.
            let point = menu_point(wparam, unsafe { cursor_position() });
            unsafe { show_menu(hwnd, Some(point)) };
        } else if code == WM_RBUTTONUP {
            unsafe { show_menu(hwnd, None) };
        } else if let Some(action) = callback_action(lparam) {
            post(action);
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
        clear_proc_hwnd(hwnd as isize);
        // The icon outlives this message only as a stale flag. Invalidate it
        // on the UI task; a closed channel simply drops the notification.
        post(TrayAction::WindowGone(hwnd as isize));
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Screen point packed into `wParam` by shell notification version 4:
/// `LOWORD` is x, `HIWORD` is y, both signed 16-bit values. A keyboard
/// invocation sends `0xFFFF_FFFF` (packed (-1, -1)), which is not a point.
#[cfg(windows)]
fn point_from_wparam(wparam: usize) -> Option<(i32, i32)> {
    if (wparam & 0xffff) == 0xffff || wparam as i32 == -1 {
        return None;
    }
    let x = (wparam & 0xffff) as u16 as i16 as i32;
    let y = ((wparam >> 16) & 0xffff) as u16 as i16 as i32;
    Some((x, y))
}

/// The context-menu point: the shell point when it is usable, otherwise the
/// current cursor position (the icon for a keyboard invocation). A zero
/// `wParam` decodes as `(0, 0)`, which is not an anchor and would put the menu
/// in the top-left corner of the screen.
#[cfg(windows)]
fn menu_point(wparam: usize, cursor: (i32, i32)) -> (i32, i32) {
    if wparam == 0 {
        return cursor;
    }
    point_from_wparam(wparam).unwrap_or(cursor)
}

#[cfg(windows)]
unsafe fn cursor_position() -> (i32, i32) {
    let mut point = Point { x: 0, y: 0 };
    let _ = unsafe { GetCursorPos(&mut point) };
    (point.x, point.y)
}

#[cfg(windows)]
unsafe fn show_menu(hwnd: *mut core::ffi::c_void, requested: Option<(i32, i32)>) {
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
        if let Some((x, y)) = requested {
            point.x = x;
            point.y = y;
        } else {
            let _ = GetCursorPos(&mut point);
        }
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

#[cfg(all(windows, not(test)))]
fn is_foreground_window(hwnd: isize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    hwnd != 0 && unsafe { GetForegroundWindow() as isize == hwnd }
}

#[cfg(any(not(windows), test))]
fn is_foreground_window(_hwnd: isize) -> bool {
    false
}

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

    #[cfg(windows)]
    #[test]
    fn a_window_proc_is_tracked_per_hwnd() {
        assert!(proc_slot_needs_install(None, 1));
        assert!(!proc_slot_needs_install(Some(1), 1));
        assert!(proc_slot_needs_install(Some(1), 2));
        assert_eq!(proc_slot_after_destroy(Some(1), 2), Some(1));
        assert_eq!(proc_slot_after_destroy(Some(1), 1), None);
        assert_eq!(proc_slot_after_destroy(None, 1), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_failed_window_proc_does_not_register_the_icon() {
        let mut tray = TrayIcon::default();
        tray.set_test_add_result(true);
        register_tray_after_proc(&mut tray, 1, false);
        assert!(!tray.is_registered());
        register_tray_after_proc(&mut tray, 1, true);
        assert!(tray.is_registered());
    }

    #[test]
    fn a_visible_active_window_is_not_shown_again() {
        assert!(!needs_show(false, true));
        assert!(needs_show(false, false));
        assert!(needs_show(true, false));
        assert!(needs_show(true, true));
    }

    #[cfg(windows)]
    #[test]
    fn version_four_packs_the_screen_point_into_wparam() {
        assert_eq!(point_from_wparam((300 << 16) | 200), Some((200, 300)));
        // Negative screen coordinates fit the signed 16-bit halves.
        assert_eq!(
            point_from_wparam(((-40_i32 as u16 as usize) << 16) | (-5_i32 as u16 as usize)),
            Some((-5, -40))
        );
        assert_eq!(point_from_wparam(0), Some((0, 0)));
    }

    #[cfg(windows)]
    #[test]
    fn a_keyboard_context_menu_falls_back_to_the_cursor() {
        // WM_CONTEXTMENU from the keyboard packs (-1, -1), not a screen point.
        assert_eq!(point_from_wparam(0xffff_ffff), None);
        assert_eq!(point_from_wparam(0x0000_ffff), None);
        assert_eq!(menu_point(0xffff_ffff, (640, 480)), (640, 480));
        // A zero wParam decodes as (0, 0); that is a missing anchor, not the
        // top-left corner, so the cursor position wins.
        assert_eq!(menu_point(0, (640, 480)), (640, 480));
        assert_eq!(menu_point((300 << 16) | 200, (640, 480)), (200, 300));
        assert_eq!(point_from_wparam(0), Some((0, 0)));
    }
}
