use super::*;

gpui::actions!(voxhold, [ComposerSend, ComposerPasteFiles]);

impl Voxhold {
    /// Bind the composer shortcuts ahead of the `gpui-component` input
    /// bindings. A multi-line `InputState` binds plain Enter to "insert
    /// newline", so the more specific `Input && Composer` context owns Enter
    /// and Ctrl+V while the field is focused.
    pub(super) fn bind_composer_keys(cx: &mut Context<Self>) {
        cx.bind_keys([
            gpui::KeyBinding::new("enter", ComposerSend, Some("Composer > Input")),
            gpui::KeyBinding::new("ctrl-enter", ComposerSend, Some("Composer > Input")),
            gpui::KeyBinding::new("ctrl-v", ComposerPasteFiles, Some("Composer > Input")),
            gpui::KeyBinding::new("cmd-v", ComposerPasteFiles, Some("Composer > Input")),
            gpui::KeyBinding::new(
                "enter",
                thread_panel::ThreadSend,
                Some("ThreadComposer > Input"),
            ),
        ]);
    }

    /// Key of the chat that owns the composer draft. A personal conversation
    /// has no server id, a thread adds its root to the channel key.
    pub(super) fn chat_key(&self) -> draft::ChatKey {
        if self.conversation.is_some() {
            draft::ChatKey::conversation(self.channel)
        } else if self.server == 0 {
            draft::ChatKey::conversation(self.channel)
        } else {
            draft::ChatKey::channel(self.server, self.channel)
        }
    }

    pub(super) fn composer_writable(&self) -> bool {
        if self.conversation.is_some() {
            // A channel ACL reload is independent of the personal surface.
            return self.channel > 0;
        }
        !self.access_pending && self.channel_chat_writable(self.channel)
    }

    pub(super) fn is_sending(&self) -> bool {
        self.pending_sends.contains_key(&self.chat_key())
    }

