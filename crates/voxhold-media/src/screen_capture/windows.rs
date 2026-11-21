//! Windows desktop capturer, source enumeration and the bounded picker
//! thumbnail. Kept apart from the pixel path so the BGRA/I420 conversion and
//! the fit rules stay in one file while the OS captures stay reviewable here.

use super::*;
use ::windows::Win32::Foundation::{HWND, LPARAM, RECT};
use ::windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
};
use ::windows::core::{BOOL, PCWSTR};
use libwebrtc::desktop_capturer::{
    CaptureError, CaptureSource, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
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
        SourceKind::Monitor => monitor_descriptors(),
        SourceKind::Window => Vec::new(),
    };
    let translated = match kind {
        SourceKind::Monitor => capturer_monitors(&sources),
        SourceKind::Window => Vec::new(),
    };
    let mapping = match kind {
        SourceKind::Monitor => confirmed_mapping(&monitors, &translated),
        SourceKind::Window => Vec::new(),
    };
    let mut result = Vec::with_capacity(sources.len());
    for source in sources {
        let id = source.id();
        let title = source.title();
        match kind {
            SourceKind::Monitor => {
                // The pinned capturer reports `display_id = -1` and an empty
                // title on this host. Identity and metadata come from the
                // `EnumDisplayDevicesW` device at this id's index, never from
                // the title or the list position. A row without a device (a
                // synthetic DirectX source) stays identity-less, so it cannot
                // be selected or captured.
                let title = title.trim().to_owned();
                let matched = mapping
                    .iter()
                    .find(|(source_id, _)| *source_id == id)
                    .map(|(_, monitor)| monitor);
                let (width, height) = matched.map(|monitor| monitor.size()).unwrap_or((0, 0));
                // The caption is never an identity; the empty Windows title
                // becomes a stable human label derived from the adapter index.
                let title = if title.is_empty() {
                    format!("Экран {}", id + 1)
                } else {
                    title
                };
                result.push(ScreenSource {
                    id,
                    kind,
                    title,
                    display_id: source.display_id(),
                    width,
                    height,
                    monitor: matched.cloned(),
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
                    monitor: None,
                });
            }
        }
    }
    Ok(result)
}

