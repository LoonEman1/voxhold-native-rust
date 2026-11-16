use super::*;

impl Voxhold {
    pub(super) fn submit_instance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.instance.read(cx).value().to_string();
        if self.auth.submit_instance(&raw).is_some() {
            self.username
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Back to the address step; keeps the confirmed origin.
    #[allow(dead_code)]
    pub(super) fn back_to_instance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth.busy {
            return;
        }
        self.auth.screen = auth::AuthScreen::Instance;
        self.auth.status.clear();
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.instance.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn submit_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let user = self.username.read(cx).value().to_string();
        let pass = self.password.read(cx).value().to_string();
        let Some(request) = self.auth.submit_login(&user, &pass) else {
            cx.notify();
            return;
        };
        self.error.clear();
        cx.notify();
        let attempt = request.attempt;
        let origin = request.origin.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                let mut c = Client::new(&origin).map_err(|e| e.to_string())?;
                c.login(&request.username, &request.password)
                    .await
                    .map_err(|e| auth::AuthState::describe_error(&e))?;
                let servers = c
                    .servers()
                    .await
                    .map_err(|e| auth::AuthState::describe_error(&e))?;
                let self_user_id = match c.profile().await {
                    Ok(profile) => profile.user_id,
                    Err(error) => {
                        // Go retries the profile once before actions that need a
                        // confirmed account id.
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        c.profile()
                            .await
                            .map(|profile| profile.user_id)
                            .map_err(|_| auth::AuthState::describe_error(&error))?
                    }
                };
                Ok::<_, String>((c, servers, self_user_id))
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                let outcome = result.as_ref().map(|_| ()).map_err(|e| e.clone());
                if !s.auth.finish_login(attempt, &outcome) {
                    cx.notify();
                    return;
                }
                let Ok((c, servers, self_user_id)) = result else {
                    return;
                };
                s.password
                    .update(cx, |input, cx| input.set_value("", window, cx));
                let runtime = s.runtime.clone();
                let attachment_client = c.clone();
                let drafts = s.drafts.clone();
                let attachments = cx.new(|cx| {
                    attachments::Attachments::new(attachment_client, runtime, drafts, window, cx)
                });
                // The composer's send button and file row depend on the draft
                // owned by this child entity; a notify keeps both in step.
                cx.observe_in(&attachments, window, |_s, _attachments, _window, cx| {
                    cx.notify();
                })
                .detach();
                s.attachments = Some(attachments);
                s.personal = Some(personal_settings::PersonalSettings::new(window, cx));
                s.server_settings = Some(server_settings::ServerSettings::new(window, cx));
                s.channel_settings = Some(channel_settings::ChannelSettings::new(window, cx));
                s.channel_notifications = Some(channel_settings::ChannelNotifications::new());
                s.inbox = Some(cx.new(|_| inbox::Inbox::new()));
                let inbox = s.inbox.clone().expect("inbox");
                let weak = cx.entity().downgrade();
                inbox.update(cx, |inbox, _| {
                    inbox.set_select_handler(Box::new(move |id, window, cx| {
                        if let Some(view) = weak.upgrade() {
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
                });
                let weak = cx.entity().downgrade();
                inbox.update(cx, |inbox, _| {
                    inbox.set_refresh_handler(Box::new(move |_window, cx| {
                        if let Some(view) = weak.upgrade() {
                            view.update(cx, |view, cx| view.refresh_recents(cx));
                        }
                    }));
                });
                s.self_user_id = self_user_id;
                s.active_profile_save = None;
                s.notification_saves.clear();
                s.client = Some(c.clone());
                s.start_realtime(c, window, cx);
                s.servers = servers;
                s.personal = Some(personal_settings::PersonalSettings::new(window, cx));
                s.server_settings = Some(server_settings::ServerSettings::new(window, cx));
                s.channel_settings = Some(channel_settings::ChannelSettings::new(window, cx));
                s.channel_notifications = Some(channel_settings::ChannelNotifications::new());
                s.auth.status = "Choose a server".into();
                // Go selects the first server after login; an empty list leaves
                // the workspace on its empty state. This also gives narrow
                // layouts a working channel list even without the rail.
                s.shell_initialized = false;
                let first_server = s.servers.first().map(|server| server.id);
                if let Some(id) = first_server {
                    s.select_server(id, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn logout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_realtime();
        for (_, pending) in self.pending_sends.drain() {
            pending.abort.abort();
        }
        self.session_generation = self.session_generation.wrapping_add(1);
        self.active_profile_save = None;
        self.notification_saves.clear();
        #[cfg(feature = "voxhold-media")]
        if let Some(voice) = self.voice.take() {
            voice.update(cx, |voice, cx| voice.leave_in_window(window, cx));
        }
        #[cfg(feature = "voxhold-media")]
        {
            self.voice_channel = 0;
        }
        self.recordings_preview_tasks.clear();
        self.recordings_dialog = None;
        self.recordings_return_focus = None;
        self.recordings_voice_entry = false;
        self.recording_access_epoch = self.recording_access_epoch.wrapping_add(1);
        self.recordings.reset();
        self.recordings
            .set_access_epoch(self.recording_access_epoch);
        self.client = None;
        self.servers.clear();
        self.channels.clear();
        self.members.clear();
        self.messages.clear();
        self.history_keys.clear();
        self.deleted_ids.clear();
        self.pending_anchor = None;
        self.history_list.reset(0);
        self.before = None;
        self.has_more = false;
        self.history_has_newer = false;
        self.self_user_id = 0;
        self.server = 0;
        self.channel = 0;
        self.reset_roster(window);
        self.error.clear();
        self.composer_error.clear();
        self.busy = false;
        self.clear_media(window);
        self.drafts.borrow_mut().clear_all();
        self.generation = self.generation.wrapping_add(1);
        self.access_generation = self.access_generation.wrapping_add(1);
        self.request_seq = self.request_seq.wrapping_add(1);
        self.access_pending = false;
        self.access_reload_in_flight = false;
        self.access_refresh_again = false;
        self.refresh_request = None;
        self.refresh_again = false;
        self.refresh_new_ids.clear();
        self.pending_edits.clear();
        self.pending_reactions.clear();
        self.older_request = 0;
        self.loading_older = false;
        self.older_error.clear();
        self.older_retry_at = None;
        self.older_scheduled = false;
        self.read_scheduled = false;
        self.read_pending = false;
        self.read_retry_at = None;
        self.read_sent = 0;
        self.reads.clear();
        self.channel_reads.clear();
        self.unread_after = 0;
        self.reaction_requests.clear();
        self.conversation_reaction_requests.clear();
        self.search_permissions_request = None;
        self.search_permissions_ready = false;
        self.recent_reactions.borrow_mut().clear();
        self.scrollbar_drag = None;
        self.context_request = None;
        self.pending_jump = None;
        self.history_reset_pending = false;
        self.hovered_message = 0;
        self.hovered_action = None;
        self.message_menu = None;
        self.forward = None;
        self.editing = None;
        self.confirm_delete = None;
        self.action_requests.clear();
        self.action_error = None;
        self.action_notice.clear();
        self.selected_messages.clear();
        self.pinned_ids.clear();
        self.pinned_request = None;
        self.highlighted_message = None;
        self.thread = None;
        self.show_thread = false;
        self.conversation = None;
        self.conversations.clear();
        self.recents_request = None;
        self.recents_again = false;
        self.channel_snapshot_request = None;
        self.attachments = None;
        self.personal = None;
        self.server_settings = None;
        self.channel_settings = None;
        self.channel_notifications = None;
        self.active_settings = None;
        self.settings_return_focus = None;
        self.settings_ring.clear();
        self.inbox = None;
        self.search = None;
        self.show_inbox = false;
        self.show_search = false;
        self.show_servers = false;
        self.auth = {
            let origin = self.auth.origin.clone();
            auth::AuthState::with_prefill(&origin)
        };
        self.auth.screen = auth::AuthScreen::Login;
        self.username
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.instance.update(cx, |input, cx| {
            input.set_value(self.auth.origin.clone(), window, cx);
        });
        self.shell_initialized = false;
        self.initial_focus_done = false;
        cx.notify();
    }
}
