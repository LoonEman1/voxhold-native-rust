use super::*;

impl Voxhold {
    pub(super) fn submit_instance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.instance.read(cx).value().to_string();
        let Some(origin) = self.auth.submit_instance(&raw) else {
            cx.notify();
            return;
        };
        // The address is confirmed: the canonical origin and the transition to
        // the login step stand even when the local preference cannot be
        // written. The checkbox state is persisted with the origin, so an
        // unchecked box also clears a previously remembered address.
        let prefs = crate::instance_prefs::InstancePrefs {
            remember: self.auth.remember_instance,
            instance: Some(origin.clone()),
        };
        let path = self.instance_path.clone().unwrap_or_default();
        if prefs.save(&path).is_err() {
            self.auth.status = crate::instance_prefs::SAVE_ERROR.to_string();
        }
        // Show the canonical origin that the login step and the session
        // will use (no trailing slash, no default port).
        self.instance.update(cx, |input, cx| {
            input.set_value(origin, window, cx);
        });
        self.username
            .update(cx, |input, cx| input.focus(window, cx));
        // The origin is confirmed. A remembered session may be checked now;
        // the attempt is entirely in the background and never blocks the form.
        self.begin_auto_login(window, cx);
        cx.notify();
    }

    /// Back to the address step; keeps the confirmed origin. An automatic
    /// attempt belongs to the login step, so it is cancelled logically; its
    /// late answer is ignored by the attempt id.
    #[allow(dead_code)]
    pub(super) fn back_to_instance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth.busy {
            return;
        }
        self.auto_attempt = self.auto_attempt.wrapping_add(1);
        self.auth.auto_login = false;
        self.auth.auto_retry = false;
        self.auto_record = None;
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
        // An empty form must not cancel a running automatic attempt; a filled
        // one supersedes it below.
        if self.auth.auto_login && (user.trim().chars().count() < 3 || pass.chars().count() < 8) {
            cx.notify();
            return;
        }
        let Some(request) = self.auth.submit_login(&user, &pass) else {
            cx.notify();
            return;
        };
        // A manual attempt owns the screen now. Any automatic answer that is
        // still in flight can no longer enter the workspace or touch the box.
        self.auto_attempt = self.auto_attempt.wrapping_add(1);
        self.auth.auto_login = false;
        self.auth.auto_retry = false;
        self.auto_record = None;
        self.remember_error.clear();
        self.error.clear();
        cx.notify();
        let attempt = request.attempt;
        let origin = request.origin.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                let mut c = Client::new(&origin).map_err(|e| e.to_string())?;
                let login = c
                    .login(&request.username, &request.password)
                    .await
                    .map_err(|e| auth::AuthState::describe_error(&e))?;
                // The login answer already carries the identity; a separate
                // profile request is no longer needed for `self_user_id`.
                let session = crate::auth_session::Session::new(
                    c.origin(),
                    login.user_id,
                    login.username.clone(),
                    login.token.clone(),
                    login.expires_at,
                )
                .ok_or_else(|| "Сервер не вернул сессию.".to_string())?;
                let servers = c
                    .servers()
                    .await
                    .map_err(|e| auth::AuthState::describe_error(&e))?;
                Ok::<_, String>((c, servers, session))
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
                let Ok((c, servers, login_session)) = result else {
                    return;
                };
                s.password
                    .update(cx, |input, cx| input.set_value("", window, cx));
                let origin = login_session.origin().to_owned();
                let account = login_session.username().to_owned();
                let user_id = login_session.user_id();
                let token = login_session.token().to_owned();
                let expires_at = login_session.expires_at();
                s.enter_workspace(c, servers, login_session, window, cx);
                // The box may have been toggled while the answer was in flight;
                // the state at completion decides, not the state at submit.
                let remember = s.auth.remember_login;
                s.remember_manual_login(
                    remember, origin, account, user_id, token, expires_at, window, cx,
                );
            });
        })
        .detach();
    }

    /// The single successful completion of a sign-in, used by the password
    /// form and by the remembered session. Everything that belongs to a new
    /// session is installed here, so both paths cannot drift apart.
    pub(super) fn enter_workspace(
        &mut self,
        c: Client,
        servers: Vec<Server>,
        login_session: crate::auth_session::Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let runtime = self.runtime.clone();
        let attachment_client = c.clone();
        let drafts = self.drafts.clone();
        let attachments = cx.new(|cx| {
            attachments::Attachments::new(attachment_client, runtime, drafts, window, cx)
        });
        // The composer's send button and file row depend on the draft owned by
        // this child entity; a notify keeps both in step.
        cx.observe_in(&attachments, window, |_s, _attachments, _window, cx| {
            cx.notify();
        })
        .detach();
        self.attachments = Some(attachments);
        self.personal = Some(personal_settings::PersonalSettings::new(window, cx));
        self.server_settings = Some(server_settings::ServerSettings::new(window, cx));
        self.channel_settings = Some(channel_settings::ChannelSettings::new(window, cx));
        self.channel_notifications = Some(channel_settings::ChannelNotifications::new());
        self.inbox = Some(cx.new(|_| inbox::Inbox::new()));
        let inbox = self.inbox.clone().expect("inbox");
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
        self.self_user_id = login_session.user_id();
        self.auth.username = login_session.username().to_owned();
        self.active_profile_save = None;
        self.notification_saves.clear();
        self.client = Some(c.clone());
        self.adopt_session(login_session, window, cx);
        self.start_realtime(c, window, cx);
        self.servers = servers;
        self.personal = Some(personal_settings::PersonalSettings::new(window, cx));
        self.server_settings = Some(server_settings::ServerSettings::new(window, cx));
        self.channel_settings = Some(channel_settings::ChannelSettings::new(window, cx));
        self.channel_notifications = Some(channel_settings::ChannelNotifications::new());
        self.auth.status = "Choose a server".into();
        // Go selects the first server after login; an empty list leaves
        // the workspace on its empty state. This also gives narrow
        // layouts a working channel list even without the rail.
        self.shell_initialized = false;
        let first_server = self.servers.first().map(|server| server.id);
        if let Some(id) = first_server {
            self.select_server(id, window, cx);
        }
        cx.notify();
    }

    /// A real logout: the local session and every job that belongs to it are
    /// stopped immediately, then one best-effort `POST /auth/logout` revokes the
    /// captured token. A network failure cannot keep the user signed in.
    pub(super) fn logout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Taking the revocation clears the credential, so a second logout (or a
        // late 401) cannot schedule a duplicate server request.
        let revocation = self
            .session
            .as_mut()
            .and_then(|session| session.take_revocation());
        // The remembered record belongs to the session being closed. The
        // delete is local only, so an offline logout still forgets it.
        let stored = self
            .session
            .as_ref()
            .map(|session| (session.origin().to_owned(), session.username().to_owned()));
        self.end_session(window, cx, String::new());
        self.forget_stored(stored, window, cx);
        if let Some(revocation) = revocation {
            let runtime = self.runtime.clone();
            runtime.spawn(async move {
                if let Ok(mut client) = Client::new(&revocation.origin) {
                    client.set_token(revocation.token);
                    // Best effort: no token, response or error goes to a log.
                    let _ = client.logout().await;
                }
            });
        }
    }

    /// A confirmed loss of the session (expired token or `401` on refresh).
    /// The token is invalid, so only local state is cleared. The remembered
    /// record cannot be used any more and is deleted even when offline.
    fn expire_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stored = self
            .session
            .as_ref()
            .map(|session| (session.origin().to_owned(), session.username().to_owned()));
        if let Some(session) = self.session.as_mut() {
            session.invalidate();
        }
        self.end_session(window, cx, "Сессия истекла. Войдите снова.".into());
        self.forget_stored(stored, window, cx);
    }

    /// Drop the whole signed-in state. `reason` is shown on the login screen;
    /// an empty reason is a normal logout.
    fn end_session(&mut self, window: &mut Window, cx: &mut Context<Self>, reason: String) {
        self.stop_session_clock();
        // Any write that captured the previous generation must not apply: a
        // save still queued from before the logout cannot revive the record,
        // and a delete from this session cannot remove a later login.
        self.session_store.bump_epoch();
        self.auto_attempt = self.auto_attempt.wrapping_add(1);
        self.last_stored_token = None;
        self.auto_record = None;
        self.remember_error.clear();
        let origin = self
            .session
            .as_ref()
            .map(|session| session.origin().to_owned())
            .unwrap_or_else(|| self.auth.origin.clone());
        let remember_instance = self.auth.remember_instance;
        self.session = None;
        self.session_epoch = self.session_epoch.wrapping_add(1);
        self.session_gate.reset();
        self.stop_voice_preview();
        self.stop_voice_meter();
        // A test tone or a preview error belongs to the signed-out session.
        self.voice_settings.tone_pending = false;
        self.voice_settings.tone_error.clear();
        self.voice_settings.preview_error.clear();
        self.voice_settings.device_notice.clear();
        self.stop_realtime();
        for (_, pending) in self.pending_sends.drain() {
            pending.abort.abort();
        }
        // The pending voice save would be aborted by the generation bump; write
        // it before the session it belongs to goes away.
        self.voice_settings.flush();
        self.session_generation = self.session_generation.wrapping_add(1);
        self.end_update_session();
        self.active_profile_save = None;
        self.notification_saves.clear();
        #[cfg(feature = "voxhold-media")]
        {
            self.abandon_voice(window, cx);
            // Room snapshots belong to the signed-in session.
            self.voice_rooms.clear();
            self.voice_streams.clear();
            self.voice_unconfirmed.clear();
            self.voice_chat_rooms.clear();
        }
        self.close_recordings(false, window, cx);
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
        self.auth = auth::AuthState::with_prefill(&origin);
        self.auth.screen = auth::AuthScreen::Login;
        self.auth.status = reason;
        self.auth.remember_instance = remember_instance;
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

    /// Install the one session owner and start its expiry/refresh timer.
    fn adopt_session(
        &mut self,
        session: crate::auth_session::Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session = Some(session);
        self.session_epoch = self.session_epoch.wrapping_add(1);
        // A new login owns a fresh store generation, so a delete that was
        // queued by the previous session is stale even if it runs later.
        self.session_store.bump_epoch();
        self.session_gate.reset();
        self.start_session_clock(window, cx);
    }

    pub(super) fn stop_session_clock(&mut self) {
        self.session_clock = None;
    }

    /// One timer covers expiry, the five-minute refresh margin and wake: the
    /// callback compares `expires_at` with the current clock instead of relying
    /// on a long sleep that a suspend would stretch.
    fn start_session_clock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_session_clock();
        let epoch = self.session_epoch;
        let executor = cx.background_executor().clone();
        self.session_clock = Some(cx.spawn_in(window, async move |this, cx| loop {
            executor.timer(std::time::Duration::from_secs(1)).await;
            let keep = this.update_in(cx, |s, window, cx| {
                if s.session_epoch != epoch || s.session.is_none() {
                    return false;
                }
                s.check_unauthorized(window, cx) && s.session_tick(window, cx)
            });
            if !matches!(keep, Ok(true)) {
                break;
            }
        }));
    }

    /// The shared client records every `401` with the token that was actually
    /// sent. Only a refusal of the current token ends the session; a late one
    /// from a token that was already rotated away is ignored.
    ///
    /// A refusal of the current token while its own rotation is in flight is
    /// not applied yet: the server revokes the old token before it answers
    /// `/auth/refresh`, so the answer may still deliver the new pair. The event
    /// stays queued (peek, no take) until that flight settles, exactly like
    /// `expire(expectedToken)` in `AuthContext.tsx` waits for the flight and
    /// then re-reads the token. This also keeps the drain from spinning on the
    /// same event inside one tick.
    pub(super) fn check_unauthorized(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        loop {
            let Some(token) = self
                .client
                .as_ref()
                .and_then(|client| client.peek_unauthorized())
            else {
                return true;
            };
            let (current, generation) = match self.session.as_ref() {
                Some(session) => (token == session.token(), session.generation()),
                None => (false, 0),
            };
            if !current {
                // A late refusal of a token that was already rotated away, or
                // of a session that has ended. Drop it now; it can never end a
                // future login.
                self.take_unauthorized();
                continue;
            }
            if self.session_gate.in_flight_for(generation) {
                // Wait for the refresh answer. The next tick re-reads the token:
                // a successful rotation makes this 401 stale, a terminal
                // failure ends the session anyway.
                return true;
            }
            self.take_unauthorized();
            // The state machine compares the refused token with the current one
            // once more and clears the credential only on a match.
            let matched = self
                .session
                .as_mut()
                .is_some_and(|session| session.handle_unauthorized(&token));
            if matched {
                self.expire_session(window, cx);
                return false;
            }
        }
    }

    fn take_unauthorized(&mut self) {
        if let Some(client) = self.client.as_ref() {
            client.take_unauthorized();
        }
    }

    /// One clock tick. Returns whether the timer should keep running.
    pub(super) fn session_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let now = crate::auth_session::now_unix();
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let plan = self.session_gate.plan(session, now);
        match plan {
            crate::auth_session::Plan::Idle => false,
            crate::auth_session::Plan::Expired => {
                self.expire_session(window, cx);
                false
            }
            crate::auth_session::Plan::Waiting => true,
            crate::auth_session::Plan::Refresh { generation, token } => {
                let Some(origin) = self
                    .session
                    .as_ref()
                    .map(|session| session.origin().to_owned())
                else {
                    return false;
                };
                self.start_refresh(generation, token, origin, window, cx);
                true
            }
        }
    }

    /// Start the one allowed rotation. The request uses its own client with the
    /// exact token from the plan, so an ordinary request cannot change the
    /// credential under it. No automatic retry of a mutating call is attempted:
    /// only this idempotent token rotation is repeated, and only within the
    /// bounded backoff of the gate. A mutate retry may be added later solely
    /// where the server contract proves idempotency (W09a).
    fn start_refresh(
        &mut self,
        generation: u64,
        token: String,
        origin: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let epoch = self.session_epoch;
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let mut client = Client::new(&origin)?;
                client.set_token(token);
                client.refresh().await
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.finish_refresh(epoch, generation, result, window, cx)
            });
        })
        .detach();
    }

    pub(super) fn finish_refresh(
        &mut self,
        epoch: u64,
        generation: u64,
        result: voxhold_api::Result<voxhold_api::SessionUpdate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A logout or another login owns the state now; this answer is stale.
        if self.session_epoch != epoch || self.session.is_none() {
            return;
        }
        self.session_gate.finish(generation);
        enum Step {
            Rotated,
            /// The credential is gone for good: 401, a rejected request or a
            /// malformed answer. No retry can repair any of them.
            Terminal,
            Retry,
            Ignore,
        }
        let mut rotation: Option<(String, String, String, String, i64)> = None;
        let step = match result {
            Ok(update) => {
                let before = self.session.as_ref().map(|session| {
                    (
                        session.token().to_owned(),
                        session.origin().to_owned(),
                        session.username().to_owned(),
                    )
                });
                let rotated = self.session.as_mut().is_some_and(|session| {
                    session.apply_rotation(generation, update.token, update.expires_at)
                });
                if rotated {
                    if let (Some((old_token, origin, account)), Some(session)) =
                        (before, self.session.as_ref())
                    {
                        rotation = Some((
                            old_token,
                            origin,
                            account,
                            session.token().to_owned(),
                            session.expires_at(),
                        ));
                    }
                    Step::Rotated
                } else {
                    Step::Ignore
                }
            }
            Err(error) => match crate::auth_session::classify_refresh_error(&error) {
                // The web client expires immediately for everything except the
                // transient set; only a 401 also names the refused credential.
                crate::auth_session::RefreshFailure::Unauthorized
                | crate::auth_session::RefreshFailure::Invalid => {
                    let current = self
                        .session
                        .as_ref()
                        .is_some_and(|session| session.generation() == generation);
                    if current {
                        Step::Terminal
                    } else {
                        Step::Ignore
                    }
                }
                crate::auth_session::RefreshFailure::Transient => Step::Retry,
            },
        };
        match step {
            Step::Rotated => {
                self.session_gate.succeeded(generation);
                self.apply_rotated_token(window, cx);
                if let Some((old_token, origin, account, new_token, expires_at)) = rotation {
                    self.persist_rotation(
                        epoch, origin, account, old_token, new_token, expires_at, window, cx,
                    );
                }
            }
            Step::Terminal => self.expire_session(window, cx),
            Step::Retry => {
                let now = crate::auth_session::now_unix();
                let expired = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_expired(now));
                if expired {
                    self.expire_session(window, cx);
                } else {
                    self.session_gate.retry_later(generation, now);
                }
            }
            Step::Ignore => {}
        }
        cx.notify();
    }

    /// The rotation was accepted: every clone of the main client already shares
    /// the new credential; the account socket is recreated and the open
    /// channel subscription and the active voice call are restored through
    /// their normal paths.
    pub(super) fn apply_rotated_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(token) = self
            .session
            .as_ref()
            .map(|session| session.token().to_owned())
        else {
            return;
        };
        if let Some(client) = self.client.as_mut() {
            client.set_token(token);
        }
        self.stop_realtime();
        if let Some(client) = self.client.clone() {
            self.start_realtime(client, window, cx);
            self.refresh_chat_subscription();
        }
        #[cfg(feature = "voxhold-media")]
        if let Some(panel) = self.voice.clone() {
            let channel = panel.read(cx).call_channel();
            if channel != 0 {
                if let Some(client) = self.client.clone() {
                    panel.update(cx, |panel, cx| {
                        panel.set_client(client);
                        panel.rejoin_call(channel, cx);
                    });
                }
                self.refresh_voice_call(window, cx);
            }
        }
        cx.notify();
    }
}
