use super::*;

/// Permission to transmit is tracked independently of the socket worker, so a
/// blocked `send` cannot keep the microphone open. Transmission requires all
/// of: no leave latch, a confirmed join, server permission and the local
/// mute/deafen/push-to-talk decision. The epoch advances only when the actual
/// transmit permission changes, so the capture thread flushes WinMM buffers on
/// mute/PTT edges (including a fast press/release) but not on DSP slider moves.
pub struct CaptureGate {
    state: Mutex<CaptureState>,
    epoch: Arc<AtomicU64>,
}

impl Default for CaptureGate {
    fn default() -> Self {
        Self {
            state: Mutex::new(CaptureState::default()),
            epoch: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[derive(Default)]
struct CaptureState {
    latched: bool,
    joined: bool,
    server_allowed: bool,
    local_allowed: bool,
    track: Option<RtcAudioTrack>,
    /// DSP parameters shared with the software capture thread. `muted` is
    /// recomputed from the permission flags, never from a possibly stale
    /// queued `State` command.
    params: Arc<Mutex<AudioParams>>,
    source: Option<libwebrtc::audio_source::native::NativeAudioSource>,
    /// Identity of the capture instance currently feeding `source`. A new
    /// capture may start before its track replaces the sender, so its
    /// acknowledgement must never enable the previous track.
    capture: Option<u64>,
    /// Newest permission generation whose old WinMM buffers were flushed and
    /// discarded by `capture`. A software track stays disabled until this
    /// catches up with `CaptureGate::epoch`.
    acked: Option<u64>,
}

impl CaptureGate {
    fn is_enabled(state: &CaptureState) -> bool {
        !state.latched && state.joined && state.server_allowed && state.local_allowed
    }

    /// A software track may only transmit after its capture thread confirmed
    /// the current generation. The platform ADM has no software capture thread,
    /// so its track follows the permission flags directly.
    fn track_enabled(&self, state: &CaptureState) -> bool {
        Self::is_enabled(state)
            && (state.source.is_none()
                || (state.capture.is_some()
                    && state
                        .acked
                        .is_some_and(|acked| acked >= self.epoch.load(Ordering::Acquire))))
    }

    fn apply(&self, state: &CaptureState) {
        let enabled = Self::is_enabled(state);
        if let Some(track) = &state.track {
            track.set_enabled(self.track_enabled(state));
        }
        if let Ok(mut params) = state.params.lock() {
            params.muted = !enabled;
        }
        if !enabled {
            // The software path must not replay a block captured before the
            // mute, PTT release, leave or server revoke.
            if let Some(source) = &state.source {
                source.clear_buffer();
            }
        }
    }

    /// Update the DSP snapshot without ever reopening a closed capture: the
    /// mute bit is owned by the permission flags above.
    pub fn set_params(&self, params: AudioParams) {
        let state = self.state.lock().unwrap();
        let mut params = params.clamped();
        params.muted = !Self::is_enabled(&state);
        if let Ok(mut shared) = state.params.lock() {
            *shared = params;
        }
    }

    pub fn params(&self) -> Arc<Mutex<AudioParams>> {
        self.state.lock().unwrap().params.clone()
    }

    /// The capture thread compares this counter with its last seen value. A
    /// fast PTT press/release may change and restore `muted` between two
    /// polls; the counter still changed, so the old driver buffers are
    /// discarded.
    pub fn permission_epoch(&self) -> Arc<AtomicU64> {
        self.epoch.clone()
    }

    fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Leave/logout/PTT release for the whole attempt: a late `State` command
    /// can never reopen the capture because the latch outranks every flag.
    pub fn latch(&self) {
        let mut state = self.state.lock().unwrap();
        let was = Self::is_enabled(&state);
        state.latched = true;
        if Self::is_enabled(&state) != was {
            self.bump();
        }
        self.apply(&state);
    }

    /// A new transport starts unconfirmed without server permission. The leave
    /// latch and local PTT decision belong to the handle, so a reconnect keeps
    /// both and a late `Start` after `leave` can never reopen the microphone.
    pub fn reset_transport(&self) {
        let mut state = self.state.lock().unwrap();
        let was = Self::is_enabled(&state);
        state.joined = false;
        state.server_allowed = false;
        if Self::is_enabled(&state) != was {
            self.bump();
        }
        self.apply(&state);
    }

    /// One transport ended. Clear only join/server permission and detach the
    /// track; the local decision and leave latch belong to the handle.
    pub fn end_transport(&self) {
        let mut state = self.state.lock().unwrap();
        let was = Self::is_enabled(&state);
        state.joined = false;
        state.server_allowed = false;
        if let Some(track) = state.track.take() {
            track.set_enabled(false);
        }
        state.source = None;
        state.capture = None;
        state.acked = None;
        if Self::is_enabled(&state) != was {
            self.bump();
        }
        self.apply(&state);
    }

    pub fn set_joined(&self, joined: bool) {
        let mut state = self.state.lock().unwrap();
        let was = Self::is_enabled(&state);
        state.joined = joined;
        if Self::is_enabled(&state) != was {
            self.bump();
        }
        self.apply(&state);
    }

    pub fn set_server_allowed(&self, allowed: bool) {
        let mut state = self.state.lock().unwrap();
        let was = Self::is_enabled(&state);
        state.server_allowed = allowed;
        if Self::is_enabled(&state) != was {
            self.bump();
        }
        self.apply(&state);
    }

    pub fn set_local(&self, allowed: bool) {
        let mut state = self.state.lock().unwrap();
        let was = Self::is_enabled(&state);
        state.local_allowed = allowed;
        if Self::is_enabled(&state) != was {
            self.bump();
        }
        self.apply(&state);
    }

    pub(super) fn attach(&self, track: RtcAudioTrack) {
        let mut state = self.state.lock().unwrap();
        state.track = Some(track);
        state.source = None;
        state.capture = None;
        state.acked = None;
        self.apply(&state);
    }

    /// Software path: the capture thread fills this source, so mute can also
    /// discard the buffered block immediately.
    pub(super) fn attach_source(
        &self,
        track: RtcAudioTrack,
        source: libwebrtc::audio_source::native::NativeAudioSource,
        capture: u64,
    ) {
        let mut state = self.state.lock().unwrap();
        state.track = Some(track);
        state.source = Some(source);
        state.capture = Some(capture);
        state.acked = None;
        self.apply(&state);
    }

    /// The capture thread `capture` has dropped every buffer from before
    /// `generation` and flushed WinMM. Only its own track may transmit again;
    /// a newer capture that started before `set_track` cannot acknowledge the
    /// old track.
    pub(crate) fn acknowledge(&self, capture: u64, generation: u64) {
        let mut state = self.state.lock().unwrap();
        if state.capture != Some(capture) {
            return;
        }
        state.acked = Some(
            state
                .acked
                .map_or(generation, |acked| acked.max(generation)),
        );
        self.apply(&state);
    }

    pub(super) fn detach(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(track) = state.track.take() {
            track.set_enabled(false);
        }
        state.source = None;
        state.capture = None;
        state.acked = None;
    }

    /// Test hook: the gate state is the one applied to the capture track.
    #[cfg(test)]
    pub fn enabled(&self) -> bool {
        let state = self.state.lock().unwrap();
        Self::is_enabled(&state)
    }
}
