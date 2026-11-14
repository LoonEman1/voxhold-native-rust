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
        ]);
    }

    /// Key of the chat that owns the composer draft. A personal conversation
    /// has no server id, a thread adds its root to the channel key.
    pub(super) fn chat_key(&self) -> draft::ChatKey {
        if self.server == 0 {
            draft::ChatKey::conversation(self.channel)
        } else {
            draft::ChatKey::channel(self.server, self.channel)
        }
    }

    pub(super) fn composer_writable(&self) -> bool {
        !self.access_pending
            && self
                .channels
                .iter()
                .any(|channel| channel.id == self.channel && channel.writable())
    }

    /// Go `Editor.OnSubmit` / `Model.Send`: validate the draft, freeze one
    /// idempotent send and keep the text and files until the server confirms.
    pub(super) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.sending || !self.composer_writable() {
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
        self.send_attempt = self.send_attempt.wrapping_add(1);
        let attempt = self.send_attempt;
        let access = self.access_generation;
        let runtime = self.runtime.clone();
        self.sending = true;
        self.composer_error.clear();
        cx.notify();
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
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
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.send_attempt != attempt || s.access_generation != access || s.access_pending {
                    return;
                }
                s.sending = false;
                let (plan, message) = match result {
                    Ok(value) => value,
                    Err(error) => {
                        s.composer_error = error;
                        cx.notify();
                        return;
                    }
                };
                let composer_now = s.composer.read(cx).value().to_string();
                if s.apply_send_success(plan, message, &composer_now) {
                    s.composer
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
                s.composer.update(cx, |input, cx| input.focus(window, cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// Apply one confirmed send. Returns true when the composer must be
    /// cleared: only when the draft belonged to this chat, never when the user
    /// edited it while the request was in flight. A row that already arrived
    /// over WS is merged by id instead of being appended twice.
    pub(super) fn apply_send_success(
        &mut self,
        plan: draft::SendPlan,
        message: Message,
        composer_now: &str,
    ) -> bool {
        if self.chat_key() != plan.key
            || message.channel_id != self.channel
            || self.deleted_ids.contains(&message.id)
        {
            return false;
        }
        let sent_ids: Vec<String> = plan.files.iter().map(|file| file.id.clone()).collect();
        let finished = self.drafts.borrow_mut().finish_send(
            plan.key,
            plan.revision,
            &plan.composer_text,
            &sent_ids,
            composer_now,
        );
        self.merge_message(true, message);
        self.trim_history(false);
        finished == draft::Finish::Cleared && composer_now == plan.composer_text
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
        if !self.composer_writable()
            || self.show_settings
            || self.show_server_settings
            || self.show_channel_settings
            || self.show_inbox
        {
            return;
        }
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |panel, cx| panel.add_paths(paths, cx));
            cx.notify();
        }
    }
}
