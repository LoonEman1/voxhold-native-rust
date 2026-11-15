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
            if let Some(voice) = self.voice.take() {
                voice.update(cx, |voice, cx| voice.leave_in_window(window, cx));
            }
            self.voice_channel = 0;
        }
        self.server = id;
        self.composer_error.clear();
        self.clear_media(window);
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |a, cx| a.set_scope(None, false, cx));
        }
        self.show_server_settings = false;
        let server_info = self.servers.iter().find(|s| s.id == id);
        let name = server_info.map(|s| s.name.clone()).unwrap_or_default();
        let owner = server_info.is_some_and(|s| s.role == "owner");
        if let Some(settings) = &self.server_settings {
            settings.update(cx, |settings, cx| {
                settings.set_scope(id, &name, owner, window, cx)
            });
        }
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
        if self.settings.is_none() {
            let client = c.clone();
            let runtime = self.runtime.clone();
            let settings =
                cx.new(|cx| personal_settings::PersonalSettings::new(client, runtime, window, cx));
            self.settings = Some(settings);
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
                        Ok((channels, members)) => {
                            s.channels = channels;
                            s.members = members;
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
        self.show_channel_settings = false;
        self.show_channels = false;
        let role = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .map(|s| s.role.as_str())
            .unwrap_or("");
        let allowed = role == "owner" || role == "admin";
        let kind = self
            .channels
            .iter()
            .find(|ch| ch.id == id)
            .map(|ch| ch.kind.clone())
            .unwrap_or_default();
        if let Some(settings) = &self.channel_settings {
            settings.update(cx, |settings, cx| {
                settings.set_scope(self.server, id, &kind, allowed, cx)
            });
        }
        #[cfg(feature = "voxhold-media")]
        if let Some(client) = self.client.clone().filter(|_| {
            self.voice_channel != id
                && self.channels.iter().any(|ch| {
                    ch.id == id && ch.kind == "voice" && ch.readable() && !ch.e2ee_required
                })
        }) {
            if let Some(voice) = self.voice.take() {
                voice.update(cx, |voice, cx| voice.leave_in_window(window, cx));
            }
            let server = self.server;
            self.voice =
                Some(cx.new(|cx| voice_panel::VoicePanel::new(client, server, id, window, cx)));
            self.voice_channel = id;
        }
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| panel.set_scope(self.server, id, cx));
        }
        self.messages.clear();
        self.history_keys.clear();
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
            self.refresh_pins(cx);
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
        self.show_channel_settings = false;
        self.show_server_settings = false;
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |a, cx| a.set_scope(None, false, cx));
        }
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| {
                panel.set_scope(0, 0, cx);
                panel.set_allowed_channels(Default::default(), cx);
            });
        }
        if let Some(settings) = &self.channel_settings {
            settings.update(cx, |panel, cx| panel.set_scope(0, 0, "", false, cx));
        }
        if let Some(settings) = &self.server_settings {
            settings.update(cx, |panel, cx| panel.set_scope(0, "", false, window, cx));
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
                        s.sync_search_permissions(cx);
                        if let Ok(members) = members {
                            s.members = members;
                        }
                        if s.in_conversation() {
                            s.access_pending = false;
                            s.error.clear();
                            s.refresh_recents(cx);
                            cx.notify();
                            return;
                        }
                        if let Some(server) = s.servers.iter_mut().find(|item| item.id == server) {
                            if let Some(member) = s
                                .members
                                .iter()
                                .find(|member| member.user_id == s.self_user_id)
                            {
                                server.role = member.role.clone();
                            }
                            if let Some(settings) = &s.server_settings {
                                settings.update(cx, |panel, cx| {
                                    panel.set_scope(
                                        server.id,
                                        &server.name,
                                        server.role == "owner",
                                        window,
                                        cx,
                                    )
                                });
                            }
                        }
                        let channel = s
                            .channels
                            .iter()
                            .find(|c| c.id == s.channel && c.readable() && !c.e2ee_required)
                            .map(|c| c.id);
                        #[cfg(feature = "voxhold-media")]
                        if !s.channels.iter().any(|c| {
                            c.id == s.voice_channel
                                && c.kind == "voice"
                                && c.readable()
                                && !c.e2ee_required
                        }) {
                            if let Some(voice) = s.voice.take() {
                                voice.update(cx, |voice, cx| voice.leave_in_window(window, cx));
                            }
                            s.voice_channel = 0;
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
