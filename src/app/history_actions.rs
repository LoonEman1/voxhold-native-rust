use super::*;
use crate::message_tools::{self, Action, HoverAction, MessagePermissions};

/// Stable key for one in-flight action on one row.
pub(super) fn action_key(message_id: i64, kind: &str) -> String {
    format!("{message_id}:{kind}")
}

impl Voxhold {
    pub(super) fn current_role(&self) -> String {
        self.servers
            .iter()
            .find(|server| server.id == self.server)
            .map(|server| server.role.clone())
            .unwrap_or_default()
    }

    pub(super) fn current_channel(&self) -> Option<Channel> {
        self.channels
            .iter()
            .find(|channel| channel.id == self.channel)
            .cloned()
    }

    pub(super) fn in_conversation(&self) -> bool {
        self.conversation.is_some()
    }

    /// One row from the confirmed window, looked up by id only.
    pub(super) fn message_snapshot(&self, id: i64) -> Option<Message> {
        self.messages
            .iter()
            .find(|message| message.id == id)
            .cloned()
    }

    pub(super) fn permissions_for_row(&self, message: &Message) -> MessagePermissions {
        let channel = self.current_channel();
        message_tools::permissions_for(
            self.self_user_id,
            &self.current_role(),
            channel.as_ref(),
            message,
            self.in_conversation(),
        )
    }

    pub(super) fn action_allowed(&self, action: Action, permissions: MessagePermissions) -> bool {
        match action {
            Action::Discuss => permissions.comments,
            Action::Reply => permissions.reply,
            Action::Edit => permissions.edit,
            Action::Pin | Action::Unpin => permissions.pin,
            Action::Copy => true,
            Action::Save => permissions.save,
            Action::Forward => permissions.forward,
            Action::Delete => permissions.delete,
            Action::Block => permissions.block,
        }
    }

    pub(super) fn action_pending(&self, message_id: i64, kind: &str) -> bool {
        self.action_requests
            .contains_key(&action_key(message_id, kind))
    }

    pub(super) fn begin_action(&mut self, message_id: i64, kind: &str) -> Option<ActionRequest> {
        let key = action_key(message_id, kind);
        if self.action_requests.contains_key(&key) {
            return None;
        }
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = ActionRequest {
            chat: self.chat_key(),
            access: self.access_generation,
            request: self.action_seq,
        };
        self.action_requests.insert(key, request.clone());
        self.action_error = None;
        Some(request)
    }

    pub(super) fn accepts_action(
        &self,
        message_id: i64,
        kind: &str,
        request: &ActionRequest,
    ) -> bool {
        let key = action_key(message_id, kind);
        self.action_requests.get(&key) == Some(request)
            && self.chat_key() == request.chat
            && self.access_generation == request.access
    }

    pub(super) fn finish_action(&mut self, message_id: i64, kind: &str, request: &ActionRequest) {
        let key = action_key(message_id, kind);
        if self.action_requests.get(&key) == Some(request) {
            self.action_requests.remove(&key);
        }
    }

