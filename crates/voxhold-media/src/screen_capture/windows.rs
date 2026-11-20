//! Windows desktop capturer, source enumeration and the bounded picker
//! thumbnail. Kept apart from the pixel path so the BGRA/I420 conversion and
//! the fit rules stay in one file while the OS captures stay reviewable here.

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
        SourceKind::Monitor => monitor_descriptors(),
        SourceKind::Window => Vec::new(),
    };
    let capturer_monitors: Vec<CapturerMonitor> = sources
        .iter()
        .map(|source| CapturerMonitor {
            id: source.id(),
            title: source.title(),
        })
        .collect();
    let mapping = match kind {
        SourceKind::Monitor => confirmed_mapping(&monitors, &capturer_monitors),
        SourceKind::Window => Vec::new(),
    };
    let topology = TopologySnapshot::from_monitors(monitors.clone());
    let mut result = Vec::with_capacity(sources.len());
    for source in sources {
        let id = source.id();
        let title = source.title();
        match kind {
            SourceKind::Monitor => {
                // The pinned capturer reports `display_id = -1` on this
                // host. Identity and metadata are matched by the capturer's
                // own title against the Windows device list, never by list
                // position. An unmatched or ambiguous title keeps the row
                // identity-less, so it cannot be selected or captured.
                let title = title.trim().to_owned();
                let matched = mapping
                    .iter()
                    .find(|(source_id, _)| *source_id == id)
                    .map(|(_, monitor)| monitor);
                let (width, height) = matched.map(|monitor| monitor.size()).unwrap_or((0, 0));
                let title = if title.is_empty() {
                    matched
                        .map(|monitor| monitor.device.trim().to_owned())
                        .filter(|device| !device.is_empty())
                        .unwrap_or_else(|| format!("Экран {}", id + 1))
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
                    topology: Some(topology.clone()),
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
                    topology: None,
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
            let capturer_monitors: Vec<CapturerMonitor> = list
                .iter()
                .map(|candidate| CapturerMonitor {
                    id: candidate.id(),
                    title: candidate.title(),
                })
                .collect();
            resolve_monitor(identity, &monitors, &capturer_monitors).map_err(
                |error| match error {
                    ResolveError::Missing => ThumbnailError::Missing,
                    ResolveError::Ambiguous | ResolveError::TopologyChanged => {
                        ThumbnailError::Changed
                    }
                },
            )?
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
        let mut device = String::new();
        for unit in info.szDevice {
            if unit == 0 {
                break;
            }
            device.push(char::from_u32(u32::from(unit)).unwrap_or('?'));
        }
        let rect = info.monitorInfo.rcMonitor;
        let work = info.monitorInfo.rcWork;
        list.push(MonitorIdentity {
            device,
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
/// detail; the topology mapping matches a capturer source by its title and
/// device name, never by this position.
fn monitor_descriptors() -> Vec<MonitorDescriptor> {
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

/// The current configuration, for the first-frame topology check.
pub(super) fn topology_snapshot() -> TopologySnapshot {
    TopologySnapshot::from_monitors(monitor_descriptors())
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
            let capturer_monitors: Vec<CapturerMonitor> = list
                .iter()
                .map(|candidate| CapturerMonitor {
                    id: candidate.id(),
                    title: candidate.title(),
                })
                .collect();
            match resolve_monitor(identity, &monitors, &capturer_monitors) {
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
