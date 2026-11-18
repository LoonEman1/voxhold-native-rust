//! System power transitions for the voice session.
//!
//! Windows delivers `WM_POWERBROADCAST` to top-level windows. GPUI has no hook
//! for it, so the main window is subclassed with `SetWindowSubclass`; the
//! callback only records the transition, and a foreground timer in the view
//! applies it. Nothing in the window procedure touches the audio engine, so a
//! slow media worker cannot block the message loop.
//!
//! A suspend releases the software playout sink and closes the capture hold; a
//! resume asks the media worker to reopen the sink and re-check the devices.

use super::*;
use std::sync::{Arc, Mutex};

/// A queued power transition. `true` is a resume.
pub(crate) type PowerQueue = Arc<Mutex<Vec<bool>>>;

pub(crate) fn new_queue() -> PowerQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// The subclass is removed when the window is destroyed. The boxed queue is
/// released there too.
#[cfg(windows)]
pub(crate) fn install(window: &gpui::Window, queue: PowerQueue) {
    use raw_window_handle::RawWindowHandle;
    if cfg!(test) {
        return;
    }
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let hwnd = win32.hwnd.get() as *mut core::ffi::c_void;
    if hwnd.is_null() {
        return;
    }
    let data = Box::into_raw(Box::new(queue));
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(power_proc), SUBCLASS_ID, data as usize);
    }
}

#[cfg(not(windows))]
pub(crate) fn install(_window: &gpui::Window, _queue: PowerQueue) {}

#[cfg(windows)]
const SUBCLASS_ID: usize = 0x5648_5057; // "VHPW"
#[cfg(windows)]
const WM_POWERBROADCAST: u32 = 0x0218;
#[cfg(windows)]
const PBT_APMSUSPEND: usize = 0x0004;
#[cfg(windows)]
const PBT_APMRESUMESUSPEND: usize = 0x0007;
#[cfg(windows)]
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
#[cfg(windows)]
const WM_NCDESTROY: u32 = 0x0082;

#[cfg(windows)]
#[link(name = "comctl32")]
unsafe extern "system" {
    fn SetWindowSubclass(
        hwnd: *mut core::ffi::c_void,
        proc_: Option<
            unsafe extern "system" fn(
                *mut core::ffi::c_void,
                u32,
                usize,
                isize,
                usize,
                usize,
            ) -> isize,
        >,
        id: usize,
        data: usize,
    ) -> i32;
    fn RemoveWindowSubclass(
        hwnd: *mut core::ffi::c_void,
        proc_: Option<
            unsafe extern "system" fn(
                *mut core::ffi::c_void,
                u32,
                usize,
                isize,
                usize,
                usize,
            ) -> isize,
        >,
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
unsafe extern "system" fn power_proc(
    hwnd: *mut core::ffi::c_void,
    msg: u32,
    wparam: usize,
    lparam: isize,
    _id: usize,
    data: usize,
) -> isize {
    if msg == WM_POWERBROADCAST {
        let transition = match wparam {
            PBT_APMSUSPEND => Some(false),
            PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => Some(true),
            _ => None,
        };
        if let Some(resumed) = transition {
            let queue = &*(data as *const PowerQueue);
            if let Ok(mut queue) = queue.lock() {
                // Last-value is enough: a suspend followed by a resume before
                // the UI polls must not replay an old suspend.
                queue.clear();
                queue.push(resumed);
            }
            return 0;
        }
    }
    if msg == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(power_proc), SUBCLASS_ID);
            drop(Box::from_raw(data as *mut PowerQueue));
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// One drained transition. `None` means nothing happened.
pub(crate) fn take(queue: &PowerQueue) -> Option<bool> {
    queue.lock().ok()?.pop()
}

impl Voxhold {
    /// Subclass the main window and poll the queue on the foreground executor.
    /// The media work happens in `power_event`, never in the window procedure.
    pub(crate) fn install_power(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let queue = new_queue();
        install(window, queue.clone());
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| loop {
            executor.timer(std::time::Duration::from_millis(250)).await;
            let Some(view) = this.upgrade() else {
                break;
            };
            let transition = take(&queue);
            if view
                .update_in(cx, |view, window, cx| {
                    if let Some(resumed) = transition {
                        view.power_event(resumed, window, cx);
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    /// Apply one system power transition through the normal voice path: the
    /// panel releases a held key, closes the microphone on suspend and asks the
    /// media worker to reopen the sink on resume.
    pub(super) fn power_event(
        &mut self,
        resumed: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = &self.voice {
            panel.update(cx, |panel, cx| panel.power(resumed, cx));
        }
        cx.notify();
    }
}
