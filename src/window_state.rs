//! Window-level state that the media preview request needs.
//!
//! GPUI exposes focus and activation, but not "this window is minimized". On
//! Windows the real answer comes from the raw `HWND` of the GPUI window
//! (`IsIconic`), so a minimized client stops converting its local preview even
//! while the capture keeps publishing. Other platforms answer `false`: the
//! demand then follows the card visibility alone.
//!
//! The helper never panics and never blocks: a window without a raw handle is
//! simply not treated as minimized.

use gpui::Window;

/// Whether `window` is minimized.
pub(crate) fn is_minimized(window: &Window) -> bool {
    #[cfg(windows)]
    {
        #[cfg(test)]
        {
            // A GPUI test window is not backed by a platform window and panics
            // by design when asked for a raw handle. The unit tests therefore
            // cannot exercise `IsIconic`; they cover `preview_request`, which
            // takes the flag directly.
            let _ = window;
            false
        }
        #[cfg(not(test))]
        {
            use raw_window_handle::RawWindowHandle;
            // `Window` has an inherent `window_handle` returning GPUI's own
            // handle, so the raw-window-handle trait is called explicitly.
            let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
                return false;
            };
            let RawWindowHandle::Win32(win32) = handle.as_raw() else {
                return false;
            };
            is_iconic(win32.hwnd.get())
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        false
    }
}

/// `IsIconic` for one raw window handle. A null handle is not a window and
/// answers `false`.
#[cfg(windows)]
fn is_iconic(hwnd: isize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::IsIconic;
    if hwnd == 0 {
        return false;
    }
    // SAFETY: `IsIconic` only reads the state of the handle it is given. The
    // handle comes from the live GPUI window, and a stale one makes the call
    // answer `false` rather than touch memory in this process.
    unsafe { IsIconic(hwnd as *mut core::ffi::c_void) != 0 }
}

#[cfg(all(test, windows))]
mod tests {
    use super::is_iconic;

    #[test]
    fn a_null_handle_is_not_minimized() {
        assert!(!is_iconic(0));
    }
}
