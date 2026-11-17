use super::*;
use crate::history::{apply_reactions, merge_revision};

/// What a realtime event asks the controller to do next.
#[derive(Default)]
pub(super) struct RealtimeEffect {
    pub(super) resync: bool,
    pub(super) access: bool,
    pub(super) server_removed: bool,
    /// Attachment ids removed by the server; their thumbnails and viewer are
    /// released by the event pump where a `Window` is available.
    pub(super) attachments: Vec<String>,
    /// A bounded attachment tombstone rolled over and cancelled the initial
    /// history refresh; the pump restarts one fresh refresh after the batch.
    pub(super) restart_history: bool,
    /// Personal conversation list or an open direct history changed.
    pub(super) recents: bool,
    /// The pin set of the active channel changed; one list refresh after the
    /// whole batch reconciles both the popup and `pinned_ids`.
    pub(super) pins: bool,
    /// Personal messages whose reactions must be fetched without replacing
    /// the loaded history window.
    pub(super) conversation_reactions: Vec<(i64, i64)>,
    /// A reconnect, channel or member change unconfirms the recordings
    /// snapshot and any private preview.
    pub(super) recording_access: bool,
    /// One `recording.updated` was merged into the server snapshot.
    pub(super) recording_event: bool,
    /// A `voice.*` event changed the remote rosters of visible rooms.
    pub(super) voice_rooms: bool,
}

