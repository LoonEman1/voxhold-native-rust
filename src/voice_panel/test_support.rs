//! Test-only hooks for the voice panel. They live in their own module so the
//! production file stays small and the seam is obviously not part of the UI.

use super::*;
use voxhold_media::publisher::Sharing;
use voxhold_media::screen_capture::ScreenSource;

impl VoicePanel {
    /// Install a handle that has no worker or socket, so `refresh_capture`
    /// queues the snapshot exactly as the live path would.
    pub fn test_attach_detached_handle(&mut self) {
        let handle = VoiceHandle::detached_for_tests();
        self.attempt = handle.attempt();
        self.handle = Some(handle);
    }

    /// The newest control snapshot waiting for the media layer, if any. The
    /// request slot holds one value, so `Some` means exactly one pending State.
    pub fn test_take_audio_snapshot(&mut self) -> Option<(Controls, bool)> {
        self.handle
            .as_ref()
            .and_then(|handle| handle.take_controls_for_tests())
    }

    /// The power transition queued for the media worker by the real handler.
    pub fn test_take_power(&mut self) -> Option<bool> {
        self.handle
            .as_ref()
            .and_then(|handle| handle.take_power_for_tests())
    }

    /// Open the picker without the OS capture gate and with a fixed list, so
    /// the keyboard and cancel paths are testable on any host.
    pub fn test_open_screen_picker(&mut self, source: ScreenSource) {
        self.test_share_gate = Some(Ok(()));
        self.screen = ScreenShareUi::default();
        self.screen.open = true;
        self.screen.picker = 7;
        self.screen.kind = voxhold_media::screen_capture::SourceKind::Monitor;
        self.screen.monitors = vec![source];
        self.screen.monitor_loading = false;
        self.screen.monitor_generation = 1;
        self.screen.window_loading = false;
        self.screen.window_generation = 1;
    }

    /// Replace the gate answer without a capture stack.
    pub fn test_set_share_gate(&mut self, gate: Result<(), &'static str>) {
        self.test_share_gate = Some(gate);
    }

    /// The reason the room would print next to the «Экран» action.
    pub fn test_screen_reason(&self) -> Option<&'static str> {
        self.screen_reason()
    }

    /// Structural check that the picker handles cannot collide with the
    /// toolbar or the on-air card.
    pub fn test_picker_focus_is_separate(&self) -> bool {
        self.picker_focus
            .iter()
            .all(|picker| !self.screen_focus.contains(picker))
    }

    /// Replace the screen-share snapshot as a worker update would.
    pub fn test_set_screen_sharing(&mut self, sharing: Sharing) {
        self.screen.sharing = sharing;
    }

    /// Queued screen actions of the detached handle, as (kind, picker) pairs.
    pub fn test_take_screen_commands(&mut self) -> Vec<(&'static str, u64)> {
        self.handle
            .as_ref()
            .map(|handle| handle.take_screen_commands_for_tests())
            .unwrap_or_default()
    }

    /// Debug label of the focused picker element, e.g. `Source(0)` or `Start`.
    pub fn test_focused_share(&self, window: &Window) -> Option<String> {
        self.screen
            .ring
            .iter()
            .find(|(handle, _)| handle.is_focused(window))
            .map(|(_, key)| format!("{key:?}"))
    }

    /// One roster row by connection id, for a test that needs to change the
    /// server-authoritative fields without an event helper.
    pub fn test_self_participant(
        &self,
        connection: &str,
    ) -> Option<voxhold_media::controller::Participant> {
        self.participants
            .iter()
            .find(|participant| participant.connection_id == connection)
            .cloned()
    }
}