    /// Reply bar, character counter, error and the composer row. The channel
    /// column and the docked room chat paint the same controls with a slightly
    /// tighter margin in the narrow panel.
    pub(super) fn render_composer(&mut self, cx: &mut Context<Self>, compact: bool) -> AnyElement {
        let p = self.palette;
        let pad = if compact { 10. } else { 18. };
        let mut body = div()
            .flex()
            .flex_col()
            .debug_selector(|| "chat-composer".into());
        let text = self.composer.read(cx).value().to_string();
        let files = self.drafts.borrow().files(self.chat_key());
        let reading = self
            .attachments
            .as_ref()
            .is_some_and(|attachments| attachments.read(cx).is_reading());
        let attachments_ready = self.attachments.is_some() && !self.is_sending() && !reading;
        let valid = draft::validate_send(&text, &files).is_ok();
        let can_send = valid
            && !self.is_sending()
            && !reading
            && !(self.access_pending && !self.in_conversation());
        let reply_id = self.drafts.borrow().reply(self.chat_key());
        if reply_id > 0 {
            let (author, excerpt) = self
                .messages
                .iter()
                .find(|message| message.id == reply_id)
                .map(|message| {
                    (
                        message.author.username.clone(),
                        message.content.replace('\n', " "),
                    )
                })
                .unwrap_or_else(|| ("Сообщение".into(), format!("#{reply_id}")));
            body = body.child(
                div()
                    .mx(px(pad))
                    .mb(px(4.))
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .bg(ink(p.surface))
                    .border_l_2()
                    .border_color(ink(p.lime))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(11.))
                            .text_color(ink(p.muted))
                            .child(format!("Ответ: {author} · {excerpt}")),
                    )
                    .child(
                        div()
                            .id("clear-reply")
                            .cursor_pointer()
                            .text_size(px(11.))
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, _, cx| s.clear_reply(cx)))
                            .child("Убрать"),
                    ),
            );
        }
        if let Some(counter) = draft::counter(&text) {
            body = body.child(
                div()
                    .px(px(pad))
                    .pb(px(2.))
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(counter),
            );
        }
        if !self.composer_error.is_empty() {
            body = body.child(
                div()
                    .px(px(pad))
                    .pb(px(2.))
                    .text_size(px(12.))
                    .text_color(ink(p.danger))
                    .child(self.composer_error.clone()),
            );
        }
        let send_button = div()
            .id("send")
            .w(px(if compact { 96. } else { 112. }))
            .h(px(40.))
            .rounded(px(11.))
            .bg(ink(if can_send { p.lime } else { p.surface }))
            .text_color(ink(if can_send { 0x10100f } else { p.muted }))
            .font_weight(gpui::FontWeight::BOLD)
            .text_size(px(14.))
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .when(can_send, |button| {
                button
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, w, cx| s.send(w, cx)))
            })
            .child(if self.is_sending() {
                "Отправка…".into_any_element()
            } else {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(
                        "icons/send.svg",
                        16.,
                        ink(if can_send { 0x10100f } else { p.muted }),
                    ))
                    .child("Отправить")
                    .into_any_element()
            });
        body.child(
            div()
                .px(px(pad))
                .py(px(12.))
                .flex()
                .items_end()
                .gap_2()
                .when(attachments_ready, |row| {
                    row.child(
                        div()
                            .id("attach-files")
                            .w(px(37.))
                            .h(px(37.))
                            .rounded(px(10.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(attachments) = &s.attachments {
                                    attachments.update(cx, |panel, cx| panel.pick(cx));
                                }
                            }))
                            .child(icon("icons/paperclip.svg", 17., ink(p.muted))),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_h(px(47.))
                        .key_context("Composer")
                        .on_action(cx.listener(|s, _: &composer::ComposerSend, window, cx| {
                            // While the viewer is open it owns the
                            // keyboard, like Go `imageViewerEvent`.
                            if s.media.viewer.is_some() {
                                return;
                            }
                            s.send(window, cx);
                        }))
                        .on_action(cx.listener(
                            |s, _: &composer::ComposerPasteFiles, _window, cx| {
                                if s.media.viewer.is_some() {
                                    return;
                                }
                                if !s.paste_attachments(cx) {
                                    // The clipboard has no files: let the
                                    // input widget paste ordinary text.
                                    cx.dispatch_action(&gpui_component::input::Paste);
                                }
                            },
                        ))
                        .child(
                            TextInput::new(&self.composer)
                                .font_family("Inter")
                                .text_size(px(14.))
                                .h_full(),
                        ),
                )
                .child(send_button),
        )
        .into_any_element()
    }

    /// Go `Editor.OnSubmit` / `Model.Send`: validate the draft, freeze one
    /// idempotent send and keep the text and files until the server confirms.
    pub(super) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.is_sending() || !self.composer_writable() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        if self
            .attachments
            .as_ref()
            .is_some_and(|attachments| attachments.read(cx).is_reading())
        {
            return;
        }
        let key = self.chat_key();
        if key.channel <= 0 {
            return;
        }
        let text = self.composer.read(cx).value().to_string();
        let plan = match self.drafts.borrow_mut().begin_send(key, &text) {
            Ok(plan) => plan,
            Err(error) => {
                self.composer_error = error;
                cx.notify();
                return;
            }
        };
        let plan_key = key;
        self.send_attempt = self.send_attempt.wrapping_add(1);
        let attempt = self.send_attempt;
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        // A new composition of this chat replaces any uncertain attempt: its
        // late echo may still update the history, but it must not consume the
        // new draft or clear its idempotency key.
        self.unconfirmed_sends.remove(&key);
        // The whole upload + POST chain runs as one runtime task so `abort()`
        // really stops it; a GPUI task wrapping `block_on` would keep going.
        // Keep the HTTP status so a conversation 404 can clear the ChatKey and
        // a 403 can be disambiguated with one explicit conversation GET, and
        // keep whether the POST itself started so a network error/504 is not
        // confused with a local upload refusal.
        let task_plan = plan.clone();
        let task = runtime.spawn(async move {
            let ids = attachments::upload_draft(&client, &task_plan)
                .await
                .map_err(|text| (None, text, false))?;
            let message = if task_plan.key.is_conversation() {
                client
                    .create_conversation_message(
                        task_plan.key.channel,
                        &task_plan.content,
                        &task_plan.send_key,
                        &ids,
                    )
                    .await
            } else {
                client
                    .send_channel_message(
                        task_plan.key.server,
                        task_plan.key.channel,
                        &task_plan.content,
                        &task_plan.send_key,
                        &ids,
                        task_plan.reply,
                        task_plan.key.root,
                    )
                    .await
            };
            message
                .map(|message| (task_plan, message))
                .map_err(|error| (error.status(), draft::describe_error(&error), true))
        });
        let abort = task.abort_handle();
        self.pending_sends.insert(
            key,
            PendingSend {
                attempt,
                abort,
                plan: plan.clone(),
            },
        );
        self.composer_error.clear();
        cx.notify();
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                // Logout aborts the task and bumps the session; a result from
                // another session never touches the new one.
                if s.session_generation != session {
                    return;
                }
                let current = s
                    .pending_sends
                    .get(&plan_key)
                    .is_some_and(|pending| pending.attempt == attempt);
                if !current {
                    return;
                }
                s.pending_sends.remove(&plan_key);
                let result = match result {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(error)) => Err(error),
                    Err(_aborted) => return,
                };
                let (plan, message) = match result {
                    Ok(value) => value,
                    Err((status, text, post_started)) => {
                        // An upload refusal happens before the POST, so its
                        // outcome is certain. A network error or a `504` of the
                        // POST leaves the outcome unknown: keep the plan so an
                        // own WebSocket echo can still confirm it.
                        let uncertain = post_started && (status.is_none() || status == Some(504));
                        let active = s.chat_key() == plan_key;
                        if uncertain {
                            s.unconfirmed_sends.insert(plan_key, plan);
                        }
                        if plan_key.is_conversation() && !uncertain {
                            match status {
                                Some(404) => {
                                    // A missing conversation is authoritative for
                                    // its row even after a switch; only the live
                                    // surface is cleared with a window, while a
                                    // background row's draft scope goes too.
                                    if active {
                                        s.conversations.retain(|row| row.id != plan_key.channel);
                                        s.clear_active_conversation(
                                            Some("Диалог недоступен."),
                                            window,
                                            cx,
                                        );
                                    } else {
                                        s.forget_conversation(plan_key.channel, None, cx);
                                    }
                                    s.refresh_recents(cx);
                                }
                                Some(403) => {
                                    // A conversation `403` also covers a
                                    // `canContact` refusal while both users stay
                                    // members. Keep the active draft and ask one
                                    // explicit GET scoped to the row.
                                    if active {
                                        s.composer_error = text.clone();
                                    }
                                    s.verify_conversation_after_403(plan_key, Some(text), cx);
                                }
                                _ => {
                                    if active {
                                        s.composer_error = text;
                                    }
                                }
                            }
                        } else if active {
                            s.composer_error = text;
                        }
                        cx.notify();
                        return;
                    }
                };
                let composer_now = s.composer.read(cx).value().to_string();
                let active = s.chat_key() == plan.key;
                s.confirm_unconfirmed(&plan);
                if active {
                    s.composer_error.clear();
                }
                if s.apply_send_success(plan, message, &composer_now, cx) {
                    s.composer
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
                if active {
                    s.composer.update(cx, |input, cx| input.focus(window, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Forget the uncertain plan of `plan`'s own attempt once a confirmation
    /// arrives. A stored plan with another send key belongs to a newer
    /// composition and must stay.
    pub(super) fn confirm_unconfirmed(&mut self, plan: &draft::SendPlan) {
        if self
            .unconfirmed_sends
            .get(&plan.key)
            .is_some_and(|stored| stored.send_key == plan.send_key)
        {
            self.unconfirmed_sends.remove(&plan.key);
        }
    }

    /// Apply one confirmed send. Returns true when the composer must be
    /// cleared: only when the draft belongs to the open chat and the user did
    /// not edit it while the request was in flight. A result for another chat
    /// still updates that chat's draft, so a switch does not turn a delivered
    /// message into an unsent draft.
    pub(super) fn apply_send_success(
        &mut self,
        plan: draft::SendPlan,
        message: Message,
        composer_now: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let active = self.chat_key() == plan.key;
        let sent_ids: Vec<String> = plan.files.iter().map(|file| file.id.clone()).collect();
        let finished = self.drafts.borrow_mut().finish_send(
            plan.key,
            plan.revision,
            &plan.composer_text,
            &sent_ids,
            active.then_some(composer_now),
        );
        // The draft still belongs to the original chat, but a channel feed may
        // only receive the confirmed row while access to the open channel is
        // confirmed. `invalidate_access` clears that history and sets the flag;
        // a late channel POST answer must not put a private row back into that
        // gap. A personal conversation is independent of the channel ACL, so a
        // DM row may merge once `active`/`message_in_scope` prove it belongs to
        // the open conversation. The WS `conversation.message` echo normally
        // hides this, but a replayed send (`replayed=true`) has no echo.
        let access_ready =
            !self.access_pending || (plan.key.is_conversation() && self.in_conversation());
        if access_ready
            && active
            && self.message_in_scope(&message)
            && !self.deleted_ids.contains(&message.id)
        {
            self.merge_message(true, message);
            self.trim_history(false);
        }
        if plan.key.is_conversation() {
            self.refresh_recents(cx);
        }
        active && finished == draft::Finish::Cleared && composer_now == plan.composer_text
    }

    /// Go `OnPasteFiles` priority: paths, then a clipboard picture, then the
    /// normal text paste. Returns true when the composer consumed the event.
    pub(super) fn paste_attachments(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.composer_writable() {
            return false;
        }
        let paste = crate::clipboard::read();
        if !paste.is_file_paste() {
            return false;
        }
        let Some(attachments) = &self.attachments else {
            return false;
        };
        let handled = attachments.update(cx, |panel, cx| panel.paste(paste, cx));
        cx.notify();
        handled
    }

    /// OS file drop. Go ignores drops while another overlay owns the screen.
    pub(super) fn drop_files(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        if !self.composer_writable() || self.active_settings.is_some() {
            return;
        }
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |panel, cx| panel.add_paths(paths, cx));
            cx.notify();
        }
    }
}