impl Voxhold {
    pub(super) fn start_realtime(
        &mut self,
        client: Client,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (subscription, desired) = tokio::sync::watch::channel(None);
        self.event_subscription = Some(subscription);
        let (tx, rx) = std::sync::mpsc::sync_channel(128);
        let overflow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.events_overflow = overflow.clone();
        self.events = Some(std::sync::Arc::new(std::sync::Mutex::new(rx)));
        self.event_task = Some(self.runtime.spawn(async move {
            let mut connected = false;
            loop {
                let sender = tx.clone();
                let overflow = overflow.clone();
                let _ = client
                    .subscribe_channels(desired.clone(), |mut event| {
                        if event.kind == "ready" && std::mem::replace(&mut connected, true) {
                            event.kind = "native.reconnected".into();
                        }
                        if sender.try_send(event).is_err() {
                            overflow.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    })
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }));
        self.schedule_events(self.realtime_generation, window, cx);
    }
    pub(super) fn schedule_events(
        &self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(150));
        cx.spawn_in(window, async move |this, cx| {
            timer.await;
            let _ = this.update_in(cx, |s, window, cx| {
                if s.realtime_generation != generation || s.events.is_none() {
                    return;
                }
                s.process_realtime_batch(window, cx);
                if s.realtime_generation == generation && s.events.is_some() {
                    s.schedule_events(generation, window, cx);
                }
            });
        })
        .detach();
    }

    /// Drain and apply one batch of queued events. The overflow flag is folded
    /// into the batch so a dropped `channels.changed` or credential event still
    /// unconfirms the recordings snapshot even when no event remains.
    pub(super) fn process_realtime_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let events: Vec<_> = self
            .events
            .as_ref()
            .map(|events| events.lock().unwrap().try_iter().collect())
            .unwrap_or_default();
        let changed = !events.is_empty();
        // A full queue may have dropped `channels.changed` or a credential
        // revocation, so the overflow itself must unconfirm the recordings
        // snapshot even when the batch is empty or the open chat is a personal
        // conversation.
        let overflow = self
            .events_overflow
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        let mut resync = overflow;
        let mut access = overflow && self.channel != 0;
        let mut server_removed = false;
        let mut removed_attachments = Vec::new();
        let mut restart_history = false;
        let mut recents = false;
        let mut pins = false;
        let mut conversation_reactions = Vec::new();
        let mut recording_access = overflow;
        let mut recording_event = false;
        let mut voice_rooms = false;
        for event in &events {
            self.forward_thread_event(event, window, cx);
            self.deliver_notification(event, window);
            let effect = self.apply_realtime_event(&event.kind, &event.data);
            resync |= effect.resync;
            access |= effect.access;
            server_removed |= effect.server_removed;
            restart_history |= effect.restart_history;
            recents |= effect.recents;
            pins |= effect.pins;
            recording_access |= effect.recording_access;
            recording_event |= effect.recording_event;
            voice_rooms |= effect.voice_rooms;
            conversation_reactions.extend(effect.conversation_reactions);
            removed_attachments.extend(effect.attachments);
        }
        #[cfg(feature = "voxhold-media")]
        if voice_rooms {
            self.sync_voice_panel(cx);
        }
        if recording_access && !server_removed {
            self.invalidate_recordings(window, cx);
        }
        if recents {
            self.refresh_recents(cx);
        }
        if pins {
            self.refresh_pins(cx);
        }
        for (conversation, message) in conversation_reactions {
            self.refresh_conversation_reaction(conversation, message, cx);
        }
        if !removed_attachments.is_empty() {
            restart_history |= self.purge_attachments(&removed_attachments, window, cx);
        }
        if server_removed {
            self.remove_current_server(window, cx);
        } else {
            if access {
                // A personal conversation keeps its history, composer
                // and selection, but the server channel ACL may still
                // have changed. Drop the confirmed search snapshot and
                // every answer started under it before asking again.
                if self.in_conversation() {
                    // The `403` recheck owns a separate slot and can
                    // still return the old channel list. Release it
                    // together with the search snapshot so its late
                    // callback fails the owner check.
                    self.channel_snapshot_request = None;
                    self.search_permissions_request = None;
                    self.search_permissions_ready = false;
                    self.sync_search_permissions(cx);
                    if self.show_search {
                        self.refresh_search_permissions(window, cx);
                    }
                    // The open dialogue keeps its history and composer, but
                    // the server ACL still has to be refreshed: leaving
                    // `voice_unconfirmed` set would hide every room until an
                    // unrelated forced reload.
                    #[cfg(feature = "voxhold-media")]
                    if self.voice_unconfirmed.contains(&self.server) {
                        if self.access_reload_in_flight {
                            self.access_refresh_again = true;
                        } else {
                            self.reload_channels(window, cx);
                        }
                    }
                } else if self.access_reload_in_flight {
                    // Access changed: hide confirmed data first, then
                    // take a new authoritative channel snapshot and
                    // history.
                    self.access_refresh_again = true;
                } else {
                    self.invalidate_access(window, cx);
                }
            }
            // A reconnect or resync of an open personal conversation
            // must not be swallowed by a server-channel access event.
            if (resync || restart_history) && !self.access_pending && !self.access_reload_in_flight
            {
                // A cancelled initial load would leave the chat empty,
                // so restart exactly one refresh after the batch.
                self.refresh_history(window, cx);
            }
        }
        if changed {
            self.sync_open_views(cx);
            // Deleting (or dropping from the window) the viewed message
            // closes the viewer before a late content answer arrives.
            let stale_viewer = self.media.viewer.as_ref().is_some_and(|viewer| {
                self.deleted_ids.contains(&viewer.key.message_id)
                    || !self
                        .messages
                        .iter()
                        .any(|row| row.id == viewer.key.message_id)
            });
            if stale_viewer {
                self.close_viewer(window);
            }
        }
        if changed || resync || access || recording_event {
            cx.notify();
        }
    }
    /// A native notification is offered only while the setting is enabled and
    /// the window is not focused. A failed tray call does not remember the key,
    /// so the next identical event can retry.
    fn deliver_notification(&mut self, event: &voxhold_api::RealtimeEvent, window: &Window) {
        let active = window.is_window_active() || self.window_active;
        let Some((key, title, body)) =
            self.notifications
                .offer(&event.kind, &event.data, &self.auth.username, active)
        else {
            return;
        };
        let hwnd = super::personal_ops::hwnd(window);
        match self.notification_tray.show(hwnd, &title, &body) {
            Ok(()) => self.notifications.shown(&key),
            Err(error) => self.notifications.failed(error),
        }
    }

