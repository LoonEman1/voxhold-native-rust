use super::*;

impl Voxhold {
    pub(super) fn select_server(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.client.clone() else { return };
        let previous = self.chat_key();
        if previous.channel != 0 {
            let text = self.composer.read(cx).value().to_string();
            self.drafts.borrow_mut().set_text(previous, &text);
        }
        self.conversation = None;
        self.close_thread(window, cx);
        self.show_inbox = false;
        self.message_menu = None;
        self.member_menu = None;
        self.forward = None;
        self.editing = None;
        self.confirm_delete = None;
        self.action_requests.clear();
        self.selected_messages.clear();
        self.highlighted_message = None;
        self.hovered_action = None;
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        #[cfg(feature = "voxhold-media")]
        if self.server != id {
            self.abandon_voice(window, cx);
        }
        self.server = id;
        self.reset_recordings(window, cx);
        self.composer_error.clear();
        self.clear_media(window);
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |a, cx| a.set_scope(None, false, cx));
        }
        self.close_stale_settings(id, 0, window, cx);
        self.channel = 0;
        let search_client = c.clone();
        let runtime = self.runtime.clone();
        let search = cx.new(|cx| search::SearchPanel::new(search_client, runtime, window, cx));
        let weak_view = cx.entity().downgrade();
        search.update(cx, |panel, _| {
            let weak_view = weak_view.clone();
            panel.set_select_handler(Box::new(move |channel, message, window, cx| {
                // The click runs inside `SearchPanel::update`. Navigation must
                // wait until that entity is off the stack, otherwise
                // `select_channel` re-enters the same panel and GPUI panics.
                let Some(view) = weak_view.upgrade() else {
                    return;
                };
                let scope = view.read(cx).search_jump_scope();
                let weak_view = weak_view.clone();
                window.defer(cx, move |window, cx| {
                    let Some(view) = weak_view.upgrade() else {
                        return;
                    };
                    view.update(cx, |view, cx| {
                        if view.search_jump_scope() != scope {
                            return;
                        }
                        view.open_search_hit(channel, message, window, cx);
                    });
                });
            }));
        });
        let weak_pins = cx.entity().downgrade();
        search.update(cx, |panel, _| {
            panel.set_pins_handler(Box::new(move |pins, cx| {
                if let Some(view) = weak_pins.upgrade() {
                    view.update(cx, |view, cx| {
                        view.pinned_ids = pins.iter().map(|message| message.id).collect();
                        cx.notify();
                    });
                }
            }));
        });
        self.search = Some(search);
        if self.inbox.is_none() {
            let inbox = cx.new(|_| inbox::Inbox::new());
            let weak_inbox = cx.entity().downgrade();
            let weak_refresh = weak_inbox.clone();
            inbox.update(cx, |inbox, _| {
                inbox.set_select_handler(Box::new(move |id, window, cx| {
                    if let Some(view) = weak_inbox.upgrade() {
                        view.update(cx, |view, cx| {
                            if let Some(conversation) = view
                                .conversations
                                .iter()
                                .find(|conversation| conversation.id == id)
                                .cloned()
                            {
                                view.select_conversation(conversation, window, cx);
                            }
                        });
                    }
                }));
                inbox.set_refresh_handler(Box::new(move |_window, cx| {
                    if let Some(view) = weak_refresh.upgrade() {
                        view.update(cx, |view, cx| view.refresh_recents(cx));
                    }
                }));
            });
            self.inbox = Some(inbox);
        }
        if self.personal.is_none() {
            self.personal = Some(personal_settings::PersonalSettings::new(window, cx));
        }
        if self.server_settings.is_none() {
            self.server_settings = Some(server_settings::ServerSettings::new(window, cx));
        }
        if self.channel_settings.is_none() {
            self.channel_settings = Some(channel_settings::ChannelSettings::new(window, cx));
        }
        if self.channel_notifications.is_none() {
            self.channel_notifications = Some(channel_settings::ChannelNotifications::new());
        }
        self.channels.clear();
        self.messages.clear();
        self.before = None;
        self.has_more = false;
        self.history_has_newer = false;
        self.loading_older = false;
        self.members.clear();
        self.reset_roster(window);
        self.reads.clear();
        self.read_sent = 0;
        self.read_pending = false;
        self.read_scheduled = false;
        self.older_scheduled = false;
        self.read_retry_at = None;
        self.busy = false;
        self.generation += 1;
        self.access_generation = self.access_generation.wrapping_add(1);
        self.access_pending = false;
        self.access_reload_in_flight = false;
        self.access_refresh_again = false;
        self.refresh_request = None;
        self.refresh_again = false;
        self.channel_snapshot_request = None;
        self.conversation_reaction_requests.clear();
        self.search_permissions_request = None;
        self.search_permissions_ready = false;
        self.pending_edits.clear();
        self.pending_reactions.clear();
        self.request_seq = self.request_seq.wrapping_add(1);
        self.older_request = 0;
        self.older_error.clear();
        self.older_retry_at = None;
        self.deleted_ids.clear();
        self.pending_anchor = None;
        self.history_list.reset(0);
        self.history_keys.clear();
        let generation = self.generation;
        self.error.clear();
        self.update_channel_subscription(false);
        self.refresh_recents(cx);
        if id != 0 {
            self.refresh_recordings(0, window, cx);
        }
        if id == 0 {
            cx.notify();
            return;
        }
        cx.notify();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async {
                let channels = c.channels(id).await.map_err(|e| e.to_string())?;
                let members = c.members(id).await.map_err(|e| e.to_string())?;
                Ok::<_, String>((channels, members))
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation || s.access_pending {
                        return;
                    }
                    match result {
                        Ok((channels, mut members)) => {
                            crate::members::sort_members(&mut members);
                            s.channels = channels;
                            s.members = members;
                            s.search_permissions_ready = true;
                            s.sync_search_permissions(cx);
                        }
                        Err(e) => s.error = e,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
    pub(super) fn select_channel(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.access_pending {
            return;
        }
        if self.client.is_none() {
            return;
        }
        let previous_channel = self.channel;
        let previous = self.chat_key();
        if previous.channel != 0 {
            let text = self.composer.read(cx).value().to_string();
            self.drafts.borrow_mut().set_text(previous, &text);
        }
        self.conversation = None;
        self.close_thread(window, cx);
        self.message_menu = None;
        self.member_menu = None;
        self.forward = None;
        self.editing = None;
        self.confirm_delete = None;
        self.action_requests.clear();
        self.selected_messages.clear();
        self.highlighted_message = None;
        self.hovered_action = None;
        let draft = self
            .drafts
            .borrow()
            .text(draft::ChatKey::channel(self.server, id));
        self.composer
            .update(cx, |input, cx| input.set_value(draft, window, cx));
        self.channel = id;
        self.composer_error.clear();
        self.close_viewer(window);
        self.preview_wakeup = None;
        self.preview_wake_at = None;
        let writable = self.channels.iter().any(|ch| ch.id == id && ch.writable());
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |a, cx| {
                a.set_scope(Some(draft::ChatKey::channel(self.server, id)), writable, cx)
            });
        }
        self.reads.clear();
        self.read_sent = 0;
        self.read_pending = false;
        self.read_retry_at = None;
        self.unread_after = self
            .channel_reads
            .get(&(self.server, id))
            .copied()
            .unwrap_or(0);
        self.reaction_requests.clear();
        self.conversation_reaction_requests.clear();
        self.channel_snapshot_request = None;
        self.search_permissions_request = None;
        self.context_request = None;
        self.pending_jump = None;
        self.history_reset_pending = false;
        self.hovered_message = 0;
        if id != previous_channel {
            self.deleted_ids.clear();
        }
        self.pending_anchor = None;
        self.access_generation = self.access_generation.wrapping_add(1);
        self.access_pending = false;
        self.access_reload_in_flight = false;
        self.access_refresh_again = false;
        self.refresh_request = None;
        self.refresh_again = false;
        self.pending_edits.clear();
        self.pending_reactions.clear();
        self.request_seq = self.request_seq.wrapping_add(1);
        self.older_request = 0;
        self.older_error.clear();
        self.older_retry_at = None;
        self.loading_older = false;
        self.older_scheduled = false;
        self.read_scheduled = false;
        self.busy = false;
        self.roster.reset_channel();
        self.roster.loading.clear();
        self.show_search = false;
        self.pinned_ids.clear();
        self.show_channels = false;
        self.close_stale_settings(self.server, id, window, cx);
        #[cfg(feature = "voxhold-media")]
        if let Some(client) = self.client.clone() {
            // A voice channel opens its room even before joining; the call in
            // another room stays alive and moves to the sidebar dock.
            if self
                .channels
                .iter()
                .any(|channel| channel.id == id && channel.kind == "voice")
            {
                if let Some(panel) = &self.voice {
                    let panel = panel.read(cx);
                    self.voice_channel = panel.call_channel();
                }
                self.open_voice_room(id, window, cx);
                let _ = client;
            }
        }
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| panel.set_scope(self.server, id, cx));
        }
        // `set_scope` drops the popup's confirmed permissions. Reinstall the
        // already-confirmed snapshot before the pin refresh, otherwise that
        // refresh exits and the row menu keeps showing "Закрепить".
        self.sync_search_permissions(cx);
        self.messages.clear();
        self.history_keys.clear();
        let kind = self
            .channels
            .iter()
            .find(|channel| channel.id == self.channel)
            .map(|channel| channel.kind.clone())
            .unwrap_or_default();
        let alignment = if kind == "news" {
            ListAlignment::Top
        } else {
            ListAlignment::Bottom
        };
        self.history_list = Self::new_history_list(alignment, cx);
        self.scrollbar_drag = None;
        self.before = None;
        self.has_more = false;
        self.history_has_newer = false;
        self.loading_older = false;
        self.generation += 1;
        self.error.clear();
        let has_history = self
            .channels
            .iter()
            .any(|ch| ch.id == id && ch.has_history());
        self.update_channel_subscription(has_history);
        self.load_channel_members(cx);
        if has_history {
            self.refresh_history(window, cx);
            if self.search_permissions_ready {
                self.refresh_pins(cx);
            } else if self.search.is_some() {
                // Returning from a personal conversation lost the confirmed
                // snapshot. Read it now; its callback installs the panel
                // permissions and then refreshes the pins.
                self.refresh_search_permissions(window, cx);
            }
        }
        cx.notify();
    }
    pub(super) fn update_channel_subscription(&self, enabled: bool) {
        if let Some(sender) = &self.event_subscription {
            sender.send_if_modified(|current| {
                let next = enabled.then_some(ChannelSubscription {
                    server_id: self.server,
                    channel_id: self.channel,
                    generation: self.access_generation,
                });
                if *current == next {
                    return false;
                }
                *current = next;
                true
            });
        }
    }

    pub(super) fn invalidate_access(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.server == 0 || self.access_reload_in_flight || self.in_conversation() {
            return;
        }
        self.access_pending = true;
        self.access_reload_in_flight = false;
        self.access_refresh_again = false;
        self.error.clear();
        self.access_generation = self.access_generation.wrapping_add(1);
        self.refresh_request = None;
        self.refresh_again = false;
        self.channel_snapshot_request = None;
        self.conversation_reaction_requests.clear();
        self.search_permissions_request = None;
        self.search_permissions_ready = false;
        self.refresh_new_ids.clear();
        self.pending_edits.clear();
        self.pending_reactions.clear();
        self.messages.clear();
        self.history_keys.clear();
        self.history_list.reset(0);
        self.before = None;
        self.has_more = false;
        self.history_has_newer = false;
        self.loading_older = false;
        self.pending_anchor = None;
        self.older_error.clear();
        self.older_retry_at = None;
        self.older_request = 0;
        self.older_scheduled = false;
        self.read_scheduled = false;
        self.read_pending = false;
        self.read_retry_at = None;
        self.read_sent = 0;
        self.reads.clear();
        self.unread_after = self
            .channel_reads
            .get(&(self.server, self.channel))
            .copied()
            .unwrap_or(0);
        self.reaction_requests.clear();
        self.context_request = None;
        self.pending_jump = None;
        self.history_reset_pending = false;
        self.hovered_message = 0;
        self.busy = false;
        self.composer_error.clear();
        #[cfg(feature = "voxhold-media")]
        {
            self.voice_rooms.clear();
            self.voice_streams.clear();
        }
        self.clear_media(window);
        self.roster.reset_channel();
        self.roster.loading.clear();
        for (_, (image, _)) in self.roster.avatars.drain() {
            let _ = window.drop_image(image);
        }
        self.roster.failures.clear();
        self.close_thread(window, cx);
        self.show_search = false;
        self.pinned_ids.clear();
        self.pinned_request = None;
        self.message_menu = None;
        self.forward = None;
        self.editing = None;
        self.confirm_delete = None;
        self.action_requests.clear();
        self.selected_messages.clear();
        self.highlighted_message = None;
        self.hovered_action = None;
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |a, cx| a.set_scope(None, false, cx));
        }
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| {
                panel.set_scope(0, 0, cx);
                panel.set_permissions(Default::default(), false, cx);
            });
        }
        self.reload_channels(window, cx);
        cx.notify();
    }

    pub(super) fn remove_current_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.servers.retain(|server| server.id != self.server);
        self.drafts.borrow_mut().clear_server(self.server);
        let next = self.servers.first().map_or(0, |server| server.id);
        self.channel = 0;
        self.select_server(next, window, cx);
    }

    pub(super) fn reload_channels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.access_reload_in_flight = true;
        let (server, generation, access) = (self.server, self.generation, self.access_generation);
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(async {
                let channels = client.channels(server).await?;
                let members = client.members(server).await;
                Ok::<_, voxhold_api::Error>((channels, members))
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.generation != generation || s.server != server || s.access_generation != access
                {
                    return;
                }
                s.access_reload_in_flight = false;
                if std::mem::take(&mut s.access_refresh_again) {
                    s.invalidate_access(window, cx);
                    return;
                }
                match result {
                    Ok((channels, members)) => {
                        s.channels = channels;
                        // A notification form follows the account, not the
                        // administration role, but it still needs a readable
                        // text/news channel. Revoking access closes it.
                        s.close_stale_settings(server, s.channel, window, cx);
                        let channel_missing = s
                            .channel_settings
                            .as_ref()
                            .filter(|_| s.active_settings == Some(SettingsKind::Channel))
                            .is_some_and(|panel| {
                                !s.channels.iter().any(|channel| channel.id == panel.channel)
                            });
                        if channel_missing {
                            s.close_settings(false, window, cx);
                        }
                        s.search_permissions_ready = true;
                        s.sync_search_permissions(cx);
                        if let Ok(mut members) = members {
                            crate::members::sort_members(&mut members);
                            s.members = members;
                        }
                        if s.in_conversation() {
                            s.access_pending = false;
                            s.error.clear();
                            s.refresh_recents(cx);
                            cx.notify();
                            return;
                        }
                        let mut close_settings = false;
                        if let Some(server) = s.servers.iter_mut().find(|item| item.id == server) {
                            if let Some(member) = s
                                .members
                                .iter()
                                .find(|member| member.user_id == s.self_user_id)
                            {
                                server.role = member.role.clone();
                            }
                            let role = crate::settings::Role::parse(&server.role);
                            close_settings = (s.active_settings == Some(SettingsKind::Server)
                                && !role.can_manage_server())
                                || (s.active_settings == Some(SettingsKind::Channel)
                                    && !role.can_manage_channels());
                        }
                        if close_settings {
                            s.close_settings(false, window, cx);
                        }
                        let channel = s
                            .channels
                            .iter()
                            .find(|c| c.id == s.channel && c.readable() && !c.e2ee_required)
                            .map(|c| c.id);
                        #[cfg(feature = "voxhold-media")]
                        {
                            s.sync_voice_panel(cx);
                            s.refresh_voice_call(window, cx);
                        }
                        s.access_pending = false;
                        s.error.clear();
                        let channel = channel.unwrap_or_default();
                        if channel != 0 {
                            s.select_channel(channel, window, cx);
                        } else {
                            if s.channel != 0 {
                                let text = s.composer.read(cx).value().to_string();
                                s.drafts
                                    .borrow_mut()
                                    .set_text(draft::ChatKey::channel(s.server, s.channel), &text);
                            }
                            s.channel = 0;
                            s.deleted_ids.clear();
                            s.composer
                                .update(cx, |input, cx| input.set_value("", window, cx));
                            s.update_channel_subscription(false);
                        }
                    }
                    Err(voxhold_api::Error::Http(403 | 404)) => {
                        s.remove_current_server(window, cx);
                    }
                    Err(error) => s.error = error.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn set_members_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.show_members = open;
        if open {
            self.load_channel_members(cx);
        } else {
            self.roster.loading.clear();
        }
        self.shell.members_open = open;
        self.save_shell();
        cx.notify();
    }
}
