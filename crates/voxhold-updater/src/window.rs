//! Branded Win32 progress window shown by the update helper.
//!
//! The helper owns the main installation surface: the client exits before the
//! installation is replaced. The window lives on its own thread with a normal
//! message loop, so file work never blocks painting.
//!
//! The window is fixed at 360 × 240 logical pixels and is centred in the work
//! area passed by the client (`--monitor <left,top,right,bottom>`) or in the
//! primary work area. DPI awareness is set before the first window exists, the
//! initial frame is scaled from the monitor DPI, and after creation every
//! metric is scaled by the window DPI (`GetDpiForWindow`) so the client area
//! stays 360 × 240 logical at 100%, 125% and 150%.
//!
//! The ring turns once per second at ~30 fps from `Instant`; the timer stops
//! while the window is minimised. When Windows reports client-area animations
//! disabled the ring is drawn static. Closing the window during the apply only
//! hides it: the replacement is never cancelled by the user.

#[cfg(windows)]
use std::sync::mpsc::{sync_channel, SyncSender};
#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::thread::JoinHandle;
#[cfg(windows)]
use std::time::{Duration, Instant};

use crate::stages::Stage;

pub use crate::monitor::MonitorRect;

/// Client area of the window in logical pixels.
const LOGICAL_WIDTH: i32 = 360;
const LOGICAL_HEIGHT: i32 = 240;
const RING_DIAMETER: i32 = 92;
const MARK_SIZE: i32 = 64;
const TIMER_ID: usize = 1;
/// ~30 fps.
const TIMER_MS: u32 = 33;
/// `WM_APP + 1` and `WM_APP + 2` (`WM_APP` is 0x8000).
const WM_APP_STAGE: u32 = 0x8001;
const WM_APP_CLOSE: u32 = 0x8002;
const CLASS_NAME: &str = "VoxholdUpdaterWindow";
const WINDOW_TITLE: &str = "Voxhold";

#[cfg(windows)]
struct StageState {
    stage: Stage,
    version: Option<String>,
}

/// Handle owned by the helper's main thread.
#[cfg(windows)]
pub struct WindowHandle {
    hwnd: isize,
    shared: Arc<Mutex<StageState>>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(windows)]
impl WindowHandle {
    /// Replace the stage line. Cheap and non-blocking: the message loop draws.
    pub fn set_stage(&self, stage: Stage) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.stage = stage;
        }
        // SAFETY: `hwnd` belongs to the window thread; posting is thread-safe.
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
            PostMessageW(self.hwnd as *mut core::ffi::c_void, WM_APP_STAGE, 0, 0);
        }
    }

    /// Ask the window thread to destroy the window. `Drop` also joins it.
    pub fn request_close(&self) {
        // SAFETY: as above; the window thread turns this into `DestroyWindow`.
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
            PostMessageW(self.hwnd as *mut core::ffi::c_void, WM_APP_CLOSE, 0, 0);
        }
    }
}

#[cfg(windows)]
impl Drop for WindowHandle {
    fn drop(&mut self) {
        self.request_close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Read `AppsUseLightTheme` before the window is created. If the value cannot
/// be read the dark palette is used, matching the client's fallback.
#[cfg(windows)]
fn apps_use_light_theme() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    const SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let subkey = wide(SUBKEY);
    let name = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let mut kind = 0u32;
    // SAFETY: both names are NUL-terminated and the output buffer is valid.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            &mut kind,
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && data != 0
}

#[cfg(windows)]
fn animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
    };

    let mut enabled: i32 = 1;
    // SAFETY: the API writes one BOOL to the provided pointer.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast(),
            0,
        )
    };
    ok == 0 || enabled != 0
}

/// DPI awareness must be selected before any window is created. A manifest may
/// already have set it; the access-denied failure is then harmless.
pub fn set_dpi_awareness() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Start the window thread and wait until the window is visible and able to
/// receive messages. Failure means the helper must not write `helper-ready.ok`.
pub fn spawn(
    monitor: Option<MonitorRect>,
    version: Option<String>,
) -> Result<WindowHandle, String> {
    #[cfg(windows)]
    {
        launch(monitor, version)
    }
    #[cfg(not(windows))]
    {
        let _ = (monitor, version);
        Err("the update window is only available on Windows".into())
    }
}

