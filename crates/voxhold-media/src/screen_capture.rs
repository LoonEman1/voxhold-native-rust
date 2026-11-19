//! Windows desktop capture for one screen-share attempt.
//!
//! The capture source is the desktop capturer that ships with the linked
//! WebRTC build (`libwebrtc::desktop_capturer`). A spike on this host proved
//! that it lists both monitors and windows and delivers real BGRA frames, so
//! no second capture stack is used.
//!
//! Layout of the module:
//! - [`list_sources`] enumerates monitors or top-level windows. Windows of this
//!   process are filtered out so the picker cannot offer a mirror of itself.
//! - [`ScreenCapture`] owns one capture thread per publishing attempt. The
//!   capturer is created on that thread; creation failures are reported back
//!   before `start` returns.
//! - the frame callback converts BGRA to I420 with a box filter (the raw frame
//!   is only valid during the callback) and stores only the newest image. A
//!   slower consumer drops frames instead of growing a queue.
//! - [`PreviewStore`] keeps one small BGRA picture for the local on-air card.
//!   It is bounded by [`MAX_PREVIEW_BYTES`] and never grows while idle.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use libwebrtc::video_frame::{I420Buffer, VideoBuffer};

/// The largest side of the local preview. The on-air card is a room tile.
pub const MAX_PREVIEW_WIDTH: u32 = 640;
/// Upper bound of one stored preview picture. A bigger picture is rejected.
pub const MAX_PREVIEW_BYTES: usize = 4 * 1024 * 1024;
/// The same limits the viewer accepts; a captured frame is not bigger.
const MAX_CAPTURE_SIDE: u32 = 4096;

/// What kind of source is captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Monitor,
    Window,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Monitor => "Мониторы",
            Self::Window => "Окна",
        }
    }
}

/// One entry of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenSource {
    /// Source id of the desktop capturer. For a window this is its `HWND`.
    pub id: u64,
    pub kind: SourceKind,
    pub title: String,
    /// `-1` when the pinned capturer does not report it (verified on this
    /// host: both monitor sources reported `-1`).
    pub display_id: i64,
    /// Best known size; `0` when the platform did not report one.
    pub width: u32,
    pub height: u32,
}

impl ScreenSource {
    /// Text of the picker row: title plus a size when one is known.
    pub fn describe(&self) -> String {
        if self.width > 0 && self.height > 0 {
            format!("{} · {}×{}", self.title, self.width, self.height)
        } else {
            self.title.clone()
        }
    }
}

/// Enumerate monitors or windows for the picker.
///
/// The list is read on a worker thread, not on the UI thread. An empty list is
/// a valid answer (a locked-down session can hide every window); a missing
/// capturer is an explicit error, never a silent empty success.
pub fn list_sources(kind: SourceKind) -> Result<Vec<ScreenSource>, &'static str> {
    #[cfg(windows)]
    {
        windows::list_sources(kind)
    }
    #[cfg(not(windows))]
    {
        let _ = kind;
        Err("Захват экрана поддерживается только в Windows.")
    }
}

/// Whether this build can capture the desktop at all. It does not prove that
/// the session has a monitor, so the picker still handles an empty list.
pub fn capability() -> Result<(), &'static str> {
    #[cfg(windows)]
    {
        if windows::capturer_available() {
            Ok(())
        } else {
            Err("Системный захват экрана недоступен.")
        }
    }
    #[cfg(not(windows))]
    {
        Err("Захват экрана поддерживается только в Windows.")
    }
}

/// Cached capability for the render path. The check creates a capturer, so it
/// must not run on every frame.
pub fn capability_cached() -> Result<(), &'static str> {
    static CACHE: std::sync::OnceLock<Result<(), &'static str>> = std::sync::OnceLock::new();
    *CACHE.get_or_init(capability)
}

/// A failure reported by the desktop capturer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureFailure {
    /// The source is temporarily unavailable (for example a locked desktop).
    Temporary,
    /// The source is gone (window closed, monitor unplugged).
    Permanent,
    /// The thread ended without an error (stop was requested).
    Ended,
}