    pub(super) fn open_message_menu(
        &mut self,
        message_id: i64,
        x: f32,
        y: f32,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        if self.message_snapshot(message_id).is_none() {
            return;
        }
        if let Some(message) = self.message_snapshot(message_id) {
            if let Some(attachments) = &self.attachments {
                attachments.update(cx, |attachments, cx| {
                    attachments.set_message(Some(&message), cx)
                });
            }
            let mine = message
                .reactions
                .iter()
                .filter(|reaction| reaction.me)
                .map(|reaction| reaction.emoji.clone())
                .collect::<Vec<_>>();
            let weak = cx.entity().downgrade();
            self.reaction_picker.update(cx, |picker, cx| {
                picker.on_pick = Some(Box::new(move |value, cx| {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |s, cx| {
                            let Some(menu) = s.message_menu.clone() else {
                                return;
                            };
                            let active =
                                s.message_snapshot(menu.message_id).is_some_and(|message| {
                                    message
                                        .reactions
                                        .iter()
                                        .any(|reaction| reaction.emoji == value && reaction.me)
                                });
                            s.message_menu = None;
                            s.toggle_reaction(menu.message_id, value.to_string(), active, cx);
                        });
                    }
                }));
                picker.reset_for_message(mine, cx);
            });
        }
        self.message_menu = Some(MessageMenu {
            message_id,
            x,
            y,
            expanded,
            confirm_delete: false,
        });
        self.action_error = None;
        self.refresh_pins(cx);
        cx.notify();
    }
    /// Hover buttons from the previous frame must not fire after the list
    /// moved. The epoch is bumped by every scroll notification.
    pub(super) fn dispatch_hover_action(
        &mut self,
        message_id: i64,
        action: HoverAction,
        epoch: u64,
        x: f32,
        y: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.scroll_epoch {
            return;
        }
        let expected = (message_id, action.key().to_string());
        if self.hovered_action.as_ref() != Some(&expected) {
            return;
        }
        match action {
            HoverAction::Quick(emoji) => {
                let Some(message) = self.message_snapshot(message_id) else {
                    return;
                };
                if !self.permissions_for_row(&message).react {
                    return;
                }
                let active = message
                    .reactions
                    .iter()
                    .any(|reaction| reaction.emoji == emoji && reaction.me);
                self.toggle_reaction(message_id, emoji.to_string(), active, cx);
            }
            HoverAction::Picker => {
                self.open_message_menu(message_id, x, y, true, cx);
            }
            HoverAction::More => {
                self.open_message_menu(message_id, x, y, false, cx);
            }
            HoverAction::Reply | HoverAction::Forward => {
                if let Some(action) = message_tools::hover_menu_action(action) {
                    self.dispatch_message_action(message_id, action, window, cx);
                }
            }
        }
    }

    /// The only dispatcher shared by the hover bar and the context menu. It
    /// re-reads the row, permissions and id before doing anything.
    pub(super) fn dispatch_message_action(
        &mut self,
        message_id: i64,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.message_snapshot(message_id) else {
            self.action_error = Some((message_id, "Сообщение недоступно".into()));
            self.message_menu = None;
            cx.notify();
            return;
        };
        let permissions = self.permissions_for_row(&message);
        if !self.action_allowed(action, permissions) {
            self.action_error = Some((message_id, "Недостаточно прав доступа.".into()));
            self.message_menu = None;
            cx.notify();
            return;
        }
        match action {
            Action::Discuss => {
                self.message_menu = None;
                self.open_thread(message, 0, window, cx);
            }
            Action::Reply => {
                self.message_menu = None;
                self.set_reply_to(&message, window, cx);
            }
            Action::Edit => {
                self.message_menu = None;
                self.start_edit(message_id, window, cx);
            }
            Action::Pin => {
                self.message_menu = None;
                self.set_pin(message_id, true, cx);
            }
            Action::Unpin => {
                self.message_menu = None;
                self.set_pin(message_id, false, cx);
            }
            Action::Copy => {
                self.message_menu = None;
                self.copy_messages(message_id, cx);
            }
            Action::Save => {
                self.message_menu = None;
                self.save_to_favorites(message_id, cx);
            }
            Action::Forward => {
                self.message_menu = None;
                self.open_forward(message_id, cx);
            }
            Action::Delete => {
                if let Some(menu) = self.message_menu.as_mut() {
                    menu.confirm_delete = true;
                } else {
                    self.confirm_delete = Some(message_id);
                }
                cx.notify();
            }
            Action::Block => {
                self.message_menu = None;
                self.block_author(message_id, cx);
            }
        }
    }

    pub(super) fn set_reply_to(
        &mut self,
        message: &Message,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = self.chat_key();
        if self.drafts.borrow_mut().set_reply(key, message.id) {
            self.action_error = None;
            self.action_notice.clear();
        }
        self.composer
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(super) fn clear_reply(&mut self, cx: &mut Context<Self>) {
        let key = self.chat_key();
        self.drafts.borrow_mut().set_reply(key, 0);
        cx.notify();
    }

    pub(super) fn start_edit(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.message_snapshot(id) else {
            return;
        };
        if !self.permissions_for_row(&message).edit {
            return;
        }
        // Every opening installs a new editor generation; a PATCH started for an
        // older opening must not close this form.
        self.edit_open_seq = self.edit_open_seq.wrapping_add(1);
        self.edit_error = None;
        self.edit_input.update(cx, |input, cx| {
            input.set_value(message.content.clone(), window, cx);
            input.focus(window, cx);
        });
        self.editing = Some(id);
        self.confirm_delete = None;
        self.action_error = None;
        cx.notify();
    }

    /// Close the inline editor, invalidate every request that belongs to it and
    /// drop the error that was bound to its generation.
    pub(super) fn close_edit(&mut self) {
        if self.editing.take().is_some() {
            self.edit_open_seq = self.edit_open_seq.wrapping_add(1);
        }
        self.edit_error = None;
    }

    pub(super) fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        let was_open = self.editing.is_some();
        self.close_edit();
        if was_open {
            cx.notify();
        }
    }

    pub(super) fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing else {
            return;
        };
        let Some(message) = self.message_snapshot(id) else {
            self.close_edit();
            cx.notify();
            return;
        };
        if !self.permissions_for_row(&message).edit {
            self.action_error = Some((id, "Недостаточно прав доступа.".into()));
            self.close_edit();
            cx.notify();
            return;
        }
        let text = self.edit_input.read(cx).value().to_string();
        if text.trim().is_empty() || text.chars().count() > draft::MAX_CONTENT {
            self.action_error = Some((
                id,
                "Введите текст сообщения длиной до 5120 символов.".into(),
            ));
            cx.notify();
            return;
        }
        if text == message.content {
            self.close_edit();
            cx.notify();
            return;
        }
        let Some(request) = self.begin_action(id, "edit") else {
            return;
        };
        let Some(client) = self.client.clone() else {
            self.finish_action(id, "edit", &request);
            return;
        };
        let (server, channel) = (self.server, self.channel);
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        // The generation and the exact text that go on the wire decide whether
        // the answer may close the editor. The server may normalize the text,
        // so the comparison uses the sent value, not the answer body.
        let open_seq = self.edit_open_seq;
        let sent = text.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(client.edit_message(server, channel, id, &text))
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.accepts_action(id, "edit", &request) {
                    return;
                }
                s.finish_action(id, "edit", &request);
                match result {
                    Ok(updated) => {
                        if updated.id == id {
                            s.merge_message(false, updated);
                            s.trim_history(false);
                            s.sync_thread_root(window, cx);
                        }
                        if s.editing == Some(id) && s.edit_open_seq == open_seq {
                            s.edit_error = None;
                            if s.edit_input.read(cx).value().to_string() == sent {
                                s.close_edit();
                            }
                        }
                    }
                    Err(error) => {
                        // Only the editor that started this attempt may show its
                        // error while it is still open.
                        if s.editing == Some(id) && s.edit_open_seq == open_seq {
                            s.edit_error = Some((id, open_seq, error.to_string()));
                        }
                        if error.status() == Some(403) {
                            s.recheck_channel_snapshot(window, cx);
                        } else if error.status() == Some(404) {
                            s.note_message_deleted(id, window, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        if self.confirm_delete.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn confirm_delete_now(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.message_snapshot(id) else {
            self.confirm_delete = None;
            return;
        };
        if !self.permissions_for_row(&message).delete {
            self.action_error = Some((id, "Недостаточно прав доступа.".into()));
            self.confirm_delete = None;
            cx.notify();
            return;
        }
        let Some(request) = self.begin_action(id, "delete") else {
            return;
        };
        let Some(client) = self.client.clone() else {
            self.finish_action(id, "delete", &request);
            return;
        };
        let (server, channel) = (self.server, self.channel);
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime.block_on(client.delete_message(server, channel, id))
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.accepts_action(id, "delete", &request) {
                    return;
                }
                s.finish_action(id, "delete", &request);
                s.confirm_delete = None;
                match result {
                    Ok(()) => s.note_message_deleted(id, window, cx),
                    Err(error) if error.status() == Some(404) => {
                        s.note_message_deleted(id, window, cx)
                    }
                    Err(error) => {
                        s.action_error = Some((id, error.to_string()));
                        if error.status() == Some(403) {
                            s.recheck_channel_snapshot(window, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A confirmed deletion is terminal for this window and for an open
    /// thread rooted at the row.
    pub(super) fn note_message_deleted(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.deleted_ids.insert(id);
        self.messages.retain(|message| message.id != id);
        self.pending_edits.remove(&id);
        self.pending_reactions.remove(&id);
        if self.editing == Some(id) {
            self.close_edit();
        }
        self.selected_messages.retain(|selected| *selected != id);
        if self.highlighted_message.is_some_and(|(row, _)| row == id) {
            self.highlighted_message = None;
        }
        let root = self.thread.as_ref().map(|thread| thread.read(cx).root_id());
        if root == Some(id) {
            self.close_thread(window, cx);
        } else if let Some(thread) = &self.thread {
            thread.update(cx, |thread, cx| thread.remove_message(id, cx));
        }
        if self
            .media
            .viewer
            .as_ref()
            .is_some_and(|viewer| viewer.key.message_id == id)
        {
            self.close_viewer(window);
        }
        cx.notify();
    }

    pub(super) fn set_pin(&mut self, id: i64, pinned: bool, cx: &mut Context<Self>) {
        if self.in_conversation() {
            return;
        }
        let Some(message) = self.message_snapshot(id) else {
            return;
        };
        if !self.permissions_for_row(&message).pin {
            self.action_error = Some((id, "Недостаточно прав доступа.".into()));
            cx.notify();
            return;
        }
        let kind = "pin";
        let Some(request) = self.begin_action(id, kind) else {
            return;
        };
        let Some(client) = self.client.clone() else {
            self.finish_action(id, kind, &request);
            return;
        };
        let (server, channel) = (self.server, self.channel);
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime.block_on(client.set_message_pin(server, channel, id, pinned))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.accepts_action(id, kind, &request) {
                        return;
                    }
                    s.finish_action(id, kind, &request);
                    match result {
                        Ok(()) => {
                            if pinned {
                                s.pinned_ids.insert(id);
                            } else {
                                s.pinned_ids.remove(&id);
                            }
                            s.refresh_pins(cx);
                            s.action_error = None;
                            s.action_notice = if pinned {
                                "Сообщение закреплено"
                            } else {
                                "Закрепление снято"
                            }
                            .into();
                        }
                        Err(error) => {
                            s.action_error = Some((id, error.to_string()));
                            if error.status() == Some(403) {
                                s.recheck_channel_snapshot_no_window(cx);
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Scope captured when a search row is clicked. The deferred navigation
    /// only runs while the same session, server and access snapshot are open
    /// and the popup still owns the screen.
    pub(super) fn search_jump_scope(&self) -> (u64, i64, u64, bool) {
        (
            self.session_generation,
            self.server,
            self.access_generation,
            self.show_search,
        )
    }

    /// Keep the search popup's permission snapshot aligned with the confirmed
    /// channel rows. While an access snapshot is being read, the popup stays
    /// closed to queries and keeps no allowed ids.
    pub(super) fn sync_search_permissions(&mut self, cx: &mut Context<Self>) {
        let ready = self.search_permissions_ready
            && self.server > 0
            && !self.access_pending
            && !self.access_reload_in_flight;
        let allowed: std::collections::HashSet<i64> = if ready {
            self.channels
                .iter()
                .filter(|channel| channel.has_history())
                .map(|channel| channel.id)
                .collect()
        } else {
            Default::default()
        };
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| panel.set_permissions(allowed, ready, cx));
        }
    }

    /// Read a fresh channel list before a search from a personal conversation.
    /// The popup stays unconfirmed until this answer is accepted.
    pub(super) fn refresh_search_permissions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.server <= 0 {
            self.search_permissions_ready = false;
            self.sync_search_permissions(cx);
            return;
        }
        if self.search_permissions_request.is_some() {
            self.sync_search_permissions(cx);
            return;
        }
        let Some(client) = self.client.clone() else {
            self.search_permissions_ready = false;
            self.sync_search_permissions(cx);
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.search_permissions_request = Some(request);
        self.search_permissions_ready = false;
        self.sync_search_permissions(cx);
        let (server, generation, access) = (self.server, self.generation, self.access_generation);
        let runtime = self.runtime.clone();
        let task = runtime.spawn(async move { client.channels(server).await });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.search_permissions_request != Some(request) {
                    return;
                }
                s.search_permissions_request = None;
                if s.server != server || s.generation != generation || s.access_generation != access
                {
                    return;
                }
                match result {
                    Ok(Ok(channels)) => {
                        s.channels = channels;
                        s.search_permissions_ready = true;
                        s.sync_search_permissions(cx);
                        // A pin refresh started while the snapshot was
                        // unconfirmed returned immediately. Run it now so the
                        // row menu and popup see the confirmed pin set.
                        s.refresh_pins(cx);
                    }
                    Ok(Err(voxhold_api::Error::Http(403 | 404))) => {
                        s.search_permissions_ready = false;
                        if !s.in_conversation() {
                            s.invalidate_access(window, cx);
                        } else {
                            s.sync_search_permissions(cx);
                        }
                    }
                    _ => {
                        // Keep the popup unconfirmed; the next open retries.
                        s.search_permissions_ready = false;
                        s.sync_search_permissions(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Search and pin jumps validate the target against the current channel
    /// snapshot first, so a fake or revoked result cannot reveal a closed
    /// channel.
    pub(super) fn search_target(
        &self,
        channel_id: i64,
        message_id: i64,
    ) -> Result<i64, &'static str> {
        if channel_id <= 0 || message_id <= 0 {
            return Err("Сообщение недоступно");
        }
        self.channels
            .iter()
            .find(|channel| channel.id == channel_id && channel.has_history())
            .map(|_| channel_id)
            .ok_or("Канал недоступен.")
    }

    pub(super) fn open_search_hit(
        &mut self,
        channel_id: i64,
        message_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_search = false;
        self.message_menu = None;
        let target = match self.search_target(channel_id, message_id) {
            Ok(target) => target,
            Err(error) => {
                self.error = error.into();
                cx.notify();
                return;
            }
        };
        if self.in_conversation() || self.channel != target {
            self.select_channel(target, window, cx);
        }
        self.jump_to_message(message_id, window, cx);
        cx.notify();
    }

    /// Release the channel-snapshot slot only when it still belongs to this
    /// request. A callback from channel A must leave B's slot untouched.
    pub(super) fn release_channel_snapshot(&mut self, request: u64) -> bool {
        if self.channel_snapshot_request == Some(request) {
            self.channel_snapshot_request = None;
            true
        } else {
            false
        }
    }

    /// Reload the channel snapshot after `403` without hiding the confirmed
    /// history. Actions that are no longer allowed disappear immediately.
    pub(super) fn recheck_channel_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.server <= 0 || self.channel_snapshot_request.is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.channel_snapshot_request = Some(request);
        self.search_permissions_ready = false;
        self.sync_search_permissions(cx);
        let (server, channel, generation, access) = (
            self.server,
            self.channel,
            self.generation,
            self.access_generation,
        );
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move { runtime.block_on(client.channels(server)) });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                // A late answer must not clear the slot that a newer request
                // installed after the channel changed.
                if !s.release_channel_snapshot(request) {
                    return;
                }
                if s.server != server
                    || s.channel != channel
                    || s.generation != generation
                    || s.access_generation != access
                {
                    return;
                }
                if let Ok(channels) = result {
                    s.channels = channels;
                    s.search_permissions_ready = true;
                    s.sync_search_permissions(cx);
                    s.refresh_pins(cx);
                    #[cfg(feature = "voxhold-media")]
                    s.prune_voice_chat();
                    let readable = s.channel_chat_readable(s.channel);
                    if !readable {
                        s.invalidate_access(window, cx);
                        return;
                    }
                    if let Some(menu) = s.message_menu.as_ref() {
                        let id = menu.message_id;
                        let allowed = s
                            .message_snapshot(id)
                            .is_some_and(|message| s.permissions_for_row(&message).edit);
                        if !allowed {
                            s.close_edit();
                        }
                    }
                } else {
                    s.sync_search_permissions(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn recheck_channel_snapshot_no_window(&mut self, cx: &mut Context<Self>) {
        // Reaction/pin paths run without a `Window`; reuse the generic channel
        // reload only when it can finish through the existing task owner.
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.server <= 0 || self.channel_snapshot_request.is_some() {
            return;
        }
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.channel_snapshot_request = Some(request);
        self.search_permissions_ready = false;
        self.sync_search_permissions(cx);
        let (server, generation, access) = (self.server, self.generation, self.access_generation);
        let runtime = self.runtime.clone();
        let task = runtime.spawn(async move { client.channels(server).await });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(_) => Err(voxhold_api::Error::Network),
            };
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    // Only the request that still owns the slot may confirm
                    // its answer. A late reply from an older screen must not
                    // turn search back on.
                    if !s.release_channel_snapshot(request) {
                        return;
                    }
                    if s.server != server
                        || s.generation != generation
                        || s.access_generation != access
                    {
                        return;
                    }
                    if let Ok(channels) = result {
                        s.channels = channels;
                        s.search_permissions_ready = true;
                        s.sync_search_permissions(cx);
                        s.refresh_pins(cx);
                    } else {
                        s.sync_search_permissions(cx);
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn highlight_message(&mut self, id: i64, cx: &mut Context<Self>) {
        self.highlight_seq = self.highlight_seq.wrapping_add(1);
        let nonce = self.highlight_seq;
        self.highlighted_message = Some((id, nonce));
        cx.notify();
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(1800));
        cx.spawn(async move |this, cx| {
            timer.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.highlighted_message == Some((id, nonce)) {
                        s.highlighted_message = None;
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn open_search_popup(
        &mut self,
        pins: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.client.is_none() {
            return;
        }
        if pins
            && (self.in_conversation()
                || !self
                    .current_channel()
                    .is_some_and(|channel| channel.has_history()))
        {
            return;
        }
        let mode = if pins {
            search::Mode::Pins
        } else {
            search::Mode::Search
        };
        if self.in_conversation() || !self.search_permissions_ready {
            // A personal chat always re-reads before a query. A channel opens
            // with the confirmed snapshot when it has one; after a revoke or a
            // `403` the next opening starts the fresh read that was deferred
            // while the popup was closed.
            self.refresh_search_permissions(window, cx);
        } else {
            self.sync_search_permissions(cx);
        }
        let (server, channel) = (self.server, self.channel);
        if let Some(panel) = &self.search {
            panel.update(cx, |panel, cx| {
                panel.open(server, channel, mode, cx);
                panel.focus(window, cx);
            });
        }
        self.show_search = true;
        self.message_menu = None;
        self.forward = None;
        cx.notify();
    }

    /// Refresh the pin snapshot for the open popup. Pin changes reconcile the
    /// full list so an ACL change cannot leave a stale entry on screen.
    pub(super) fn refresh_pins(&mut self, cx: &mut Context<Self>) {
        if self.in_conversation() || self.channel <= 0 {
            self.pinned_ids.clear();
            return;
        }
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| panel.reload_pins(cx));
        }
    }
}
