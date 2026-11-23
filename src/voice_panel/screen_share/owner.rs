//! Ownership of a screen publication: the confirmed `Stream` permission of
//! every room, the owner identity captured at the publish start, the gate that
//! is pushed to the media worker and the one-stop latch.
//!
//! The displayed room may be a text channel, another voice channel or a DM;
//! live settings must still use the access of the room that owns the stream.
//! That access is re-read from the current confirmation on every gate push: a
//! permission or slot loss after the start must reach the worker exactly like
//! it did before the own PiP existed.

use super::*;

impl VoicePanel {
    pub fn set_stream_permission(&mut self, allowed: bool) {
        self.can_stream = allowed;
        // Remember the confirmed permission of this room, so a publication
        // started here keeps its own access after the user opens another room.
        self.stream_permissions.insert(self.room, allowed);
        // A revocation that arrives while the picker, a capture preparation or
        // a quality preflight is open must reach the worker, not wait for the
        // next Start.
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

    /// Confirmed free slot of one room. The call roster is authoritative for
    /// the room this session publishes in; a room the panel neither joined nor
    /// displays has no confirmed contradiction to report.
    pub(super) fn slot_available_in(&self, room: i64) -> bool {
        let streams = if self.call_channel != 0 && room == self.call_channel {
            self.streams.as_slice()
        } else if room == self.room {
            self.room_streams()
        } else {
            return true;
        };
        streams
            .iter()
            .filter(|publication| {
                Some(&publication.publisher_connection_id) != self.self_connection.as_ref()
            })
            .count()
            < MAX_CHANNEL_PUBLISHERS
    }

    /// The current confirmed permission of the owner. Never the snapshot taken
    /// at the start: a revocation has to cancel a preparation or a preflight and
    /// block the live settings, exactly like the displayed room did before.
    pub(super) fn publish_permission(&self) -> bool {
        self.stream_permission_for(self.publisher_room())
    }

    pub(super) fn publish_slot_available(&self) -> bool {
        self.slot_available_in(self.publisher_room())
    }

    /// Push the current gate to the worker while a picker or an attempt is
    /// open. The owner branch starts with the sent intent, not with its `Live`
    /// confirmation: navigating away during a preparation must not replace the
    /// owner gate with the displayed room's. A terminal answer of an
    /// unconfirmed start keeps that intent as well, so `Failed`/`Idle` after a
    /// sent Start cannot fall back to the displayed room either.
    pub(crate) fn sync_screen_access(&self) {
        let owned_attempt = self.publish_access.is_some() || self.share_active();
        if !(self.screen.open || self.screen.busy || owned_attempt) {
            return;
        }
        let Some(handle) = &self.handle else {
            return;
        };
        let (permission, slot_available) = if owned_attempt {
            (self.publish_permission(), self.publish_slot_available())
        } else {
            (
                self.stream_permission_for(self.room),
                self.active_publishers_in_room() < MAX_CHANNEL_PUBLISHERS,
            )
        };
        handle.screen_access(permission, slot_available);
    }

    /// Stable identity of the publish session: the picker id of its first
    /// start. It does not change when the media layer confirms the attempt, so
    /// the latch still suppresses a second stop of the same intent.
    pub(super) fn screen_stop_id(&self) -> u64 {
        self.publish_access
            .as_ref()
            .map(|access| access.session)
            .unwrap_or(self.screen.picker)
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

    /// Bind the attempt the media layer confirmed to the intent that is already
    /// open. A recovered reconnect reports the attempt again with the new
    /// connection, so the PiP stops calling a healthy stream "recovering".
    pub(super) fn confirm_publish_access(&mut self) {
        let attempt = self.screen.sharing.attempt;
        if attempt == 0 {
            return;
        }
        let connection = self.self_connection.clone();
        let Some(access) = self.publish_access.as_mut() else {
            return;
        };
        access.attempt = attempt;
        access.connection = connection;
    }
}
