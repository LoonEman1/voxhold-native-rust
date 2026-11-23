//! Test-only hooks of the compact PiP and of the stream player. They are part
//! of the same seam as `test_support`, split out so each file stays inside its
//! size budget and the PiP accessors sit next to the surface they describe.

use super::*;

impl VoicePanel {
    pub fn test_pip_active(&self) -> bool {
        self.pip_active()
    }

    pub fn test_screen_card_in_room(&self) -> bool {
        self.screen_card_in_room()
    }

    pub fn test_pip_tabs(&self) -> bool {
        self.pip_tabs()
    }

    pub fn test_screen_pip_active(&self) -> bool {
        self.screen_pip_active()
    }

    pub fn test_screen_card_stop_painted(&self) -> bool {
        self.screen_card_stop_painted()
    }

    pub fn test_pip_role(&self) -> super::PipRole {
        self.active_pip_role()
    }

    pub fn test_selected_pip_role(&self) -> super::PipRole {
        self.pip_role
    }

    pub fn test_select_pip_role(&mut self, role: super::PipRole) {
        self.pip_role = role;
    }

    pub fn test_pip_settings_open(&self) -> bool {
        self.pip_settings
    }

    pub fn test_pip_close_role(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pip_close_role(window, cx);
    }

    /// Whether the player made the window fullscreen and still owes its exit.
    pub fn test_owns_fullscreen(&self) -> bool {
        self.owns_fullscreen
    }

    pub fn test_sync_pip(&mut self, window: &mut Window) {
        self.sync_pip(window);
    }

    pub fn test_publisher_room(&self) -> i64 {
        self.publisher_room()
    }

    pub fn test_pip_restore_focus(&self) -> bool {
        self.pip_restore_focus
    }

    /// The stream intent commands the UI handed to the detached worker. A
    /// navigation must not add `watch`/`close` entries.
    pub fn test_take_stream_commands(&mut self) -> Vec<(&'static str, u64)> {
        self.handle
            .as_ref()
            .map(|handle| handle.take_stream_commands_for_tests())
            .unwrap_or_default()
    }

    pub fn test_watching_selection(&self) -> Option<u64> {
        self.watching_selection
    }

    pub fn test_pip_status(&self) -> String {
        self.pip_status()
    }

    /// The title the PiP paints for the active role. The publisher title labels
    /// a confirmed connection that the current one no longer matches.
    pub fn test_pip_title(&self) -> String {
        self.pip_title()
    }

    /// Whether the PiP paints its close control as a live action or as the
    /// status of a stop that is already in flight.
    pub fn test_pip_close_enabled(&self) -> bool {
        self.pip_close_enabled()
    }

    pub fn test_pip_settings_enabled(&self) -> bool {
        self.pip_settings_enabled()
    }

    /// The confirmed owner identity of the publication, if an intent exists.
    pub fn test_publish_access(&self) -> Option<(i64, u64, u64, Option<String>)> {
        self.publish_access.as_ref().map(|access| {
            (
                access.room,
                access.session,
                access.attempt,
                access.connection.clone(),
            )
        })
    }

    pub fn test_take_stream_mute(&mut self) -> Option<(u64, bool)> {
        self.handle
            .as_ref()
            .and_then(|handle| handle.take_stream_mute_for_tests())
    }
}
