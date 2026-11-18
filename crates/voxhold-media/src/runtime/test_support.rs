//! Inert constructors used by the UI snapshot tests. They live here so the
//! runtime module keeps its size and the seam is a separate, obvious surface.

use super::*;

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
}