#[cfg(windows)]
fn launch(monitor: Option<MonitorRect>, version: Option<String>) -> Result<WindowHandle, String> {
    let shared = Arc::new(Mutex::new(StageState {
        stage: Stage::Verifying,
        version,
    }));
    let thread_shared = Arc::clone(&shared);
    let (ready_tx, ready_rx) = sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("voxhold-updater-window".into())
        .spawn(move || run_window(thread_shared, monitor, ready_tx))
        .map_err(|error| format!("cannot start the window thread: {error}"))?;
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(hwnd)) => Ok(WindowHandle {
            hwnd,
            shared,
            thread: Some(thread),
        }),
        Ok(Err(message)) => {
            let _ = thread.join();
            Err(message)
        }
        // A stuck thread cannot be stopped safely; the process exits soon
        // anyway because the helper refuses to continue without the window.
        Err(_) => Err("the update window did not appear".into()),
    }
}

#[cfg(not(windows))]
pub struct WindowHandle {
    _private: (),
}

#[cfg(not(windows))]
impl WindowHandle {
    pub fn set_stage(&self, _stage: Stage) {}

    pub fn request_close(&self) {}
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::cell::RefCell;
    use std::f64::consts::{FRAC_PI_2, TAU};

    use crate::stages::TITLE;
    use windows_sys::Win32::Foundation::{
        GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT,
        WPARAM,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        Arc, BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
        CreatePen, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint,
        FillRect, GetMonitorInfoW, GetStockObject, InvalidateRect, MonitorFromPoint,
        MonitorFromRect, RoundRect, SelectObject, SetArcDirection, SetBkMode, SetTextColor,
        UpdateWindow, AD_CLOCKWISE, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE,
        DT_VCENTER, FW_BOLD, HBRUSH, HDC, HFONT, HGDIOBJ, HMONITOR, MONITORINFO,
        MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, NULL_BRUSH, SRCCOPY, TRANSPARENT,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{
        AdjustWindowRectExForDpi, GetDpiForMonitor, GetDpiForSystem, GetDpiForWindow,
        MDT_EFFECTIVE_DPI,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, DrawIconEx,
        GetClientRect, GetMessageW, GetSystemMetrics, KillTimer, LoadCursorW, LoadImageW,
        PostQuitMessage, RegisterClassW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage,
        UnregisterClassW, CS_HREDRAW, CS_VREDRAW, DI_NORMAL, HICON, IDC_ARROW, IMAGE_ICON,
        LR_DEFAULTCOLOR, MSG, SIZE_MINIMIZED, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE,
        SWP_NOZORDER, SW_HIDE, SW_SHOWNORMAL, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
        WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX, WS_SYSMENU,
    };

    // Only the messages this module needs; the rest goes to `DefWindowProcW`.
    thread_local! {
        static STATE: RefCell<Option<WindowData>> = const { RefCell::new(None) };
    }

    fn with_state<R>(f: impl FnOnce(&mut WindowData) -> R) -> Option<R> {
        STATE.with(|cell| cell.borrow_mut().as_mut().map(f))
    }

    #[derive(Clone, Copy)]
    struct Colors {
        bg: COLORREF,
        fg: COLORREF,
        lime: COLORREF,
        muted: COLORREF,
        ring: COLORREF,
    }

    impl Colors {
        fn new(light: bool) -> Self {
            let (bg, fg, lime, muted, line) = if light {
                (0xf2f1ec, 0x20211d, 0xb5cf16, 0x62635b, 0xdbdbd1)
            } else {
                (0x10100f, 0xf5f3ec, 0xe7ff5b, 0xa6a69e, 0x292927)
            };
            Self {
                bg: colorref(bg),
                fg: colorref(fg),
                lime: colorref(lime),
                muted: colorref(muted),
                // The same `line` token the client uses for its ring.
                ring: colorref(line),
            }
        }
    }

    fn colorref(hex: u32) -> COLORREF {
        let r = (hex >> 16) & 0xff;
        let g = (hex >> 8) & 0xff;
        let b = hex & 0xff;
        r | (g << 8) | (b << 16)
    }

    /// Scale a logical pixel metric to the window DPI.
    fn scaled(value: i32, dpi: u32) -> i32 {
        ((value as f32) * dpi as f32 / 96.0).round() as i32
    }

    /// Physical client area of the fixed logical window size at `dpi`.
    ///
    /// The window is created with this area, so 100%, 125% and 150% show the
    /// same 360 x 240 logical client instead of a physical 360 x 240 that would
    /// clip the ring and the stage line.
    fn client_size(dpi: u32) -> (i32, i32) {
        (
            scaled(LOGICAL_WIDTH, dpi).max(1),
            scaled(LOGICAL_HEIGHT, dpi).max(1),
        )
    }

    /// Outer window size that yields `client_width` x `client_height`.
    unsafe fn outer_size(
        style: u32,
        client_width: i32,
        client_height: i32,
        dpi: u32,
    ) -> (i32, i32) {
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: client_width,
            bottom: client_height,
        };
        if AdjustWindowRectExForDpi(&mut frame, style, 0, 0, dpi) == 0 {
            return (client_width.max(1), client_height.max(1));
        }
        (
            (frame.right - frame.left).max(1),
            (frame.bottom - frame.top).max(1),
        )
    }

