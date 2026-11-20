//! Inert constructors used by the UI snapshot tests. They live here so the
//! runtime module keeps its size and the seam is a separate, obvious surface.

use super::*;
use crate::runtime::requests::ScreenCommand;

impl VoiceHandle {
    /// A handle with no worker and no socket. The settings tests use it to
    /// observe exactly which control snapshot the UI hands to the media layer;
    /// nothing is ever joined or written to the device.
    pub fn detached_for_tests() -> Self {
        static ATTEMPT: AtomicU64 = AtomicU64::new(1);
        Self {
            requests: Arc::new(Requests::default()),
            updates: Arc::new(Updates::new()),
            frame: Arc::new(Mutex::new(None)),
            capture: Arc::new(CaptureGate::default()),
            screen_preview: Arc::new(crate::screen_capture::PreviewStore::default()),
            screen_picker: Arc::new(AtomicU64::new(1)),
            source_generation: Arc::new(AtomicU64::new(1)),
            attempt: ATTEMPT.fetch_add(1, Ordering::SeqCst),
            worker: None,
            finished: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    /// The newest control snapshot waiting for the media worker, if the UI
    /// queued one. The slot holds one value, so this is also the count of
    /// pending snapshots.
    pub fn take_controls_for_tests(&self) -> Option<(Controls, bool)> {
        self.requests.controls.lock().ok()?.take()
    }

    /// The queued power transition, if the UI forwarded one.
    pub fn take_power_for_tests(&self) -> Option<bool> {
        self.requests.power.lock().ok()?.take()
    }

    /// The newest viewport the UI requested. The slot is a last-value cell, so
    /// `None` after a take proves that an unchanged size was not rewritten.
    pub fn take_viewport_for_tests(&self) -> Option<(u32, u32)> {
        self.requests.viewport.lock().ok()?.take()
    }

    /// The newest screen gate the UI pushed for the worker while a picker or
    /// an attempt was open.
    pub fn take_screen_access_for_tests(&self) -> Option<(bool, bool)> {
        self.requests
            .screen_access
            .lock()
            .ok()?
            .take()
            .map(|access| (access.stream_permission, access.slot_available))
    }

    /// Drain the queued screen actions as (kind, picker) pairs. The detached
    /// handle has no worker, so this is exactly what the UI sent.
    pub fn take_screen_commands_for_tests(&self) -> Vec<(&'static str, u64)> {
        let mut out = Vec::new();
        while let Some(pending) = self.requests.take() {
            let Pending::Screen(command) = pending else {
                continue;
            };
            out.push(match command {
                ScreenCommand::OpenPicker { picker } => ("open", picker),
                ScreenCommand::Publish { picker, .. } => ("publish", picker),
                ScreenCommand::Restart { .. } => ("restart", 0),
                ScreenCommand::LiveQuality { .. } => ("bitrate", 0),
                ScreenCommand::CancelRestart => ("cancel-restart", 0),
                ScreenCommand::CancelPicker { picker } => ("cancel", picker),
                ScreenCommand::Stop => ("stop", 0),
            });
        }
        out
    }
}
