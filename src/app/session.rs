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
                let runtime = s.runtime.clone();
                let settings_client = c.clone();
                s.settings = Some(cx.new(|cx| {
                    personal_settings::PersonalSettings::new(settings_client, runtime, window, cx)
                }));
                let runtime = s.runtime.clone();
                let server_admin_client = c.clone();
                s.server_settings = Some(cx.new(|cx| {
                    server_settings::ServerSettings::new(
                        server_admin_client,
                        runtime.clone(),
                        window,
                        cx,
                    )
                }));
                let admin_client = c.clone();
                s.channel_settings = Some(cx.new(|cx| {
                    channel_settings::ChannelSettings::new(admin_client, runtime, window, cx)
                }));
                let runtime = s.runtime.clone();
                let inbox_client = c.clone();
                s.inbox = Some(cx.new(|cx| inbox::Inbox::new(inbox_client, runtime, window, cx)));
                s.self_user_id = self_user_id;
                s.client = Some(c.clone());
                s.start_realtime(c, window, cx);
                s.servers = servers;
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
        #[cfg(feature = "voxhold-media")]
        if let Some(voice) = self.voice.take() {
            voice.update(cx, |voice, cx| voice.leave_in_window(window, cx));
        }
        #[cfg(feature = "voxhold-media")]
        {
            self.voice_channel = 0;
        }
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
        self.sending = false;
        self.busy = false;
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
        self.recent_reactions.borrow_mut().clear();
        self.scrollbar_drag = None;
        self.context_request = None;
        self.pending_jump = None;
        self.history_reset_pending = false;
        self.hovered_message = 0;
        self.attachments = None;
        self.settings = None;
        self.server_settings = None;
        self.channel_settings = None;
        self.inbox = None;
        self.search = None;
        self.message_tools = None;
        self.show_inbox = false;
        self.show_settings = false;
        self.show_server_settings = false;
        self.show_channel_settings = false;
        self.show_search = false;
        self.show_tools = false;
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
