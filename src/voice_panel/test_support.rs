//! Test-only hooks for the voice panel. They live in their own module so the
//! production file stays small and the seam is obviously not part of the UI.

use super::*;

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