    pub(super) fn message_in_scope(&self, message: &Message) -> bool {
        if self.in_conversation() {
            message.conversation_id == self.channel && message.thread_root_id.is_none()
        } else {
            message.channel_id == self.channel && message.thread_root_id.is_none()
        }
    }

    /// Thread rows live in their own timeline; only the root and its comments
    /// are forwarded there.
    pub(super) fn forward_thread_event(
        &mut self,
        event: &voxhold_api::RealtimeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = &self.thread else {
            return;
        };
        // A root deletion is a workspace operation: it must close the panel
        // and save the draft before the panel callback could re-enter the
        // workspace. Handling it inside `thread.update` would panic.
        if event.kind == "message.deleted"
            && event.data["channel_id"].as_i64() == Some(self.channel)
        {
            if let Some(id) = event.data["message_id"].as_i64() {
                if thread.read(cx).root_id() == id {
                    self.note_message_deleted(id, window, cx);
                    return;
                }
            }
        }
        thread.update(cx, |thread, cx| {
            thread.handle_event(&event.kind, &event.data, self.access_generation, cx)
        });
    }

    /// Merge one message while keeping the body revision and the separate
    /// reaction revision apart. Backend `message.updated` omits `reactions`, so
    /// replacing the object wholesale would reset them to version 0.
    pub(super) fn merge_message(&mut self, created: bool, message: Message) {
        if message.id <= 0
            || !self.message_in_scope(&message)
            || self.deleted_ids.contains(&message.id)
        {
            return;
        }
        let id = message.id;
        let position = self.messages.binary_search_by_key(&id, |row| row.id);
        match position {
            Ok(index) => merge_revision(&mut self.messages[index], message),
            Err(index) if created => {
                if self.refresh_request.is_some() {
                    self.refresh_new_ids.insert(id);
                }
                let mut message = message;
                if let Some(edit) = self.pending_edits.remove(&id) {
                    merge_revision(&mut message, edit);
                }
                if let Some(update) = self.pending_reactions.remove(&id) {
                    apply_reactions(&mut message, update);
                }
                self.messages.insert(index, message);
            }
            Err(_) => {
                if self.pending_edits.len() >= 128 && !self.pending_edits.contains_key(&id) {
                    self.pending_edits.clear();
                }
                self.pending_edits
                    .entry(id)
                    .and_modify(|row| merge_revision(row, message.clone()))
                    .or_insert(message);
            }
        }
        // An older page or a late event cannot resurrect a deleted attachment.
        let purged = &self.media.purged;
        if let Ok(index) = self.messages.binary_search_by_key(&id, |row| row.id) {
            crate::history::apply_attachment_tombstones(&mut self.messages[index], purged);
        }
    }

