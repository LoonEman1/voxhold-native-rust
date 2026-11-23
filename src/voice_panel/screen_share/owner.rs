//! Ownership of a screen publication: the confirmed `Stream` permission of
//! every room, the owner snapshot captured at the publish start, the gate that
//! is pushed to the media worker and the one-stop latch.
//!
//! The displayed room may be a text channel, another voice channel or a DM;
//! live settings must still use the access of the room that owns the stream.

use super::*;

impl VoicePanel {
    pub fn set_stream_permission(&mut self, allowed: bool) {
        self.can_stream = allowed;
        // Remember the confirmed permission of this room, so a publication
        // started here keeps its own access after the user opens another room.
        self.stream_permissions.insert(self.room, allowed);
        // A revocation that arrives while the picker or a capture preparation
        // is open must reach the worker, not wait for the next Start.
        self.sync_screen_access();
    }

    /// Confirmed `Stream` permission of every voice channel of this server.
    /// The PiP settings of a live publication read its owner room from here.
    pub fn set_stream_permissions(&mut self, permissions: HashMap<i64, bool>) {
        self.stream_permissions = permissions;
        if let Some(allowed) = self.stream_permissions.get(&self.room).copied() {
            self.can_stream = allowed;
        }
        self.sync_screen_access();
    }

    /// Confirmed permission of one room. The displayed room also carries the
    /// legacy single flag set by the shell before the full map arrived.
    pub(super) fn stream_permission_for(&self, room: i64) -> bool {
        if room == self.room {
            self.can_stream
        } else {
            self.stream_permissions.get(&room).copied().unwrap_or(false)
        }
    }

    /// Whether the connection captured at the publish start is still the
    /// confirmed one. A reconnect may hand the attempt to a new connection id;
    /// the media layer decides whether it survives, the UI only labels it.
    pub(crate) fn publisher_connection_matches(&self) -> bool {
        match (&self.publish_access, &self.self_connection) {
            (Some(access), Some(current)) => access
                .connection
                .as_ref()
                .is_none_or(|confirmed| confirmed == current),
            _ => true,
        }
    }

    /// The room that owns the active publication, from the confirmed request
    /// snapshot. A test may install only a `Sharing` snapshot without the
    /// request; the active call is then the only factual owner.
    pub(crate) fn publisher_room(&self) -> i64 {
        if let Some(access) = &self.publish_access {
            if access.room != 0 {
                return access.room;
            }
        }
        if self.screen.owner_room != 0 {
            return self.screen.owner_room;
        }
        self.call_channel
    }

    pub(super) fn publish_permission(&self) -> bool {
        self.publish_access
            .as_ref()
            .map(|access| access.can_stream)
            .unwrap_or_else(|| self.stream_permission_for(self.publisher_room()))
    }

    pub(super) fn publish_slot_available(&self) -> bool {
        self.publish_access
            .as_ref()
            .map(|access| access.slot_available)
            .unwrap_or_else(|| self.active_publishers_in_room() < MAX_CHANNEL_PUBLISHERS)
    }

    /// Push the current gate to the worker while a picker or an attempt is
    /// open. A live attempt uses the confirmed access of its owner room; only a
    /// start uses the displayed room. The session re-checks it before it
    /// accepts a prepared engine, so a permission or slot loss during the
    /// capture start cannot be missed.
    pub(crate) fn sync_screen_access(&self) {
        if !(self.screen.open || self.screen.busy || self.share_active()) {
            return;
        }
        let Some(handle) = &self.handle else {
            return;
        };
        let (permission, slot_available) = if self.share_active() || self.screen.restart {
            (self.publish_permission(), self.publish_slot_available())
        } else {
            (
                self.stream_permission_for(self.room),
                self.active_publishers_in_room() < MAX_CHANNEL_PUBLISHERS,
            )
        };
        handle.screen_access(permission, slot_available);
    }

    /// The one id that may own a stop intent: the confirmed attempt, or the
    /// picker id while the first publish is still unconfirmed.
    pub(super) fn screen_stop_id(&self) -> u64 {
        if self.screen.sharing.attempt != 0 {
            self.screen.sharing.attempt
        } else {
            self.screen.picker
        }
    }

    /// Latch one stop per attempt. Returns whether the command must be sent.
    pub(super) fn latch_screen_stop(&mut self) -> bool {
        if self.screen.sharing.phase == Phase::Stopping {
            return false;
        }
        let id = self.screen_stop_id();
        if self.screen.stop_sent == Some(id) {
            return false;
        }
        self.screen.stop_sent = Some(id);
        true
    }
}