    struct WindowData {
        shared: Arc<Mutex<StageState>>,
        dpi: u32,
        started: Instant,
        animate: bool,
        timer_on: bool,
        hidden: bool,
        colors: Colors,
        bg_brush: HBRUSH,
        title_font: HFONT,
        stage_font: HFONT,
        mark_font: HFONT,
        icon: HICON,
    }

    impl WindowData {
        unsafe fn new(
            shared: Arc<Mutex<StageState>>,
            dpi: u32,
            colors: Colors,
            animate: bool,
            icon: HICON,
        ) -> Self {
            Self {
                shared,
                dpi,
                started: Instant::now(),
                animate,
                timer_on: false,
                hidden: false,
                colors,
                bg_brush: CreateSolidBrush(colors.bg),
                // 15 px bold matches the client's installing overlay.
                title_font: create_font(15, FW_BOLD, dpi),
                stage_font: create_font(13, 400, dpi),
                mark_font: create_font(30, FW_BOLD, dpi),
                icon,
            }
        }

        unsafe fn set_dpi(&mut self, dpi: u32) {
            if self.dpi == dpi || dpi == 0 {
                return;
            }
            replace_font(&mut self.title_font, 15, FW_BOLD, dpi);
            replace_font(&mut self.stage_font, 13, 400, dpi);
            replace_font(&mut self.mark_font, 30, FW_BOLD, dpi);
            self.dpi = dpi;
        }
    }

    impl Drop for WindowData {
        fn drop(&mut self) {
            // SAFETY: every handle belongs to this window thread and is freed once.
            unsafe {
                for font in [self.title_font, self.stage_font, self.mark_font] {
                    if !font.is_null() {
                        DeleteObject(font as HGDIOBJ);
                    }
                }
                if !self.bg_brush.is_null() {
                    DeleteObject(self.bg_brush as HGDIOBJ);
                }
                if !self.icon.is_null() {
                    DestroyIcon(self.icon);
                }
            }
        }
    }

    unsafe fn create_font(size_px: i32, weight: u32, dpi: u32) -> HFONT {
        let mut face: Vec<u16> = "Segoe UI".encode_utf16().collect();
        face.push(0);
        let height = -scaled(size_px, dpi);
        CreateFontW(
            height,
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            windows_sys::Win32::Graphics::Gdi::DEFAULT_CHARSET as u32,
            0,
            0,
            windows_sys::Win32::Graphics::Gdi::CLEARTYPE_QUALITY as u32,
            (windows_sys::Win32::Graphics::Gdi::DEFAULT_PITCH
                | windows_sys::Win32::Graphics::Gdi::FF_DONTCARE) as u32,
            face.as_ptr(),
        )
    }

    unsafe fn replace_font(font: &mut HFONT, size_px: i32, weight: u32, dpi: u32) {
        let replacement = create_font(size_px, weight, dpi);
        if !replacement.is_null() {
            if !font.is_null() {
                DeleteObject(*font as HGDIOBJ);
            }
            *font = replacement;
        }
    }

