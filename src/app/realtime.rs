use super::*;
use crate::history::{apply_reactions, merge_revision};

/// What a realtime event asks the controller to do next.
#[derive(Default)]
pub(super) struct RealtimeEffect {
    pub(super) resync: bool,
    pub(super) access: bool,
    pub(super) server_removed: bool,
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
                let events: Vec<_> = s
                    .events
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .try_iter()
                    .collect();
                let changed = !events.is_empty();
                let mut resync = s
                    .events_overflow
                    .swap(false, std::sync::atomic::Ordering::Relaxed);
                let mut access = resync && s.channel != 0;
                let mut server_removed = false;
                for event in events {
                    let effect = s.apply_realtime_event(&event.kind, &event.data);
                    resync |= effect.resync;
                    access |= effect.access;
                    server_removed |= effect.server_removed;
                }
                if server_removed {
                    s.remove_current_server(window, cx);
                } else if access {
                    // Access changed: hide confirmed data first, then take a
                    // new authoritative channel snapshot and history.
                    if s.access_reload_in_flight {
                        s.access_refresh_again = true;
                    } else {
                        s.invalidate_access(window, cx);
                    }
                } else if resync {
                    s.refresh_history(window, cx);
                }
                if changed {
                    s.sync_tools_message(cx);
                }
                if changed || resync || access {
                    cx.notify();
                }
                if s.realtime_generation == generation && s.events.is_some() {
                    s.schedule_events(generation, window, cx);
                }
            });
        })
        .detach();
    }
    /// Merge one message while keeping the body revision and the separate
    /// reaction revision apart. Backend `message.updated` omits `reactions`, so
    /// replacing the object wholesale would reset them to version 0.
    pub(super) fn merge_message(&mut self, created: bool, message: Message) {
        if message.id <= 0
            || message.channel_id != self.channel
            || message.thread_root_id.is_some()
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
    }

    /// Apply one realtime event to the active scope.
    ///
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
            "native.reconnected" => effect.access = self.server != 0,
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
            }
            "server.member_joined" | "server.member_role_updated"
                if data["server_id"].as_i64() == Some(self.server) =>
            {
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
                    self.members
                        .retain(|member| Some(member.user_id) != user_id);
                    effect.access = true;
                } else if removed {
                    self.servers.retain(|item| item.id != server);
                    self.drafts.retain(|(id, _), _| *id != server);
                }
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
            "message.deleted" if !self.access_pending => {
                if data["channel_id"].as_i64() == Some(self.channel) {
                    if let Some(id) = data["message_id"].as_i64() {
                        self.deleted_ids.insert(id);
                        self.messages.retain(|m| m.id != id);
                        self.pending_edits.remove(&id);
                        self.pending_reactions.remove(&id);
                    }
                }
            }
            "message.reactions_changed" if !self.access_pending => {
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