    /// Apply one realtime event to the active scope.    ///
    /// A stale body revision never overwrites a confirmed row, a tombstone wins
    /// over late `created`/`updated` events and access events invalidate the
    /// confirmed scope before any late HTTP answer can be applied.
    pub(super) fn apply_realtime_event(
        &mut self,
        kind: &str,
        data: &serde_json::Value,
    ) -> RealtimeEffect {
        let mut effect = RealtimeEffect::default();
        match kind {
            "channel.subscribed" | "channel.sync_required"
                if data["server_id"].as_i64() == Some(self.server)
                    && data["channel_id"].as_i64() == Some(self.channel)
                    && (kind == "channel.sync_required"
                        || data["generation"].as_u64() == Some(self.access_generation)) =>
            {
                effect.resync = true;
            }
            "ready" => {
                // A reconnect can miss a permission change; unconfirm the
                // recordings snapshot before the next list request.
                effect.recording_access = true;
            }
            "native.reconnected" => {
                effect.recording_access = true;
                if self.in_conversation() {
                    // An open direct chat missed whatever arrived while the
                    // socket was down; refresh its history and recents even
                    // when no server is selected.
                    effect.resync = true;
                    effect.recents = true;
                } else {
                    effect.access = self.server != 0;
                }
            }
            "native.subscription_forbidden"
                if data["server_id"].as_i64() == Some(self.server)
                    && data["channel_id"].as_i64() == Some(self.channel)
                    && data["generation"].as_u64() == Some(self.access_generation) =>
            {
                effect.access = true;
            }
            "channels.changed" | "channel.created" | "channel.updated" | "channel.deleted"
                if data["server_id"].as_i64() == Some(self.server) =>
            {
                effect.access = true;
                effect.recording_access = true;
                // Snapshots precede this event. Hide them until the fresh
                // channel list confirms the rooms that are still readable.
                // Without any voice surface there is nothing to hide; a later
                // room open still uses the confirmed channel list.
                #[cfg(feature = "voxhold-media")]
                if self.voice.is_some() || self.voice_channel != 0 || self.voice_room != 0 {
                    self.voice_unconfirmed.insert(self.server);
                }
            }
            "server.member_joined" | "server.member_role_updated"
                if data["server_id"].as_i64() == Some(self.server) =>
            {
                effect.recording_access = true;
                if let Ok(member) = serde_json::from_value::<Member>(data["member"].clone()) {
                    if member.user_id == self.self_user_id {
                        if let Some(server) = self
                            .servers
                            .iter_mut()
                            .find(|server| server.id == self.server)
                        {
                            server.role = member.role.clone();
                        }
                    }
                    self.members
                        .retain(|current| current.user_id != member.user_id);
                    self.members.push(member);
                    crate::members::sort_members(&mut self.members);
                }
                effect.access = true;
            }
            "server.member_removed" | "server.deleted" if data["server_id"].as_i64().is_some() => {
                let user_id = data["user_id"].as_i64();
                let server = data["server_id"].as_i64().unwrap_or_default();
                let removed = kind == "server.deleted" || user_id == Some(self.self_user_id);
                if removed {
                    self.online_by_server.remove(&server);
                }
                if server == self.server {
                    effect.server_removed = removed;
                    effect.recording_access = true;
                    self.members
                        .retain(|member| Some(member.user_id) != user_id);
                    effect.access = true;
                } else if removed {
                    self.servers.retain(|item| item.id != server);
                    self.drafts.borrow_mut().clear_server(server);
                }
            }
            "recording.updated" => {
                if let Ok(record) = serde_json::from_value::<voxhold_api::Recording>(data.clone()) {
                    if record.server_id == self.server
                        && self
                            .recordings
                            .apply_event(record, super::recordings_ops::now_secs())
                    {
                        effect.recording_event = true;
                    }
                }
            }
            #[cfg(feature = "voxhold-media")]
            "stream.snapshot" => {
                // The account socket covers every available server. Keep all
                // of them scoped by server and channel; visibility is applied
                // when a room is read.
                let mut rooms: std::collections::HashMap<
                    (i64, i64),
                    Vec<voxhold_media::controller::Publication>,
                > = std::collections::HashMap::new();
                if let Some(streams) = data["streams"].as_array() {
                    for value in streams {
                        let Ok(publication) = serde_json::from_value::<
                            voxhold_media::controller::Publication,
                        >(value.clone()) else {
                            continue;
                        };
                        if publication.server_id <= 0
                            || publication.channel_id <= 0
                            || publication.publisher_connection_id.is_empty()
                        {
                            continue;
                        }
                        rooms
                            .entry((publication.server_id, publication.channel_id))
                            .or_default()
                            .push(publication);
                    }
                }
                self.voice_streams = rooms;
                effect.voice_rooms = true;
            }
            #[cfg(feature = "voxhold-media")]
            "stream.started" | "stream.updated" => {
                let Ok(publication) =
                    serde_json::from_value::<voxhold_media::controller::Publication>(data.clone())
                else {
                    return effect;
                };
                if publication.server_id <= 0
                    || publication.channel_id <= 0
                    || publication.publisher_connection_id.is_empty()
                {
                    return effect;
                }
                let key = (publication.server_id, publication.channel_id);
                let list = self.voice_streams.entry(key).or_default();
                if let Some(existing) = list
                    .iter_mut()
                    .find(|old| old.publisher_connection_id == publication.publisher_connection_id)
                {
                    *existing = publication;
                } else {
                    list.push(publication);
                }
                effect.voice_rooms = true;
            }
            #[cfg(feature = "voxhold-media")]
            "stream.stopped" => {
                let server = data["server_id"].as_i64().unwrap_or_default();
                let channel = data["channel_id"].as_i64().unwrap_or_default();
                let publisher = data["publisher_connection_id"].as_str().unwrap_or_default();
                if server <= 0 || channel <= 0 {
                    return effect;
                }
                let key = (server, channel);
                if let Some(list) = self.voice_streams.get_mut(&key) {
                    if publisher.is_empty() {
                        list.clear();
                    } else {
                        list.retain(|old| old.publisher_connection_id != publisher);
                    }
                    if list.is_empty() {
                        self.voice_streams.remove(&key);
                    }
                }
                effect.voice_rooms = true;
            }
            #[cfg(feature = "voxhold-media")]
            "voice.snapshot" => {
                let mut rooms: std::collections::HashMap<
                    (i64, i64),
                    Vec<voxhold_media::controller::Participant>,
                > = std::collections::HashMap::new();
                if let Some(participants) = data["participants"].as_array() {
                    for value in participants {
                        let Ok(participant) = serde_json::from_value::<
                            voxhold_media::controller::Participant,
                        >(value.clone()) else {
                            continue;
                        };
                        if participant.server_id <= 0
                            || participant.channel_id <= 0
                            || participant.connection_id.is_empty()
                        {
                            continue;
                        }
                        rooms
                            .entry((participant.server_id, participant.channel_id))
                            .or_default()
                            .push(participant);
                    }
                }
                self.voice_rooms = rooms;
                effect.voice_rooms = true;
            }
            #[cfg(feature = "voxhold-media")]
            "voice.participant_joined" | "voice.state_updated" | "voice.participant_left" => {
                let Ok(participant) =
                    serde_json::from_value::<voxhold_media::controller::Participant>(data.clone())
                else {
                    return effect;
                };
                if participant.connection_id.is_empty()
                    || participant.server_id <= 0
                    || participant.channel_id <= 0
                {
                    return effect;
                }
                // A connection may move between rooms or servers: remove its
                // old copy from every cached roster before inserting it.
                for roster in self.voice_rooms.values_mut() {
                    roster.retain(|old| old.connection_id != participant.connection_id);
                }
                self.voice_rooms.retain(|_, roster| !roster.is_empty());
                if kind != "voice.participant_left" {
                    self.voice_rooms
                        .entry((participant.server_id, participant.channel_id))
                        .or_default()
                        .push(participant);
                }
                effect.voice_rooms = true;
            }
            "presence.snapshot" => {
                if let Some(servers) = data["servers"].as_array() {
                    for server in servers {
                        if let (Some(id), Some(ids)) = (
                            server["server_id"].as_i64(),
                            server["online_user_ids"].as_array(),
                        ) {
                            self.online_by_server.insert(
                                id,
                                ids.iter().filter_map(serde_json::Value::as_i64).collect(),
                            );
                        }
                    }
                }
            }
            "presence.updated" => {
                if let (Some(server), Some(id)) =
                    (data["server_id"].as_i64(), data["user_id"].as_i64())
                {
                    let online = self.online_by_server.entry(server).or_default();
                    match data["status"].as_str() {
                        Some("online") => {
                            online.insert(id);
                        }
                        Some("offline") => {
                            online.remove(&id);
                        }
                        _ => {}
                    }
                    if let Some(at) = data["last_seen_at"].as_i64() {
                        let seen = self.roster.last_seen.entry(id).or_default();
                        *seen = (*seen).max(at);
                    }
                }
            }
            "channel.read_snapshot" if !self.access_pending => {
                if data["server_id"].as_i64() == Some(self.server)
                    && data["channel_id"].as_i64() == Some(self.channel)
                {
                    let mut mine = 0;
                    if let Some(reads) = data["reads"].as_array() {
                        for value in reads {
                            if let Ok(read) =
                                serde_json::from_value::<voxhold_api::ChannelRead>(value.clone())
                            {
                                let slot = self.reads.entry(read.user_id).or_default();
                                *slot = (*slot).max(read.last_read_message_id);
                                if read.user_id == self.self_user_id {
                                    mine = mine.max(read.last_read_message_id);
                                }
                            }
                        }
                    }
                    if mine > 0 {
                        let key = (self.server, self.channel);
                        let slot = self.channel_reads.entry(key).or_default();
                        let known_before = *slot;
                        *slot = (*slot).max(mine);
                        // The snapshot arrives after the channel was opened.
                        // Place the divider at the confirmed read id once, and
                        // never move it after a read from this visit.
                        if self.unread_after == 0 && self.read_sent == 0 && known_before == 0 {
                            self.unread_after = *slot;
                        }
                    }
                }
            }
            "channel.read" if !self.access_pending => {
                if let Ok(read) = serde_json::from_value::<voxhold_api::ChannelRead>(data.clone()) {
                    if read.server_id == self.server && read.channel_id == self.channel {
                        let slot = self.reads.entry(read.user_id).or_default();
                        *slot = (*slot).max(read.last_read_message_id);
                        if read.user_id == self.self_user_id {
                            let known = self
                                .channel_reads
                                .entry((self.server, self.channel))
                                .or_default();
                            *known = (*known).max(read.last_read_message_id);
                        }
                    }
                }
            }
            "message.created" | "message.updated" if !self.access_pending => {
                if let Ok(message) = serde_json::from_value::<Message>(data.clone()) {
                    if message.channel_id == self.channel {
                        self.merge_message(kind == "message.created", message);
                        self.trim_history(false);
                    }
                }
            }
            "message.deleted" if !self.access_pending && !self.in_conversation() => {
                if data["channel_id"].as_i64() == Some(self.channel) {
                    if let Some(id) = data["message_id"].as_i64() {
                        self.deleted_ids.insert(id);
                        self.messages.retain(|m| m.id != id);
                        self.pending_edits.remove(&id);
                        self.pending_reactions.remove(&id);
                    }
                }
            }
            "conversation.message" if !self.access_pending => {
                if let Ok(message) = serde_json::from_value::<Message>(data.clone()) {
                    if self.in_conversation() && message.conversation_id == self.channel {
                        self.merge_message(true, message);
                        self.trim_history(false);
                    }
                }
                effect.recents = true;
            }
            "conversation.deleted" => {
                let matches = self.in_conversation()
                    && data["conversation_id"].as_i64() == Some(self.channel);
                if matches {
                    if let Some(id) = data["message_id"].as_i64() {
                        self.deleted_ids.insert(id);
                        self.messages.retain(|row| row.id != id);
                        self.pending_edits.remove(&id);
                        self.pending_reactions.remove(&id);
                        self.selected_messages.retain(|row| *row != id);
                    }
                }
                effect.recents = true;
            }
            "conversation.changed" => {
                let matches = self.in_conversation()
                    && data["conversation_id"].as_i64() == Some(self.channel);
                if matches {
                    effect.resync = true;
                }
                effect.recents = true;
            }
            "conversation.reactions" if !self.access_pending => {
                if self.in_conversation() && data["conversation_id"].as_i64() == Some(self.channel)
                {
                    if let Some(message_id) = data["message_id"].as_i64().filter(|id| *id > 0) {
                        // Fetch only the reacted row. A full latest-page refresh
                        // would replace the loaded older history window.
                        effect
                            .conversation_reactions
                            .push((self.channel, message_id));
                    }
                }
            }
            "conversation.notification" | "conversation.read" => {
                effect.recents = true;
            }
            "attachments.changed" if !self.access_pending => {
                if data["channel_id"].as_i64() == Some(self.channel) {
                    let ids: Vec<String> = data["attachment_ids"]
                        .as_array()
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    if ids.is_empty() {
                        // The fallback for an event without a list.
                        effect.resync = true;
                    } else {
                        let mut history_stale = false;
                        for id in &ids {
                            for message in &mut self.messages {
                                for file in &mut message.attachments {
                                    if file.id == *id {
                                        file.deleted = true;
                                    }
                                }
                            }
                            history_stale |= self.media.mark_purged(id);
                        }
                        if history_stale {
                            // A marker rolled out of the bounded set. Answers
                            // requested before this point may still describe
                            // the evicted attachment as live, so discard them.
                            effect.restart_history = self.invalidate_history_requests();
                        }
                        self.attachment_epoch = self.attachment_epoch.wrapping_add(1);
                        effect.attachments = ids;
                    }
                }
            }
            "message.pinned" | "message.unpinned" if !self.access_pending => {
                let channel = data["channel_id"]
                    .as_i64()
                    .or_else(|| data["message"]["channel_id"].as_i64());
                if channel == Some(self.channel) && !self.in_conversation() {
                    // One list refresh after the whole batch keeps the popup
                    // and `pinned_ids` in step with another client.
                    effect.pins = true;
                }
            }
            "message.reactions_changed" if !self.access_pending && !self.in_conversation() => {
                if let Ok(update) =
                    serde_json::from_value::<voxhold_api::ReactionsUpdate>(data.clone())
                {
                    if update.channel_id == self.channel
                        && !self.deleted_ids.contains(&update.message_id)
                    {
                        if let Some(message) =
                            self.messages.iter_mut().find(|m| m.id == update.message_id)
                        {
                            apply_reactions(message, update);
                        } else {
                            if self.pending_reactions.len() >= 128
                                && !self.pending_reactions.contains_key(&update.message_id)
                            {
                                self.pending_reactions.clear();
                            }
                            let entry = self
                                .pending_reactions
                                .entry(update.message_id)
                                .or_insert_with(|| update.clone());
                            if update.revision > entry.revision {
                                *entry = update;
                            }
                        }
                    }
                }
            }
            "notification.settings_updated" => {
                let server = data["server_id"].as_i64();
                let channel = data["channel_id"].as_i64();
                let mode = data["mode"].as_str().unwrap_or_default();
                if let Some(panel) = &mut self.channel_notifications {
                    if panel.server == server.unwrap_or_default()
                        && panel.channel == channel.unwrap_or_default()
                        && !mode.is_empty()
                    {
                        // Keep an unsaved local choice. The confirmed value is
                        // always replaced, and the local editor follows it only
                        // when it still matched the old confirmed value.
                        let edited = panel.mode != panel.saved;
                        panel.saved = mode.to_owned();
                        if !edited {
                            panel.mode = mode.to_owned();
                        }
                        panel.loaded = true;
                        panel.error.clear();
                        // A GET that started before this event carries an
                        // older snapshot and must not overwrite the event.
                        panel.epoch = panel.epoch.wrapping_add(1);
                    }
                }
            }
            "avatar.updated" => {
                if let Some(user) = data["user_id"].as_i64() {
                    let version = data["version"]
                        .as_str()
                        .filter(|version| !version.is_empty())
                        .map(str::to_owned);
                    if let Some(member) = self.members.iter_mut().find(|m| m.user_id == user) {
                        member.avatar_version = version;
                    }
                    self.roster.failures.remove(&user);
                    self.roster.loading.remove(&user);
                }
            }
            _ => {}
        }
        effect
    }
}