impl CaptureFailure {
    pub fn message(self) -> &'static str {
        match self {
            Self::Temporary => "Захват экрана временно недоступен.",
            Self::Permanent => "Источник захвата закрыт или отключён.",
            Self::Ended => "Захват экрана остановлен.",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Temporary => "temporary",
            Self::Permanent => "permanent",
            Self::Ended => "ended",
        }
    }
}

/// One converted capture result.
pub struct CapturedImage {
    pub width: u32,
    pub height: u32,
    pub i420: I420Buffer,
    /// Small BGRA copy for the local preview.
    pub preview: Option<PreviewFrame>,
}

impl std::fmt::Debug for CapturedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("preview", &self.preview.is_some())
            .finish()
    }
}

/// One BGRA picture of the local preview.
#[derive(Clone, Debug)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    /// BGRA bytes, row-major, `width * 4` stride.
    pub bgra: Vec<u8>,
}

/// Last-value slot between the capture thread and the publishing engine.
#[derive(Default)]
pub struct LatestImage {
    slot: Mutex<Option<CapturedImage>>,
    published: AtomicU64,
    dropped: AtomicU64,
}

impl LatestImage {
    /// Store a newer image. The previous one is discarded, which is the drop
    /// counter of a consumer that falls behind.
    pub fn publish(&self, image: CapturedImage) {
        self.published.fetch_add(1, Ordering::Relaxed);
        let Ok(mut slot) = self.slot.lock() else {
            return;
        };
        if slot.replace(image).is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn take(&self) -> Option<CapturedImage> {
        self.slot.lock().ok()?.take()
    }

    pub fn clear(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = None;
        }
    }

    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Bounded store of the newest local preview picture for the on-air card.
#[derive(Default)]
pub struct PreviewStore {
    latest: Mutex<Option<PreviewFrame>>,
    published: AtomicU64,
    rejected: AtomicU64,
}

impl PreviewStore {
    /// Replace the stored picture. A picture above the memory bound is
    /// rejected; the previous one stays until the next acceptable frame.
    pub fn publish(&self, frame: PreviewFrame) -> bool {
        let bytes = frame.bgra.len();
        let expected = frame.width as usize * frame.height as usize * 4;
        if frame.width == 0
            || frame.height == 0
            || bytes != expected
            || bytes > MAX_PREVIEW_BYTES
            || frame.width > MAX_PREVIEW_WIDTH
        {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let Ok(mut slot) = self.latest.lock() else {
            return false;
        };
        *slot = Some(frame);
        self.published.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Take the newest picture. Only one consumer (the panel) exists, so the
    /// slot is emptied and an idle UI costs no memory after the last read.
    pub fn take(&self) -> Option<PreviewFrame> {
        self.latest.lock().ok()?.take()
    }

    pub fn clear(&self) {
        if let Ok(mut slot) = self.latest.lock() {
            *slot = None;
        }
    }

    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    /// Bytes currently held by the slot.
    pub fn bytes(&self) -> usize {
        self.latest
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|frame| frame.bgra.len()))
            .unwrap_or(0)
    }
}

/// One capture thread. Dropping it stops and joins the thread.
pub struct ScreenCapture {
    stop: Arc<AtomicBool>,
    latest: Arc<LatestImage>,
    failure: Arc<Mutex<Option<CaptureFailure>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ScreenCapture {
    /// Start capturing `source` into `latest`.
    ///
    /// `target` is the selected quality box; the real picture keeps the source
    /// aspect ratio inside that box and is always even-sized for I420. `fps`
    /// paces the pull loop so a 30 FPS publication does not convert twice as
    /// many frames as it sends.
    pub fn start(
        source: ScreenSource,
        target: (u32, u32),
        fps: u32,
        latest: Arc<LatestImage>,
    ) -> Result<Self, &'static str> {
        capability()?;
        let stop = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), &'static str>>();