    /// Work area of the monitor that will host the window, and its effective DPI.
    ///
    /// `--monitor` is trusted only while the rectangle intersects a live monitor;
    /// a stale rectangle (monitor unplugged, layout changed) falls back to the
    /// primary work area so the window never opens off-screen. A failed monitor
    /// query falls back to the primary screen size and the system DPI.
    unsafe fn target_area(monitor: Option<MonitorRect>) -> (RECT, u32) {
        let requested = monitor.map(|rect| RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        });
        // MONITOR_DEFAULTTONULL returns null when no monitor intersects.
        let handle = requested
            .map(|rect| MonitorFromRect(&rect, MONITOR_DEFAULTTONULL))
            .filter(|handle| !handle.is_null())
            .unwrap_or_else(|| MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY));
        let dpi = monitor_dpi(handle);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !handle.is_null() && GetMonitorInfoW(handle, &mut info) != 0 {
            (info.rcWork, dpi)
        } else {
            (
                RECT {
                    left: 0,
                    top: 0,
                    right: GetSystemMetrics(SM_CXSCREEN).max(1),
                    bottom: GetSystemMetrics(SM_CYSCREEN).max(1),
                },
                dpi,
            )
        }
    }

    /// Effective DPI of `monitor`, falling back to the system DPI.
    unsafe fn monitor_dpi(monitor: HMONITOR) -> u32 {
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        if !monitor.is_null()
            && GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) == 0
            && dpi_x >= 96
        {
            return dpi_x;
        }
        match GetDpiForSystem() {
            0 => 96,
            dpi => dpi,
        }
    }

    pub(super) fn run_window(
        shared: Arc<Mutex<StageState>>,
        monitor: Option<MonitorRect>,
        ready: SyncSender<Result<isize, String>>,
    ) {
        // SAFETY: all handles created here are released on every path below.
        unsafe {
            let class_name = wide(CLASS_NAME);
            let title = wide(WINDOW_TITLE);
            let module = GetModuleHandleW(std::ptr::null());
            let colors = Colors::new(apps_use_light_theme());
            let animate = animations_enabled();
            let brush = CreateSolidBrush(colors.bg);
            let class_icon = LoadImageW(
                module,
                1usize as *const u16,
                IMAGE_ICON,
                64,
                64,
                LR_DEFAULTCOLOR,
            );
            let cursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: module,
                hIcon: class_icon,
                hCursor: cursor,
                hbrBackground: brush,
                lpszClassName: class_name.as_ptr(),
                ..Default::default()
            };
            let mut atom = RegisterClassW(&class);
            if atom == 0 && GetLastError() == ERROR_CLASS_ALREADY_EXISTS {
                // A class left by an earlier attempt still holds the previous
                // brush and icon; drop it before registering the live handles.
                UnregisterClassW(class_name.as_ptr(), module);
                atom = RegisterClassW(&class);
            }
            if atom == 0 {
                let code = GetLastError();
                cleanup_brush_and_icon(brush, class_icon);
                let _ = ready.send(Err(format!("RegisterClassW failed with code {code}")));
                return;
            }

            let (area, monitor_dpi) = target_area(monitor);
            let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let (client_width, client_height) = client_size(monitor_dpi);
            let (width, height) = outer_size(style, client_width, client_height, monitor_dpi);
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                style,
                area.left + (area.right - area.left - width) / 2,
                area.top + (area.bottom - area.top - height) / 2,
                width,
                height,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                module,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                let code = GetLastError();
                cleanup_brush_and_icon(brush, class_icon);
                // The class would outlive the brush and icon it points at.
                UnregisterClassW(class_name.as_ptr(), module);
                let _ = ready.send(Err(format!("CreateWindowExW failed with code {code}")));
                return;
            }

            let dpi = match GetDpiForWindow(hwnd) {
                0 => monitor_dpi,
                value => value,
            };
            if dpi != monitor_dpi {
                // Windows placed the window on a monitor with another DPI than
                // the one used for the initial frame. Recompute the outer frame
                // before the window is shown so the client area still matches
                // the logical size and the size never jumps after that.
                let (client_width, client_height) = client_size(dpi);
                let (width, height) = outer_size(style, client_width, client_height, dpi);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    area.left + (area.right - area.left - width) / 2,
                    area.top + (area.bottom - area.top - height) / 2,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            let data = WindowData::new(Arc::clone(&shared), dpi, colors, animate, class_icon);
            STATE.with(|cell| *cell.borrow_mut() = Some(data));

            ShowWindow(hwnd, SW_SHOWNORMAL);
            UpdateWindow(hwnd);
            start_timer(hwnd);
            // The marker is written by the caller only after this message, so
            // the window is visible; the loop below is entered immediately
            // after the send, so queued stage messages are never lost.
            let _ = ready.send(Ok(hwnd as isize));

            let mut message = MSG::default();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            UnregisterClassW(class_name.as_ptr(), module);
            STATE.with(|cell| {
                let _ = cell.borrow_mut().take();
            });
        }
    }

    unsafe fn cleanup_brush_and_icon(brush: HBRUSH, icon: HICON) {
        if !brush.is_null() {
            DeleteObject(brush as HGDIOBJ);
        }
        if !icon.is_null() {
            DestroyIcon(icon);
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn start_timer(hwnd: HWND) {
        with_state(|data| {
            if data.animate && !data.hidden && !data.timer_on {
                data.timer_on = SetTimer(hwnd, TIMER_ID, TIMER_MS, None) != 0;
            }
        });
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                paint(hwnd);
                0
            }
            WM_TIMER => {
                if wparam == TIMER_ID {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                0
            }
            WM_SIZE => {
                if wparam as u32 == SIZE_MINIMIZED {
                    KillTimer(hwnd, TIMER_ID);
                    with_state(|data| data.timer_on = false);
                } else {
                    start_timer(hwnd);
                }
                0
            }
            // Closing never cancels an apply that already started: the helper
            // keeps working and closes the window itself after the handshake.
            WM_CLOSE => {
                KillTimer(hwnd, TIMER_ID);
                with_state(|data| {
                    data.timer_on = false;
                    data.hidden = true;
                });
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_APP_STAGE => {
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            WM_APP_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                KillTimer(hwnd, TIMER_ID);
                PostQuitMessage(0);
                0
            }
            WM_DPICHANGED => {
                let dpi = (wparam & 0xffff) as u32;
                let suggested = &*(lparam as *const RECT);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                with_state(|data| data.set_dpi(dpi));
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn paint(hwnd: HWND) {
        let mut paint = windows_sys::Win32::Graphics::Gdi::PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut paint);
        if hdc.is_null() {
            return;
        }
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client);
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        let memory = CreateCompatibleDC(hdc);
        if !memory.is_null() {
            let bitmap = if width > 0 && height > 0 {
                CreateCompatibleBitmap(hdc, width, height)
            } else {
                std::ptr::null_mut()
            };
            if !bitmap.is_null() {
                let previous = SelectObject(memory, bitmap as HGDIOBJ);
                draw(memory, width, height);
                BitBlt(hdc, 0, 0, width, height, memory, 0, 0, SRCCOPY);
                SelectObject(memory, previous);
                DeleteObject(bitmap as HGDIOBJ);
            } else {
                draw(hdc, width, height);
            }
            // Every path that created the memory DC releases it here, so a
            // failed bitmap allocation cannot leak a DC per paint.
            DeleteDC(memory);
        } else {
            draw(hdc, width, height);
        }
        EndPaint(hwnd, &paint);
    }

    unsafe fn draw(hdc: HDC, width: i32, height: i32) {
        with_state(|data| {
            let background = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            FillRect(hdc, &background, data.bg_brush);
            if width <= 0 || height <= 0 {
                return;
            }
            let px = |value: i32| scaled(value, data.dpi);
            let center_x = width / 2;
            let center_y = (height as f32 * 0.42).round() as i32;
            let diameter = px(RING_DIAMETER);
            let radius = (diameter / 2).max(2);
            let hollow = GetStockObject(NULL_BRUSH);

            let ring_pen = CreatePen(
                windows_sys::Win32::Graphics::Gdi::PS_SOLID,
                px(2).max(1),
                data.colors.ring,
            );
            if !ring_pen.is_null() {
                let previous_brush = SelectObject(hdc, hollow);
                let previous_pen = SelectObject(hdc, ring_pen as HGDIOBJ);
                Ellipse(
                    hdc,
                    center_x - radius,
                    center_y - radius,
                    center_x + radius,
                    center_y + radius,
                );
                SelectObject(hdc, previous_pen);
                SelectObject(hdc, previous_brush);
                DeleteObject(ring_pen as HGDIOBJ);
            }

            let arc_pen = CreatePen(
                windows_sys::Win32::Graphics::Gdi::PS_SOLID,
                px(3).max(1),
                data.colors.lime,
            );
            if !arc_pen.is_null() {
                let angle = if data.animate {
                    data.started.elapsed().as_secs_f64() * TAU
                } else {
                    -FRAC_PI_2
                };
                let (start_x, start_y) = point_on_ring(center_x, center_y, radius, angle);
                let (end_x, end_y) = point_on_ring(center_x, center_y, radius, angle + FRAC_PI_2);
                let previous_brush = SelectObject(hdc, hollow);
                let previous_pen = SelectObject(hdc, arc_pen as HGDIOBJ);
                // GDI draws counterclockwise by default; the ring angle grows
                // with the screen's clockwise direction, so ask for clockwise
                // to keep the highlighted arc a quarter of the ring.
                SetArcDirection(hdc, AD_CLOCKWISE);
                Arc(
                    hdc,
                    center_x - radius,
                    center_y - radius,
                    center_x + radius,
                    center_y + radius,
                    start_x,
                    start_y,
                    end_x,
                    end_y,
                );
                SelectObject(hdc, previous_pen);
                SelectObject(hdc, previous_brush);
                DeleteObject(arc_pen as HGDIOBJ);
            }

            let mark = px(MARK_SIZE);
            let mark_left = center_x - mark / 2;
            let mark_top = center_y - mark / 2;
            if !data.icon.is_null() {
                DrawIconEx(
                    hdc,
                    mark_left,
                    mark_top,
                    data.icon,
                    mark,
                    mark,
                    0,
                    std::ptr::null_mut(),
                    DI_NORMAL,
                );
            } else {
                let brush = CreateSolidBrush(data.colors.lime);
                let pen = CreatePen(
                    windows_sys::Win32::Graphics::Gdi::PS_SOLID,
                    1,
                    data.colors.lime,
                );
                if !brush.is_null() {
                    let previous_brush = SelectObject(hdc, brush as HGDIOBJ);
                    let previous_pen = SelectObject(hdc, pen);
                    RoundRect(
                        hdc,
                        mark_left,
                        mark_top,
                        mark_left + mark,
                        mark_top + mark,
                        px(14),
                        px(14),
                    );
                    SelectObject(hdc, previous_pen);
                    SelectObject(hdc, previous_brush);
                }
                if !pen.is_null() {
                    DeleteObject(pen as HGDIOBJ);
                }
                if !brush.is_null() {
                    DeleteObject(brush as HGDIOBJ);
                }
                draw_line(
                    hdc,
                    "V",
                    mark_left,
                    mark_top,
                    mark,
                    mark,
                    data.mark_font,
                    data.colors.bg,
                );
            }

            draw_line(
                hdc,
                TITLE,
                0,
                (height as f32 * 0.665).round() as i32,
                width,
                px(26),
                data.title_font,
                data.colors.fg,
            );
            let (stage, version) = match data.shared.lock() {
                Ok(shared) => (shared.stage, shared.version.clone()),
                Err(_) => (Stage::Verifying, None),
            };
            let line = stage.line(version.as_deref());
            draw_line(
                hdc,
                &line,
                0,
                (height as f32 * 0.815).round() as i32,
                width,
                px(20),
                data.stage_font,
                data.colors.muted,
            );
        });
    }

    fn point_on_ring(center_x: i32, center_y: i32, radius: i32, angle: f64) -> (i32, i32) {
        let x = center_x + (radius as f64 * angle.cos()).round() as i32;
        let y = center_y + (radius as f64 * angle.sin()).round() as i32;
        (x, y)
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_line(
        hdc: HDC,
        text: &str,
        left: i32,
        top: i32,
        width: i32,
        height: i32,
        font: HFONT,
        color: COLORREF,
    ) {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut rect = RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, color);
        let previous = SelectObject(hdc, font as HGDIOBJ);
        DrawTextW(
            hdc,
            wide.as_ptr(),
            -1,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(hdc, previous);
    }

    #[cfg(test)]
    mod metric_tests {
        use super::*;

        #[test]
        fn logical_metrics_scale_with_the_window_dpi() {
            assert_eq!(scaled(360, 96), 360);
            assert_eq!(scaled(240, 96), 240);
            assert_eq!(scaled(92, 96), 92);
            assert_eq!(scaled(360, 120), 450);
            assert_eq!(scaled(240, 120), 300);
            assert_eq!(scaled(92, 120), 115);
            assert_eq!(scaled(360, 144), 540);
            assert_eq!(scaled(240, 144), 360);
            assert_eq!(scaled(92, 144), 138);
            assert_eq!(scaled(1, 144), 2);
        }

        #[test]
        fn the_client_area_stays_360_by_240_logical_pixels() {
            assert_eq!(client_size(96), (360, 240));
            assert_eq!(client_size(120), (450, 300));
            assert_eq!(client_size(144), (540, 360));
        }
    }
}

#[cfg(windows)]
use platform::run_window;
