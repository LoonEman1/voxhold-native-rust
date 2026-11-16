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
            return !self.access_pending && self.channel > 0;
        }
        !self.access_pending
            && self
                .channels
                .iter()
                .any(|channel| channel.id == self.channel && channel.writable())
    }

    pub(super) fn is_sending(&self) -> bool {
        self.pending_sends.contains_key(&self.chat_key())
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
        // The whole upload + POST chain runs as one runtime task so `abort()`
        // really stops it; a GPUI task wrapping `block_on` would keep going.
        let task = runtime.spawn(async move {
            let ids = attachments::upload_draft(&client, &plan).await?;
            let message = if plan.key.is_conversation() {
                client
                    .create_conversation_message(
                        plan.key.channel,
                        &plan.content,
                        &plan.send_key,
                        &ids,
                    )
                    .await
            } else {
                client
                    .send_channel_message(
                        plan.key.server,
                        plan.key.channel,
                        &plan.content,
                        &plan.send_key,
                        &ids,
                        plan.reply,
                        plan.key.root,
                    )
                    .await
            };
            message
                .map(|message| (plan, message))
                .map_err(|error| draft::describe_error(&error))
        });
        let abort = task.abort_handle();
        self.pending_sends
            .insert(key, PendingSend { attempt, abort });
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
                    Err(error) => {
                        if s.chat_key() == plan_key {
                            s.composer_error = error;
                        }
                        cx.notify();
                        return;
                    }
                };
                let composer_now = s.composer.read(cx).value().to_string();
                let active = s.chat_key() == plan.key;
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
        // The draft still belongs to the original chat, but the feed may only
        // receive the confirmed row while access to the open channel is
        // confirmed. `invalidate_access` clears history and sets this flag;
        // a late POST answer must not put a private row back into that gap.
        if !self.access_pending
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
        if !self.composer_writable() || self.active_settings.is_some() || self.show_inbox {
            return;
        }
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |panel, cx| panel.add_paths(paths, cx));
            cx.notify();
        }
    }
}