        let worker_stop = stop.clone();
        let worker_latest = latest.clone();
        let worker_failure = failure.clone();
        let thread = std::thread::Builder::new()
            .name("voxhold-screen-capture".into())
            .spawn(move || {
                #[cfg(windows)]
                windows::run(
                    source,
                    target,
                    fps,
                    worker_latest,
                    worker_stop,
                    worker_failure,
                    ready_tx,
                );
                #[cfg(not(windows))]
                {
                    let _ = (
                        &source,
                        target,
                        fps,
                        &worker_latest,
                        &worker_stop,
                        &worker_failure,
                    );
                    let _ = ready_tx.send(Err("Захват экрана поддерживается только в Windows."));
                }
            })
            .map_err(|_| "Не удалось запустить поток захвата экрана.")?;

        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                latest,
                failure,
                thread: Some(thread),
            }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(message)
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                let _ = thread.join();
                Err("Захват экрана не запустился за 3 секунды.")
            }
        }
    }

    /// The slot the publishing engine reads. It is also the drop counter.
    pub fn latest(&self) -> &Arc<LatestImage> {
        &self.latest
    }

    pub fn failure(&self) -> Option<CaptureFailure> {
        self.failure.lock().ok().and_then(|failure| *failure)
    }

    /// Ask the thread to stop and wait a bounded time for it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.latest.clear();
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Fit `(width, height)` into `(max_width, max_height)`, keeping the aspect
/// ratio, and round both sides down to even numbers for I420. A source that
/// already fits is never upscaled.
pub fn fit_even(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let max_width = max_width.min(MAX_CAPTURE_SIDE).max(2);
    let max_height = max_height.min(MAX_CAPTURE_SIDE).max(2);
    if width == 0 || height == 0 {
        return (0, 0);
    }
    if width <= max_width && height <= max_height {
        return ((width & !1).max(2), (height & !1).max(2));
    }
    let (mut out_w, mut out_h) =
        if u64::from(max_width) * u64::from(height) <= u64::from(max_height) * u64::from(width) {
            (
                max_width,
                ((u64::from(max_width) * u64::from(height) / u64::from(width)) as u32).max(2),
            )
        } else {
            (
                ((u64::from(max_height) * u64::from(width) / u64::from(height)) as u32).max(2),
                max_height,
            )
        };
    out_w &= !1;
    out_h &= !1;
    (out_w.max(2), out_h.max(2))
}

/// BT.601 limited-range conversion of one averaged BGRA pixel.
fn bgra_to_yuv(red: u32, green: u32, blue: u32) -> (u8, u8, u8) {
    // The averages are `sum / samples`; the scale is 1/256 of the full value.
    let (red, green, blue) = (red as i32, green as i32, blue as i32);
    let y = ((66 * red + 129 * green + 25 * blue + 128) >> 8) + 16;
    let u = ((-38 * red - 74 * green + 112 * blue + 128) >> 8) + 128;
    let v = ((112 * red - 94 * green - 18 * blue + 128) >> 8) + 128;
    (
        y.clamp(0, 255) as u8,
        u.clamp(0, 255) as u8,
        v.clamp(0, 255) as u8,
    )
}

