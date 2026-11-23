//! System power transitions for the voice session.
//!
//! Windows delivers `WM_POWERBROADCAST` to top-level windows. GPUI has no hook
//! for it, so the main window is subclassed with `SetWindowSubclass`; the
//! callback only records the transition in a process-wide last-value slot. A
//! foreground poller, which exists only while a voice call is active, drains it
//! and applies it through the normal voice path.
//!
//! The subclass has no heap allocation attached: repeated installs are
//! idempotent and `WM_NCDESTROY` only removes the subclass. A suspend releases
//! the software playout sink and closes the capture hold; a resume asks the
//! media worker to reopen the sink and re-check the devices.

use super::*;
use std::sync::{Mutex, OnceLock};

/// Last queued transition: `true` is a resume. One slot is enough; a suspend
/// followed by a resume before the poll runs must not replay the old suspend.
fn queue() -> &'static Mutex<Vec<bool>> {
    static QUEUE: OnceLock<Mutex<Vec<bool>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn clear() {
    if let Ok(mut queue) = queue().lock() {
        queue.clear();
    }
}

pub(crate) fn take() -> Option<bool> {
    queue().lock().ok()?.pop()
}

fn push(resumed: bool) {
    if let Ok(mut queue) = queue().lock() {
        queue.clear();
        queue.push(resumed);
    }
}

/// Install the subclass once. It is idempotent: a second call would fail inside
/// `SetWindowSubclass` for the same id and is ignored.
#[cfg(windows)]
pub(crate) fn install(window: &gpui::Window) {
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
    // Touch the queue so the callback never sees an uninitialised OnceLock.
    let _ = queue();
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(power_proc), SUBCLASS_ID, 0);
    }
}

#[cfg(not(windows))]
pub(crate) fn install(_window: &gpui::Window) {}

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
unsafe extern "system" fn power_proc(
    hwnd: *mut core::ffi::c_void,
    msg: u32,
    wparam: usize,
    lparam: isize,
    _id: usize,
    _data: usize,
) -> isize {
    if msg == WM_POWERBROADCAST {
        match wparam {
            PBT_APMSUSPEND => push(false),
            PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => push(true),
            _ => {}
        }
        if matches!(
            wparam,
            PBT_APMSUSPEND | PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC
        ) {
            return 0;
        }
    }
    if msg == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(power_proc), SUBCLASS_ID);
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

impl Voxhold {
    /// Install the window hook. The poller is started only when a call becomes
    /// active (`update_power_poll`), so an idle window has no timer.
    pub(crate) fn install_power(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = cx;
        install(window);
    }

    /// Start or stop the foreground poller after a call starts or ends. This is
    /// called from the panel's call handler, so every join/leave path reaches
    /// it, including the room's own button and a server-ended call.
    pub(super) fn update_power_poll(&mut self, cx: &mut Context<Self>) {
        let active = self.voice_call_active_for_power(cx);
        if !active {
            // Dropping a Task cancels it; no timer remains while idle.
            self.power_task = None;
            return;
        }
        if self.power_task.is_some() {
            return;
        }
        // Transitions from before the call are stale.
        clear();
        let executor = cx.background_executor().clone();
        self.power_task = Some(cx.spawn(async move |this, cx| loop {
            executor.timer(std::time::Duration::from_millis(250)).await;
            let Some(view) = this.upgrade() else {
                break;
            };
            let active = view
                .update(cx, |view, cx| view.voice_call_active_for_power(cx))
                .unwrap_or(false);
            if !active {
                break;
            }
            if let Some(resumed) = take() {
                view.update(cx, |view, cx| view.power_event(resumed, cx))
                    .ok();
            }
        }));
    }

    pub(super) fn voice_call_active_for_power(&self, cx: &Context<Self>) -> bool {
        self.voice
            .as_ref()
            .is_some_and(|panel| panel.read(cx).call_active())
    }

    /// Apply one system power transition through the normal voice path: the
    /// panel releases a held key, closes the microphone on suspend and asks the
    /// media worker to reopen the sink on resume.
    pub(super) fn power_event(&mut self, resumed: bool, cx: &mut Context<Self>) {
        if let Some(panel) = &self.voice {
            panel.update(cx, |panel, cx| panel.power(resumed, cx));
        }
        cx.notify();
    }
}
