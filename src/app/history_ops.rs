use super::*;
use crate::history::{apply_reactions, merge_revision};

impl Voxhold {
    pub(super) fn schedule_visible_read(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.read_scheduled = true;
        let scope = (
            self.chat_key(),
            self.server,
            self.channel,
            self.access_generation,
            self.chat_scope_epoch,
        );
        let painted = self.painted_read.clone();
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            if let Some(this) = weak.upgrade() {
                this.update(cx, |s, cx| {
                    if (
                        s.chat_key(),
                        s.server,
                        s.channel,
                        s.access_generation,
                        s.chat_scope_epoch,
                    ) != scope
                    {
                        return;
                    }
                    s.read_scheduled = false;
                    s.mark_visible_read(painted.get(), window, cx);
                });
            }
        });
    }

    /// Deferred auto-load for the top of history. Never touches `ListState` from
    /// the scroll callback itself.
    pub(super) fn schedule_older_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.older_scheduled = true;
        let scope = (
            self.chat_key(),
            self.server,
            self.channel,
            self.access_generation,
        );
        cx.spawn_in(window, async move |this, window| {
            window
                .background_executor()
                .timer(std::time::Duration::from_millis(1))
                .await;
            let _ = this.update_in(window, |s, window, cx| {
                if (s.chat_key(), s.server, s.channel, s.access_generation) != scope {
                    return;
                }
                s.older_scheduled = false;
                if s.loading_older || !s.has_more {
                    return;
                }
                if s.history_list.logical_scroll_top().item_ix <= 3 {
                    s.load_older(window, cx);
                }
            });
        })
        .detach();
    }

    /// Access changed for the current scope: drop confirmed data, invalidate
    /// in-flight requests and take a fresh channel snapshot before restoring the chat.
    /// Visibility gate for read marks. `SW_HIDE` leaves
    /// `Window::is_window_active` on its last `WM_ACTIVATE` value, so the
    /// tracked flag is the only reliable signal while the window is in the
    /// tray. Kept separate so a test can prove the gate without a network
    /// client or a loaded history page.
    pub(super) fn can_mark_visible_read(&self) -> bool {
        let conversation = self.in_conversation();
        self.window_active
            && !self.read_pending
            && !self.history_has_newer
            && !(self.access_pending && !conversation)
            && !self
                .read_retry_at
                .is_some_and(|at| at > std::time::Instant::now())
            && (conversation || self.channel_chat_readable(self.channel))
    }

    pub(super) fn mark_visible_read(
        &mut self,
        message_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let conversation = self.in_conversation();
        if !self.can_mark_visible_read() {
            return;
        }
        if message_id <= self.read_sent
            || self
                .messages
                .binary_search_by_key(&message_id, |message| message.id)
                .is_err()
        {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let (server, channel, generation, access_generation) = (
            self.server,
            self.channel,
            self.generation,
            self.access_generation,
        );
        let chat = self.chat_key();
        let runtime = self.runtime.clone();
        self.read_pending = true;
        let task = cx.background_spawn(async move {
            if conversation {
                runtime.block_on(client.mark_conversation_read(channel, message_id))
            } else {
                runtime.block_on(client.mark_read(server, channel, message_id))
            }
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.generation != generation
                    || s.access_generation != access_generation
                    || s.chat_key() != chat
                {
                    return;
                }
                s.read_pending = false;
                match result {
                    Ok(_) => {
                        s.read_sent = s.read_sent.max(message_id);
                        if !conversation {
                            let known = s.channel_reads.entry((s.server, s.channel)).or_default();
                            *known = (*known).max(message_id);
                        } else {
                            s.refresh_recents(cx);
                        }
                    }
                    Err(voxhold_api::Error::Http(403 | 404)) if conversation => {
                        // A revoked or deleted conversation owns no active
                        // ChatKey: leave the composer clean and let the index
                        // remove the row instead of showing a writable window.
                        s.clear_active_conversation(Some("Диалог недоступен."), window, cx);
                        s.refresh_recents(cx);
                    }
                    Err(voxhold_api::Error::Http(403 | 404)) => {
                        s.invalidate_access(window, cx);
                    }
                    Err(error) => {
                        let delay = Self::history_retry_delay(&error);
                        s.read_retry_at = Some(std::time::Instant::now() + delay);
                        let timer = cx.background_executor().timer(delay);
                        cx.spawn_in(window, async move |this, window| {
                            timer.await;
                            let _ = this.update_in(window, |s, _, cx| {
                                if s.access_generation == access_generation {
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Toggle a reaction from a row chip. One in-flight request per message and
    /// emoji; the confirmed update is applied through the same revision rule as
    /// HTTP history and realtime events.
    pub(super) fn toggle_reaction(
        &mut self,
        message_id: i64,
        emoji: String,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        let conversation = self.in_conversation();
        if (self.access_pending && !conversation)
            || (!conversation && !self.channel_chat_readable(self.channel))
        {
            return;
        }
        let Some(message) = self.message_snapshot(message_id) else {
            return;
        };
        if !self.permissions_for_row(&message).react {
            self.action_error = Some((message_id, "Недостаточно прав доступа.".into()));
            cx.notify();
            return;
        }
        let key = (message_id, emoji.clone());
        if !self.reaction_requests.insert(key.clone()) {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.reaction_requests.remove(&key);
            return;
        };
        let (server, channel, generation, access) = (
            self.server,
            self.channel,
            self.generation,
            self.access_generation,
        );
        let chat = self.chat_key();
        let runtime = self.runtime.clone();
        let recent = self.recent_reactions.clone();
        let task = cx.background_spawn(async move {
            if conversation {
                runtime
                    .block_on(
                        client.set_conversation_reaction(channel, message_id, &emoji, !active),
                    )
                    .map(|answer| voxhold_api::ReactionsUpdate {
                        channel_id: 0,
                        message_id,
                        revision: answer.revision,
                        reactions: answer.reactions,
                    })
            } else {
                runtime.block_on(client.set_reaction(server, channel, message_id, &emoji, !active))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    s.reaction_requests.remove(&key);
                    let failure = result.as_ref().err().and_then(|error| error.status());
                    let in_scope = s.generation == generation
                        && s.access_generation == access
                        && s.chat_key() == chat
                        && s.server == server
                        && s.channel == channel;
                    if conversation && matches!(failure, Some(403 | 404)) {
                        // A conversation `403` is ambiguous like a send: one
                        // explicit GET decides. A `404` removes the row at once.
                        if failure == Some(403) {
                            s.verify_conversation_after_403(chat, None, cx);
                        } else {
                            s.note_conversation_unavailable(chat.channel, cx);
                        }
                    }
                    if !in_scope {
                        cx.notify();
                        return;
                    }
                    match result {
                        Ok(update) => {
                            s.apply_reaction_update(update);
                            if !active {
                                reactions::touch_recent(&mut recent.borrow_mut(), &key.1);
                            }
                            s.error.clear();
                            s.action_error = None;
                        }
                        Err(error) => {
                            s.action_error = Some((message_id, error.to_string()));
                            if !conversation && failure == Some(403) {
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

    /// Focus an already loaded row or open a bounded `context` page around a
    /// quoted message that is outside the current window. A comment target
    /// opens its root thread instead of inserting the comment into the main
    /// timeline.
    pub(super) fn jump_to_message(
        &mut self,
        message_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self
            .messages
            .iter()
            .position(|message| message.id == message_id)
        {
            self.history_list.scroll_to(gpui::ListOffset {
                item_ix: index,
                offset_in_item: px(0.),
            });
            self.pending_jump = None;
            self.highlight_message(message_id, cx);
            cx.notify();
            return;
        }
        if self.context_request.is_some() || self.access_pending || self.in_conversation() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let (server, channel, generation, access) = (
            self.server,
            self.channel,
            self.generation,
            self.access_generation,
        );
        // The context answer owns the next window. Invalidate a refresh that
        // started with the channel switch so it cannot replace the context.
        self.refresh_request = None;
        self.refresh_again = false;
        self.refresh_new_ids.clear();
        self.clear_latest_buffer();
        self.request_seq = self.request_seq.wrapping_add(1);
        let request = self.request_seq;
        self.context_request = Some(request);
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let context = client
                    .message_context(server, channel, message_id, 25, 25)
                    .await?;
                let root = context
                    .messages
                    .get(context.target_index)
                    .and_then(|message| message.thread_root_id)
                    .filter(|root| *root > 0);
                let root_context = match root {
                    Some(root) => Some(client.message_context(server, channel, root, 1, 1).await?),
                    None => None,
                };
                Ok::<_, voxhold_api::Error>((context, root_context))
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if !s.accepts_context_reply(request, generation, access, server, channel) {
                    return;
                }
                s.context_request = None;
                match result {
                    Ok((_context, Some(root_context))) => {
                        let root = root_context
                            .messages
                            .get(root_context.target_index)
                            .cloned();
                        let Some(root) = root else {
                            s.error = "Сообщение недоступно".into();
                            cx.notify();
                            return;
                        };
                        s.open_thread(root, message_id, window, cx);
                    }
                    Ok((context, None)) => {
                        if s.apply_context_snapshot(context) {
                            s.highlight_message(message_id, cx);
                        }
                    }
                    Err(voxhold_api::Error::Http(404)) => {
                        s.error = "Сообщение недоступно".into();
                    }
                    Err(error) => s.error = error.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Merge a bounded context page into the current window with the same
    /// revision/tombstone rules as history and realtime events. `false` means
    /// the target was deleted while the request was in flight; the old window
    /// is kept and an unavailable message is shown.
    pub(super) fn apply_context_snapshot(&mut self, context: voxhold_api::MessageContext) -> bool {
        let target = context.target_message_id;
        if self.deleted_ids.contains(&target) {
            self.error = "Сообщение недоступно".into();
            return false;
        }
        let context_len = context.messages.len();
        let target_index = context.target_index;
        let current: std::collections::HashMap<i64, Message> = self
            .messages
            .iter()
            .cloned()
            .map(|message| (message.id, message))
            .collect();
        let mut merged = Vec::with_capacity(context_len);
        let mut applied = Vec::new();
        for mut message in context.messages {
            if message.channel_id != self.channel
                || (message.thread_root_id.is_some() && message.id != target)
                || self.deleted_ids.contains(&message.id)
            {
                continue;
            }
            if let Some(existing) = current.get(&message.id).cloned() {
                let mut existing = existing;
                merge_revision(&mut existing, message);
                message = existing;
            }
            if let Some(edit) = self.pending_edits.get(&message.id).cloned() {
                merge_revision(&mut message, edit);
            }
            if let Some(update) = self.pending_reactions.get(&message.id).cloned() {
                apply_reactions(&mut message, update);
            }
            crate::history::apply_attachment_tombstones(&mut message, &self.media.purged);
            applied.push(message.id);
            merged.push(message);
        }
        if !applied.contains(&target) {
            self.error = "Сообщение недоступно".into();
            return false;
        }
        merged.sort_by_key(|message| message.id);
        for id in &applied {
            self.pending_edits.remove(id);
            self.pending_reactions.remove(id);
        }
        self.messages = merged;
        self.reset_history_cache();
        self.history_list.reset(0);
        self.before = self.messages.first().map(|message| message.id);
        self.has_more = target_index >= 25;
        self.history_has_newer = context_len.saturating_sub(target_index + 1) >= 25;
        self.pending_anchor = None;
        self.pending_jump = Some(target);
        // The old page belongs to the replaced window; invalidate its pending
        // cursor and its retry state before the next `load_older`.
        self.older_request = 0;
        self.loading_older = false;
        self.older_error.clear();
        self.older_retry_at = None;
        self.older_scheduled = false;
        self.error.clear();
        true
    }

    /// A deletion marker rolled out of the bounded set, so a server answer
    /// requested before the deletion may still describe that attachment as
    /// live. Drop the in-flight history, older-page and `/context` answers and
    /// their request slots; later requests start after the deletion. Returns
    /// whether a history refresh was cancelled and must be restarted once the
    /// whole event batch has been applied.
    pub(super) fn invalidate_history_requests(&mut self) -> bool {
        let refresh_was_active = self.refresh_request.is_some();
        self.refresh_request = None;
        self.refresh_again = false;
        self.refresh_new_ids.clear();
        self.clear_latest_buffer();
        self.older_request = 0;
        self.loading_older = false;
        self.older_error.clear();
        self.older_retry_at = None;
        self.older_scheduled = false;
        self.context_request = None;
        refresh_was_active
    }

    /// The guard applied to a `messages_page` answer. Extracted so a context
    /// replacement can be tested against a late older-page response.
    pub(super) fn accepts_older_reply(
        &self,
        request: u64,
        before: i64,
        server: i64,
        channel: i64,
        generation: u64,
        access: u64,
    ) -> bool {
        self.generation == generation
            && self.access_generation == access
            && self.older_request == request
            && self.before == Some(before)
            && self.server == server
            && self.channel == channel
    }

    pub(super) fn accepts_context_reply(
        &self,
        request: u64,
        generation: u64,
        access: u64,
        server: i64,
        channel: i64,
    ) -> bool {
        self.context_request == Some(request)
            && self.generation == generation
            && self.access_generation == access
            && self.server == server
            && self.channel == channel
    }

    /// "К последним" owns the next window. A late `/context` answer must not
    /// reopen the older window after this reset.
    pub(super) fn reset_for_latest(&mut self) {
        self.context_request = None;
        self.pending_jump = None;
        self.history_reset_pending = true;
    }

    /// Common reaction confirmation used by row chips and the open picker.
    /// Applies the revision rule to the loaded row, or stores a bounded pending
    /// update when that row is outside the window.
    pub(super) fn apply_reaction_update(&mut self, update: voxhold_api::ReactionsUpdate) {
        let in_scope = if self.in_conversation() {
            update.channel_id == 0
                && self
                    .messages
                    .iter()
                    .any(|message| message.id == update.message_id)
        } else {
            update.channel_id == self.channel
        };
        if !in_scope || self.deleted_ids.contains(&update.message_id) || update.revision <= 0 {
            return;
        }
        if let Some(message) = self
            .messages
            .iter_mut()
            .find(|message| message.id == update.message_id)
        {
            apply_reactions(message, update);
        } else if !self.in_conversation()
            && (self.pending_reactions.len() < 128
                || self.pending_reactions.contains_key(&update.message_id))
        {
            let entry = self
                .pending_reactions
                .entry(update.message_id)
                .or_insert_with(|| update.clone());
            if update.revision > entry.revision {
                *entry = update;
            }
        }
    }

    /// Reaction-only refresh for a loaded personal message. The backend uses
    /// a strict `id < before_id`, so `mid + 1` makes the reacted row the first
    /// answer. The current window, cursor and scroll anchor stay untouched.
    pub(super) fn refresh_conversation_reaction(
        &mut self,
        conversation_id: i64,
        message_id: i64,
        cx: &mut Context<Self>,
    ) {
        if !self.in_conversation()
            || self.channel != conversation_id
            || !self.messages.iter().any(|row| row.id == message_id)
            || self.deleted_ids.contains(&message_id)
        {
            return;
        }
        let chat = self.chat_key();
        let key = (chat, message_id);
        if let Some(active) = self.conversation_reaction_requests.get_mut(&key) {
            // A second event owns one repeat after the active answer. It must
            // not start a parallel GET or free the first request's slot.
            active.again = true;
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.conversation_reaction_requests.insert(
            key,
            ConversationReactionRequest {
                request,
                again: false,
            },
        );
        let (generation, access) = (self.generation, self.access_generation);
        let runtime = self.runtime.clone();
        let before = message_id.saturating_add(1);
        let task = runtime
            .spawn(async move { client.conversation_messages(conversation_id, before).await });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(_) => Err(voxhold_api::Error::Network),
            };
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    let Some(active) = s.conversation_reaction_requests.get_mut(&key) else {
                        return;
                    };
                    if active.request != request {
                        // A late callback may only free the slot it owns.
                        return;
                    }
                    let again = std::mem::take(&mut active.again);
                    s.conversation_reaction_requests.remove(&key);
                    if s.generation != generation
                        || s.access_generation != access
                        || s.chat_key() != chat
                        || s.channel != conversation_id
                        || !s.in_conversation()
                    {
                        return;
                    }
                    if let Ok(page) = result {
                        let Some(fetched) = page.messages.iter().find(|row| row.id == message_id)
                        else {
                            return;
                        };
                        if s.deleted_ids.contains(&message_id) {
                            return;
                        }
                        let update = voxhold_api::ReactionsUpdate {
                            channel_id: 0,
                            message_id,
                            revision: fetched.reactions_revision,
                            reactions: fetched.reactions.clone(),
                        };
                        if let Some(row) = s.messages.iter_mut().find(|row| row.id == message_id) {
                            apply_reactions(row, update);
                        }
                    }
                    if again {
                        s.refresh_conversation_reaction(conversation_id, message_id, cx);
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Push the confirmed row state into the open thread and context menu. The
    /// old side panel no longer owns message state.
    pub(super) fn sync_open_views(&mut self, cx: &mut Context<Self>) {
        if let Some(thread) = &self.thread {
            thread.update(cx, |thread, cx| {
                thread.sync_from_history(&self.messages, cx)
            });
        }
    }

    pub(super) fn history_anchor(&self) -> Option<(i64, gpui::Pixels)> {
        let offset = self.history_list.logical_scroll_top();
        self.history_keys
            .get(offset.item_ix)
            .map(|(id, ..)| (*id, offset.offset_in_item))
    }

    pub(super) fn history_retry_delay(error: &voxhold_api::Error) -> std::time::Duration {
        match error {
            voxhold_api::Error::Http(429) => std::time::Duration::from_secs(20),
            voxhold_api::Error::Http(status) if (500..600).contains(status) => {
                std::time::Duration::from_secs(8)
            }
            _ => std::time::Duration::from_secs(3),
        }
    }

    pub(super) fn schedule_retry_label(
        &self,
        window: &mut Window,
        access: u64,
        request: u64,
        cx: &mut Context<Self>,
    ) {
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(1));
        cx.spawn_in(window, async move |this, window| {
            timer.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.access_generation != access || s.older_request != request {
                    return;
                }
                if s.older_retry_at
                    .is_some_and(|at| at > std::time::Instant::now())
                {
                    s.schedule_retry_label(window, access, request, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn refresh_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let conversation = self.in_conversation();
        let (server, channel, generation, access) = (
            self.server,
            self.channel,
            self.generation,
            self.access_generation,
        );
        let chat = self.chat_key();
        if (self.access_pending && !conversation)
            || (!conversation && !self.channel_chat_readable(channel))
        {
            return;
        }
        if self.refresh_request.is_some() {
            // One reset already asked the running page to replace the window;
            // from now on its rows also belong to that page's replay set.
            self.refresh_again = true;
            if let Some(request) = self.refresh_request {
                self.bind_latest_buffer(request);
            }
            return;
        }
        self.request_seq = self.request_seq.wrapping_add(1);
        let request = self.request_seq;
        self.refresh_request = Some(request);
        self.refresh_new_ids.clear();
        self.bind_latest_buffer(request);
        self.older_request = 0;
        self.loading_older = false;
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            if conversation {
                runtime.block_on(client.conversation_messages(channel, 0))
            } else {
                runtime.block_on(client.messages_page(server, channel, 0))
            }
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if !s.accepts_refresh_reply(request, generation, access, chat, server, channel) {
                    return;
                }
                s.refresh_request = None;
                // The replay set belongs to this answer only. A failed or
                // superseded answer drops it; the confirmed page then either
                // consumes it or the normal repeat asks again.
                let buffered = s.take_latest_buffer(request);
                let refresh_again = std::mem::take(&mut s.refresh_again);
                let new_ids = std::mem::take(&mut s.refresh_new_ids);
                match result {
                    Ok(page) => {
                        let return_to_latest = std::mem::take(&mut s.history_reset_pending);
                        s.pending_anchor = if return_to_latest {
                            None
                        } else {
                            s.history_anchor()
                        };
                        if return_to_latest {
                            s.messages.clear();
                            s.reset_history_cache();
                            s.history_list.reset(0);
                            s.before = None;
                            // The confirmed latest page is the new right edge.
                            // Merge the rows that arrived while it loaded with
                            // the same revision/tombstone rules as every other
                            // history path, so each id appears once.
                            let messages = s.merge_latest_page(page.messages, buffered);
                            s.apply_history_snapshot(
                                voxhold_api::MessagePage {
                                    messages,
                                    pagination: page.pagination,
                                },
                                new_ids,
                            );
                            s.pending_jump = s.messages.last().map(|message| message.id);
                        } else {
                            // An ordinary refresh started at a known gap also
                            // replaces the fragment with the latest page.
                            // Replay the rows it held for us, otherwise the
                            // race would leave a hole that later rows cannot
                            // fill. Without a gap the page is passed untouched:
                            // `refresh_new_ids` still carries the in-flight
                            // rows that are newer than the page.
                            let page = if buffered.is_empty() {
                                page
                            } else {
                                voxhold_api::MessagePage {
                                    messages: s.merge_latest_page(page.messages, buffered),
                                    pagination: page.pagination,
                                }
                            };
                            s.apply_history_snapshot(page, new_ids);
                        }
                        s.sync_open_views(cx);
                        s.older_error.clear();
                        s.older_retry_at = None;
                        s.error.clear();
                    }
                    Err(error) => match error {
                        voxhold_api::Error::Http(403) | voxhold_api::Error::Http(404)
                            if conversation =>
                        {
                            // A revoked or deleted conversation owns no
                            // ChatKey: clear pending sends/drafts and let the
                            // index answer reconcile the missing row.
                            s.clear_active_conversation(Some("Диалог недоступен."), window, cx);
                            s.refresh_recents(cx);
                        }
                        voxhold_api::Error::Http(403) | voxhold_api::Error::Http(404) => {
                            s.invalidate_access(window, cx);
                        }
                        other => s.error = other.to_string(),
                    },
                }
                if refresh_again && !s.access_pending {
                    s.refresh_history(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn apply_history_snapshot(
        &mut self,
        page: voxhold_api::MessagePage,
        new_ids: std::collections::HashSet<i64>,
    ) {
        let mut current: std::collections::HashMap<_, _> = std::mem::take(&mut self.messages)
            .into_iter()
            .map(|row| (row.id, row))
            .collect();
        let last = page.messages.iter().map(|m| m.id).max().unwrap_or(0);
        self.before = page.pagination.next_before_id;
        self.has_more = page.pagination.has_more;
        self.history_has_newer = false;
        for mut row in page.messages {
            if let Some(mut existing) = current.remove(&row.id) {
                merge_revision(&mut existing, row);
                row = existing;
            }
            self.merge_message(true, row);
        }
        for row in current
            .into_values()
            .filter(|row| row.id > last && new_ids.contains(&row.id))
        {
            self.merge_message(true, row);
        }
        self.trim_history(false);
    }

    pub(super) fn trim_history(&mut self, prepending: bool) {
        const MAX_ROWS: usize = 1000;
        if self.messages.len() <= MAX_ROWS {
            return;
        }
        let excess = self.messages.len() - MAX_ROWS;
        let keep_from = if prepending || self.history_has_newer {
            0
        } else {
            self.history_anchor()
                .and_then(|(id, _)| self.messages.binary_search_by_key(&id, |row| row.id).ok())
                .map_or(excess, |anchor| excess.min(anchor))
        };
        self.history_has_newer |= keep_from < excess;
        self.messages.truncate(keep_from + MAX_ROWS);
        if keep_from != 0 {
            self.messages.drain(0..keep_from);
            self.before = self.messages.first().map(|row| row.id);
            self.has_more = true;
        }
    }

    /// Guard for one history refresh answer. A clear/leave/403 bumps the
    /// generation and request slot, so its late answer cannot repaint.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn accepts_refresh_reply(
        &self,
        request: u64,
        generation: u64,
        access: u64,
        chat: draft::ChatKey,
        server: i64,
        channel: i64,
    ) -> bool {
        self.refresh_request == Some(request)
            && self.generation == generation
            && self.access_generation == access
            && self.chat_key() == chat
            && self.server == server
            && self.channel == channel
    }

    pub(super) fn load_older(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading_older
            || !self.has_more
            || (self.access_pending && !self.in_conversation())
            || self.refresh_request.is_some()
        {
            return;
        }
        if let Some(at) = self.older_retry_at {
            if std::time::Instant::now() < at {
                return;
            }
        }
        let (Some(client), Some(before)) = (self.client.clone(), self.before) else {
            return;
        };
        let conversation = self.in_conversation();
        let (server, channel, generation, access) = (
            self.server,
            self.channel,
            self.generation,
            self.access_generation,
        );
        let chat = self.chat_key();
        self.request_seq = self.request_seq.wrapping_add(1);
        let request = self.request_seq;
        self.older_request = request;
        self.loading_older = true;
        self.older_error.clear();
        cx.notify();
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            if conversation {
                runtime.block_on(client.conversation_messages(channel, before))
            } else {
                runtime.block_on(client.messages_page(server, channel, before))
            }
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if !s.accepts_older_reply(request, before, server, channel, generation, access)
                    || s.chat_key() != chat
                {
                    return;
                }
                s.loading_older = false;
                match result {
                    Ok(page) => {
                        // The anchor is captured when the page is applied,
                        // not when it was requested, so a reader who scrolled
                        // away keeps the new position.
                        let anchor = s.history_anchor();
                        s.before = page.pagination.next_before_id;
                        s.has_more = page.pagination.has_more;
                        for row in page.messages {
                            s.merge_message(true, row);
                        }
                        s.trim_history(true);
                        s.sync_open_views(cx);
                        s.older_retry_at = None;
                        s.pending_anchor = anchor;
                    }
                    Err(voxhold_api::Error::Http(403) | voxhold_api::Error::Http(404))
                        if conversation =>
                    {
                        s.clear_active_conversation(Some("Диалог недоступен."), window, cx);
                        s.refresh_recents(cx);
                    }
                    Err(voxhold_api::Error::Http(403) | voxhold_api::Error::Http(404)) => {
                        s.invalidate_access(window, cx);
                    }
                    Err(error) => {
                        s.older_retry_at =
                            Some(std::time::Instant::now() + Self::history_retry_delay(&error));
                        s.older_error = error.to_string();
                        s.schedule_retry_label(window, access, request, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