/// Convert a BGRA picture into `output` with a box filter.
///
/// `output` must be sized to the wanted picture. The source may have any
/// stride (including padding after every row) and odd sizes; the destination
/// is I420 and therefore always even. Returns `false` on an unusable input.
pub fn convert_bgra_to_i420(
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    output: &mut I420Buffer,
) -> bool {
    let (out_w, out_h) = (output.width(), output.height());
    if width == 0
        || height == 0
        || stride < width.saturating_mul(4)
        || out_w == 0
        || out_h == 0
        || out_w & 1 == 1
        || out_h & 1 == 1
    {
        return false;
    }
    if bgra.len() < stride as usize * height as usize {
        return false;
    }
    let (stride_y, stride_u, stride_v) = output.strides();
    let (stride_y, stride_u, stride_v) = (stride_y as usize, stride_u as usize, stride_v as usize);
    let chroma_width = output.chroma_width() as usize;
    let chroma_height = output.chroma_height() as usize;
    let (data_y, data_u, data_v) = output.data_mut();
    let box_mean = |x0: u32, x1: u32, y0: u32, y1: u32| -> (u32, u32, u32) {
        let mut red = 0_u32;
        let mut green = 0_u32;
        let mut blue = 0_u32;
        let mut samples = 0_u32;
        for y in y0..y1 {
            let row = y as usize * stride as usize;
            for x in x0..x1 {
                let at = row + x as usize * 4;
                // BGRA: blue, green, red, alpha.
                blue += u32::from(bgra[at]);
                green += u32::from(bgra[at + 1]);
                red += u32::from(bgra[at + 2]);
                samples += 1;
            }
        }
        let samples = samples.max(1);
        (red / samples, green / samples, blue / samples)
    };
    // One chroma sample covers a 2×2 block of destination luma pixels, so its
    // source box is the union of those four boxes.
    for chroma_y in 0..chroma_height {
        let y0 = (chroma_y as u64 * 2 * u64::from(height) / u64::from(out_h)) as u32;
        let y1 = (((chroma_y as u64 + 1) * 2 * u64::from(height) / u64::from(out_h)) as u32)
            .max(y0 + 1)
            .min(height);
        for chroma_x in 0..chroma_width {
            let x0 = (chroma_x as u64 * 2 * u64::from(width) / u64::from(out_w)) as u32;
            let x1 = (((chroma_x as u64 + 1) * 2 * u64::from(width) / u64::from(out_w)) as u32)
                .max(x0 + 1)
                .min(width);
            let (red, green, blue) = box_mean(x0, x1, y0, y1);
            let (_, u, v) = bgra_to_yuv(red, green, blue);
            data_u[chroma_y * stride_u + chroma_x] = u;
            data_v[chroma_y * stride_v + chroma_x] = v;
        }
    }
    // Luma is written per destination pixel so a non-integer scale keeps the
    // geometry; the chroma pass above used the same source boxes.
    for out_y in 0..out_h {
        let y0 = ((out_y as u64) * u64::from(height) / u64::from(out_h)) as u32;
        let y1 = ((((out_y as u64) + 1) * u64::from(height) / u64::from(out_h)) as u32)
            .max(y0 + 1)
            .min(height);
        for out_x in 0..out_w {
            let x0 = ((out_x as u64) * u64::from(width) / u64::from(out_w)) as u32;
            let x1 = ((((out_x as u64) + 1) * u64::from(width) / u64::from(out_w)) as u32)
                .max(x0 + 1)
                .min(width);
            let (red, green, blue) = box_mean(x0, x1, y0, y1);
            let (y, _, _) = bgra_to_yuv(red, green, blue);
            data_y[out_y as usize * stride_y + out_x as usize] = y;
        }
    }
    true
}

/// Nearest-neighbour BGRA preview built from an I420 picture.
pub fn preview_from_i420(source: &I420Buffer, max_width: u32) -> Option<PreviewFrame> {
    let (width, height) = (source.width(), source.height());
    if width == 0 || height == 0 {
        return None;
    }
    let (out_w, out_h) = fit_even(width, height, max_width.max(2), u32::MAX / 4);
    if out_w == 0 || out_h == 0 {
        return None;
    }
    let (stride_y, stride_u, stride_v) = source.strides();
    let (data_y, data_u, data_v) = source.data();
    let chroma_width = source.chroma_width();
    let mut bgra = vec![0_u8; out_w as usize * out_h as usize * 4];
    for y in 0..out_h {
        let source_y = ((u64::from(y) * u64::from(height) / u64::from(out_h)) as usize)
            .min(height as usize - 1);
        for x in 0..out_w {
            let source_x = ((u64::from(x) * u64::from(width) / u64::from(out_w)) as usize)
                .min(width as usize - 1);
            let luma = f32::from(data_y[source_y * stride_y as usize + source_x]);
            let chroma_x = (source_x / 2).min(chroma_width as usize - 1);
            let chroma_y = (source_y / 2).min(source.chroma_height() as usize - 1);
            let u = f32::from(data_u[chroma_y * stride_u as usize + chroma_x]) - 128.0;
            let v = f32::from(data_v[chroma_y * stride_v as usize + chroma_x]) - 128.0;
            let c = (luma - 16.0).max(0.0) * 1.164;
            let red = (c + 1.596 * v).clamp(0.0, 255.0) as u8;
            let green = (c - 0.392 * u - 0.813 * v).clamp(0.0, 255.0) as u8;
            let blue = (c + 2.017 * u).clamp(0.0, 255.0) as u8;
            let at = (y as usize * out_w as usize + x as usize) * 4;
            bgra[at] = blue;
            bgra[at + 1] = green;
            bgra[at + 2] = red;
            bgra[at + 3] = 255;
        }
    }
    Some(PreviewFrame {
        width: out_w,
        height: out_h,
        bgra,
    })
}

