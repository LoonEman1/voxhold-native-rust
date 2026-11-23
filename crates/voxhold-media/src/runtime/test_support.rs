//! Inert constructors used by the UI snapshot tests. They live here so the
//! runtime module keeps its size and the seam is a separate, obvious surface.

use super::*;
use crate::runtime::requests::ScreenCommand;

impl VoiceHandle {
    /// A handle with no worker and no socket. The settings tests use it to
    /// observe exactly which control snapshot the UI hands to the media layer;
    /// nothing is ever joined or written to the device.
    pub fn detached_for_tests() -> Self {
        Self::detached_for_tests_with_wake(Arc::new(Notify::new()))
    }

    /// A detached handle that signals `wake` exactly like a joined handle. The
    /// panel passes its own wake here, so a test observes the production
    /// wake path instead of a timer.
    pub fn detached_for_tests_with_wake(wake: Arc<Notify>) -> Self {
        static ATTEMPT: AtomicU64 = AtomicU64::new(1);
        Self {
            requests: Arc::new(Requests::default()),
            updates: Arc::new(Updates::with_wake(wake.clone())),
            wake: wake.clone(),
            frame: Arc::new(Mutex::new(None)),
            capture: Arc::new(CaptureGate::default()),
            screen_preview: Arc::new(crate::screen_capture::PreviewStore::with_wake(wake)),
            screen_demand: Arc::new(crate::screen_capture::PreviewDemand::default()),
            screen_picker: Arc::new(AtomicU64::new(1)),
            source_generation: Arc::new(AtomicU64::new(1)),
            attempt: ATTEMPT.fetch_add(1, Ordering::SeqCst),
            worker: None,
            finished: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    /// A detached handle that signals `wake` and whose first authorization is
    /// deferred exactly like `join_with_rotation(..., true)`. No socket
    /// exists; only the shared rotation gate is observable.
    pub fn detached_for_tests_with_wake_and_rotation(
        wake: Arc<Notify>,
        rotation_pending: bool,
    ) -> Self {
        let handle = Self::detached_for_tests_with_wake(wake);
        if rotation_pending {
            handle.requests.rotation_started(Instant::now());
        }
        handle
    }

    /// Push one update through the real coalescing queue, exactly as the
    /// worker does. The panel tests use this to model a burst that is read as
    /// one latest snapshot.
    pub fn push_update_for_tests(&self, update: VoiceUpdate) {
        self.updates.push(update);
    }

    /// Whether a rotation currently owns the next authorization.
    pub fn test_rotation_active(&self) -> bool {
        self.requests
            .rotation
            .lock()
            .map(|rotation| rotation.active())
            .unwrap_or(false)
    }

    /// Whether the worker may open its first socket now.
    pub fn test_rotation_authorizes(&self) -> bool {
        self.requests
            .rotation
            .lock()
            .map(|rotation| rotation.authorization_allowed(Instant::now()))
            .unwrap_or(true)
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

    /// Put one preview picture into the shared slot exactly as the publisher
    /// worker does when the capture attaches it to a frame.
    pub fn push_screen_preview_for_tests(&self, frame: crate::screen_capture::PreviewFrame) {
        self.screen_preview.publish(frame);
    }

    /// Publish one prepared player picture and wake the panel, exactly like
    /// the session worker's `StreamFrame` slot does.
    pub fn push_stream_frame_for_tests(&self, frame: StreamFrame) {
        if let Ok(mut slot) = self.frame.lock() {
            *slot = Some(frame);
        }
        self.wake.notify_one();
    }

    /// The demand this handle shares with the capture thread. A test reads
    /// `is_enabled`/`width`/counters from it without a worker.
    pub fn screen_preview_demand_for_tests(&self) -> Arc<crate::screen_capture::PreviewDemand> {
        self.screen_demand.clone()
    }

    /// Whether the shared preview slot still holds a picture. Read-only, so a
    /// test can prove that a discarded frame did not stay in the slot.
    pub fn screen_preview_pending_for_tests(&self) -> bool {
        self.screen_preview.bytes() > 0
    }

    /// The newest viewport the UI requested. The slot is a last-value cell, so
    /// `None` after a take proves that an unchanged size was not rewritten.
    pub fn take_viewport_for_tests(&self) -> Option<(u32, u32)> {
        self.requests.viewport.lock().ok()?.take()
    }

    /// Take the player's pending picture request exactly like the worker does.
    /// `true` means one conversion is allowed.
    pub fn take_frame_request_for_tests(&self) -> bool {
        self.requests.frame_request.swap(false, Ordering::SeqCst)
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

    /// Drain the queued screen actions as `(kind, target)` pairs. The detached
    /// handle has no worker, so this is exactly what the UI sent. The target is
    /// the picker id for picker actions and the real quality-operation id for
    /// `restart`, `bitrate` and `cancel-restart`; `stop` has none.
    pub fn take_screen_commands_for_tests(&self) -> Vec<(&'static str, u64)> {
        self.take_screen_requests_for_tests()
            .into_iter()
            .map(|(kind, target, _)| (kind, target))
            .collect()
    }

    /// Drain the queued screen actions with the codec of the request they
    /// carry. Only publish/restart/bitrate commands have a concrete codec; the
    /// other actions describe no payload.
    pub fn take_screen_requests_for_tests(
        &self,
    ) -> Vec<(&'static str, u64, Option<crate::publisher::Codec>)> {
        let Ok(mut queue) = self.requests.screen.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Some(command) = queue.pop_front() {
            out.push(match command {
                ScreenCommand::OpenPicker { picker } => ("open", picker, None),
                ScreenCommand::Publish {
                    picker, request, ..
                } => ("publish", picker, Some(request.codec)),
                ScreenCommand::Restart { op, request, .. } => ("restart", op, Some(request.codec)),
                ScreenCommand::LiveQuality { op, request, .. } => {
                    ("bitrate", op, Some(request.codec))
                }
                ScreenCommand::CancelRestart { op } => ("cancel-restart", op, None),
                ScreenCommand::CancelPicker { picker } => ("cancel", picker, None),
                ScreenCommand::ContinueOnCpu => ("continue-cpu", 0, None),
                ScreenCommand::Stop => ("stop", 0, None),
            });
        }
        out
    }

    /// Drain the queued stream intents as `(kind, selection)` pairs. A route
    /// between channels must not add a watch or a leave for the same session.
    pub fn take_stream_commands_for_tests(&self) -> Vec<(&'static str, u64)> {
        let Ok(mut queue) = self.requests.stream.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Some(command) = queue.take() {
            out.push(match command {
                StreamCommand::Watch { selection, .. } => ("watch", selection),
                StreamCommand::Close { selection } => ("close", selection),
            });
        }
        out
    }

    /// The newest viewer mute the UI sent. The slot holds one value.
    pub fn take_stream_mute_for_tests(&self) -> Option<(u64, bool)> {
        self.requests.stream_mute.lock().ok()?.take()
    }
}