/// Capture exactly one preview frame for the picker. The capturer is local
/// to this call, so it cannot disturb a running publication. Only one frame
/// can enter the bounded channel; later callbacks are dropped.
pub(super) fn capture_thumbnail(
    source: &ScreenSource,
    max_width: u32,
) -> Result<PreviewFrame, ThumbnailError> {
    let mut options = DesktopCapturerOptions::new(source_type(source.kind));
    // A cursor in a small card says nothing about the source and moves
    // between attempts; the picker preview deliberately excludes it.
    options.set_include_cursor(false);
    let Some(mut capturer) = DesktopCapturer::new(options) else {
        return Err(ThumbnailError::Unavailable);
    };
    let list = capturer.get_source_list();
    let selected_id = match source.kind {
        SourceKind::Monitor => {
            let Some(identity) = source.monitor.as_ref() else {
                return Err(ThumbnailError::Changed);
            };
            let monitors = monitor_descriptors();
            let sources = capturer_monitors(&list);
            resolve_monitor(identity, &monitors, &sources).map_err(|error| match error {
                ResolveError::Missing => ThumbnailError::Missing,
                ResolveError::Ambiguous | ResolveError::TopologyChanged => ThumbnailError::Changed,
            })?
        }
        // A window id is its `HWND`, a durable identity of its own.
        SourceKind::Window => source.id,
    };
    let Some(selected) = list
        .into_iter()
        .find(|candidate| candidate.id() == selected_id)
    else {
        return Err(ThumbnailError::Missing);
    };
    // The frame is only valid inside the callback, so the downscale and
    // the copy both happen there. A one-slot channel bounds the memory.
    let (sender, receiver) = std::sync::mpsc::sync_channel::<PreviewFrame>(1);
    capturer.start_capture(Some(selected), move |result| {
        if let Ok(frame) = result {
            let width = frame.width().max(0) as u32;
            let height = frame.height().max(0) as u32;
            if let Some(preview) =
                preview_from_bgra(frame.data(), width, height, frame.stride(), max_width)
            {
                let _ = sender.try_send(preview);
            }
        }
    });
    let deadline = Instant::now() + THUMBNAIL_TIMEOUT;
    loop {
        capturer.capture_frame();
        match receiver.try_recv() {
            Ok(preview) => return Ok(preview),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(ThumbnailError::Unavailable);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err(ThumbnailError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Device names in `EnumDisplayDevicesW(NULL, index, ...)` order.
///
/// The pinned capturer lists a Windows screen as `(index, "")`: the id is
/// exactly the index of this enumeration and the title is always empty. The
/// index is only valid for one enumeration, so it is translated to the durable
/// `\\.\DISPLAYn` name before it becomes an identity.
fn display_devices() -> Vec<String> {
    let mut devices = Vec::new();
    let mut index = 0_u32;
    loop {
        let mut device = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        let found = unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) };
        if !found.as_bool() {
            return devices;
        }
        devices.push(wide_to_string(&device.DeviceName));
        index += 1;
    }
}

/// Translate the capturer's numeric screen ids to the Windows device names
/// they name in this enumeration.
///
/// A row whose id is beyond the display-device list (a synthetic DirectX
/// source) has no device name and stays identity-less.
fn capturer_monitors(sources: &[CaptureSource]) -> Vec<CapturerMonitor> {
    let devices = display_devices();
    sources
        .iter()
        .map(|source| CapturerMonitor {
            id: source.id(),
            device: usize::try_from(source.id())
                .ok()
                .and_then(|index| devices.get(index))
                .cloned()
                .unwrap_or_default(),
        })
        .collect()
}

/// A NUL-terminated wide string from the Win32 fixed-size buffers.
fn wide_to_string(units: &[u16]) -> String {
    let len = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    String::from_utf16_lossy(&units[..len])
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
    let list = unsafe { &mut *(lparam.0 as *mut Vec<MonitorDescriptor>) };
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
        let rect = info.monitorInfo.rcMonitor;
        let work = info.monitorInfo.rcWork;
        list.push(MonitorIdentity {
            device: wide_to_string(&info.szDevice),
            bounds: MonitorRect::new(
                rect.left,
                rect.top,
                (rect.right - rect.left).max(0) as u32,
                (rect.bottom - rect.top).max(0) as u32,
            ),
            work: MonitorRect::new(
                work.left,
                work.top,
                (work.right - work.left).max(0) as u32,
                (work.bottom - work.top).max(0) as u32,
            ),
            width: (rect.right - rect.left).max(0) as u32,
            height: (rect.bottom - rect.top).max(0) as u32,
        });
    }
    BOOL(1)
}

/// Monitor descriptors in enumeration order. The order is only a cache
/// detail; a capturer source is matched by the device name Windows reports
/// for its id, never by this position.
pub(super) fn monitor_descriptors() -> Vec<MonitorDescriptor> {
    let mut list: Vec<MonitorDescriptor> = Vec::new();
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
    demand: Arc<PreviewDemand>,
    ready: Sender<Result<(), &'static str>>,
) {
    let mut options = DesktopCapturerOptions::new(source_type(source.kind));
    options.set_include_cursor(true);
    let Some(mut capturer) = DesktopCapturer::new(options) else {
        let _ = ready.send(Err("Системный захват экрана недоступен."));
        return;
    };
    let list = capturer.get_source_list();
    let selected_id = match source.kind {
        SourceKind::Monitor => {
            let Some(identity) = source.monitor.as_ref() else {
                let _ = ready.send(Err(
                    "Не удалось однозначно определить выбранный экран. Обновите список и выберите экран снова.",
                ));
                return;
            };
            let monitors = monitor_descriptors();
            let sources = capturer_monitors(&list);
            match resolve_monitor(identity, &monitors, &sources) {
                Ok(id) => id,
                Err(error) => {
                    let _ = ready.send(Err(error.message()));
                    return;
                }
            }
        }
        // A window id is its `HWND`, a durable identity of its own.
        SourceKind::Window => source.id,
    };
    let selected = list
        .into_iter()
        .find(|candidate| candidate.id() == selected_id);
    let Some(selected) = selected else {
        let _ = ready.send(Err(ResolveError::Missing.message()));
        return;
    };
    if let Ok(mut slot) = failure.lock() {
        *slot = None;
    }
    let converter_latest = latest.clone();
    let converter_failure = failure.clone();
    // The preview is paced here, on the capture thread. The published stream
    // keeps its target, fps and I420 conversion; only the extra BGRA copy for
    // the local card is gated.
    let mut throttle = PreviewThrottle::default();
    let preview_demand = demand;
    capturer.start_capture(Some(selected), move |result| match result {
        Ok(frame) => {
            let (width, height) = (frame.width().max(0) as u32, frame.height().max(0) as u32);
            let output = fit_even(width, height, target.0, target.1);
            if output == (0, 0) {
                return;
            }
            let mut buffer = I420Buffer::new(output.0, output.1);
            if convert_bgra_to_i420(frame.data(), width, height, frame.stride(), &mut buffer) {
                let picture = throttle
                    .poll(Instant::now(), &preview_demand)
                    .and_then(|max_width| preview_from_i420(&buffer, max_width));
                let preview = preview_demand.published_preview(picture);
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