#[cfg(windows)]
mod windows {
    use super::*;
    use ::windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use ::windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };
    use ::windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    };
    use ::windows::core::BOOL;
    use libwebrtc::desktop_capturer::{
        CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
    };
    use std::sync::mpsc::Sender;
    use std::time::{Duration, Instant};

    /// Time between `capture_frame` calls. The capturer is pulled, so its call
    /// rate is the capture frame rate; frames above the encoder rate are
    /// dropped by [`LatestImage`].
    const MAX_FPS: u32 = 60;

    pub(super) fn capturer_available() -> bool {
        let options = DesktopCapturerOptions::new(DesktopCaptureSourceType::Window);
        DesktopCapturer::new(options).is_some()
    }

    fn source_type(kind: SourceKind) -> DesktopCaptureSourceType {
        match kind {
            SourceKind::Monitor => DesktopCaptureSourceType::Screen,
            SourceKind::Window => DesktopCaptureSourceType::Window,
        }
    }

    pub(super) fn list_sources(kind: SourceKind) -> Result<Vec<ScreenSource>, &'static str> {
        let options = DesktopCapturerOptions::new(source_type(kind));
        let Some(capturer) = DesktopCapturer::new(options) else {
            return Err("Системный захват экрана недоступен.");
        };
        let sources = capturer.get_source_list();
        let own = own_process_windows();
        let monitors = match kind {
            SourceKind::Monitor => monitor_names(),
            SourceKind::Window => Vec::new(),
        };
        let mut result = Vec::with_capacity(sources.len());
        for source in sources {
            let id = source.id();
            let title = source.title();
            match kind {
                SourceKind::Monitor => {
                    // The pinned capturer reports `display_id = -1` on this
                    // host, so the name/size are matched by the capturer's
                    // source order. That is a known limitation: a reordered
                    // source list can attach a neighbour's size to a monitor
                    // until the capturer exposes a stable display id.
                    let (name, width, height) = monitors
                        .get(id as usize)
                        .cloned()
                        .unwrap_or_else(|| (format!("Экран {}", id + 1), 0, 0));
                    result.push(ScreenSource {
                        id,
                        kind,
                        title: name,
                        display_id: source.display_id(),
                        width,
                        height,
                    });
                }
                SourceKind::Window => {
                    if own.contains(&id) {
                        continue;
                    }
                    let title = title.trim().to_owned();
                    if title.is_empty() {
                        continue;
                    }
                    let (width, height) = window_size(id);
                    result.push(ScreenSource {
                        id,
                        kind,
                        title,
                        display_id: source.display_id(),
                        width,
                        height,
                    });
                }
            }
        }
        Ok(result)
    }

    /// `HWND` values of this process. Window sources are top-level windows, so
    /// a match means the picker would mirror the client itself.
    fn own_process_windows() -> std::collections::HashSet<u64> {
        let process = std::process::id();
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let state = unsafe { &mut *(lparam.0 as *mut (u32, std::collections::HashSet<u64>)) };
            let mut owner = 0_u32;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut owner));
            }
            if owner == state.0 {
                state.1.insert(hwnd.0 as u64);
            }
            BOOL(1)
        }
        let mut state = (process, std::collections::HashSet::<u64>::new());
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut state as *mut _ as isize));
        }
        state.1
    }

    unsafe extern "system" fn monitor_callback(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let list = unsafe { &mut *(lparam.0 as *mut Vec<(String, u32, u32)>) };
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let ok = unsafe {
            GetMonitorInfoW(
                monitor,
                &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
            )
            .as_bool()
        };
        if ok {
            let mut device = String::new();
            for unit in info.szDevice {
                if unit == 0 {
                    break;
                }
                device.push(char::from_u32(u32::from(unit)).unwrap_or('?'));
            }
            let rect = info.monitorInfo.rcMonitor;
            let name = if device.is_empty() {
                format!("Экран {}", list.len() + 1)
            } else {
                format!("Экран {} ({device})", list.len() + 1)
            };
            list.push((
                name,
                (rect.right - rect.left).max(0) as u32,
                (rect.bottom - rect.top).max(0) as u32,
            ));
        }
        BOOL(1)
    }

    /// Display names and sizes in the same order the capturer lists monitors.
    fn monitor_names() -> Vec<(String, u32, u32)> {
        let mut list: Vec<(String, u32, u32)> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(monitor_callback),
                LPARAM(&mut list as *mut _ as isize),
            );
        }
        list
    }

    fn window_size(id: u64) -> (u32, u32) {
        let mut rect = RECT::default();
        let hwnd = HWND(id as *mut _);
        let ok = unsafe { GetWindowRect(hwnd, &mut rect).is_ok() };
        if !ok || !unsafe { IsWindowVisible(hwnd).as_bool() } {
            return (0, 0);
        }
        (
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run(
        source: ScreenSource,
        target: (u32, u32),
        fps: u32,
        latest: Arc<LatestImage>,
        stop: Arc<AtomicBool>,
        failure: Arc<Mutex<Option<CaptureFailure>>>,
        ready: Sender<Result<(), &'static str>>,
    ) {
        let mut options = DesktopCapturerOptions::new(source_type(source.kind));
        options.set_include_cursor(true);
        let Some(mut capturer) = DesktopCapturer::new(options) else {
            let _ = ready.send(Err("Системный захват экрана недоступен."));
            return;
        };
        let selected = capturer
            .get_source_list()
            .into_iter()
            .find(|candidate| candidate.id() == source.id);
        let Some(selected) = selected else {
            let _ = ready.send(Err("Выбранный источник захвата больше недоступен."));
            return;
        };
        if let Ok(mut slot) = failure.lock() {
            *slot = None;
        }
        let converter_latest = latest.clone();
        let converter_failure = failure.clone();
        capturer.start_capture(Some(selected), move |result| match result {
            Ok(frame) => {
                let (width, height) = (frame.width().max(0) as u32, frame.height().max(0) as u32);
                let output = fit_even(width, height, target.0, target.1);
                if output == (0, 0) {
                    return;
                }
                let mut buffer = I420Buffer::new(output.0, output.1);
                if convert_bgra_to_i420(frame.data(), width, height, frame.stride(), &mut buffer) {
                    let preview = preview_from_i420(&buffer, MAX_PREVIEW_WIDTH);
                    converter_latest.publish(CapturedImage {
                        width: output.0,
                        height: output.1,
                        i420: buffer,
                        preview,
                    });
                } else if let Ok(mut slot) = converter_failure.lock() {
                    *slot = Some(CaptureFailure::Permanent);
                }
            }
            Err(CaptureError::Temporary) => {
                if let Ok(mut slot) = converter_failure.lock() {
                    *slot = Some(CaptureFailure::Temporary);
                }
            }
            Err(CaptureError::Permanent) => {
                if let Ok(mut slot) = converter_failure.lock() {
                    *slot = Some(CaptureFailure::Permanent);
                }
            }
        });
        let _ = ready.send(Ok(()));
        let paced = fps.clamp(1, MAX_FPS);
        let interval = Duration::from_micros(1_000_000 / u64::from(paced));
        while !stop.load(Ordering::SeqCst) {
            if let Ok(slot) = failure.lock() {
                if matches!(*slot, Some(CaptureFailure::Permanent)) {
                    break;
                }
            }
            let began = Instant::now();
            capturer.capture_frame();
            if let Some(delay) = interval.checked_sub(began.elapsed()) {
                std::thread::sleep(delay);
            }
        }
    }
}

#[cfg(test)]
mod tests;
