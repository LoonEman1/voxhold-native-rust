use super::*;
use crate::messages::{self, Notifications, PAGE_SIZE};

impl Voxhold {
    /// Enter the personal surface without discarding the channel context. The
    /// previous server/channel pair is restored by `return_to_channels` when
    /// that channel is still readable.
    pub(super) fn enter_personal_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.client.is_none() {
            return;
        }
        if self.personal_mode {
            self.messaging
                .query
                .update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
            return;
        }
        self.save_composer_draft(cx);
        self.personal_mode = true;
        self.personal_return = Some((self.server, self.channel));
        self.messaging.epoch = self.messaging.epoch.wrapping_add(1);
        self.messaging.clear_search();
        self.messaging.clear_leave();
        self.messaging.creator = None;
        self._creator_subscriptions.clear();
        self.messaging.notifications = None;
        self.messaging.older_error.clear();
        self.messaging.older_request = None;
        self.messaging.older_loading = false;
        self.messaging.open_request = None;
        self.conversations_reconcile = None;
        self.conversation = None;
        self.channel = 0;
        self.reset_personal_chat(window, cx);
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.show_members = false;
        self.refresh_recents(cx);
        self.messaging.query.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder("Имя или название беседы", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// "К каналам"/"Вернуться в сообщество". A revoked server or channel is
    /// never restored blindly: the fallback is the first readable channel of a
    /// still available server, otherwise the empty channel view.
    pub(super) fn return_to_channels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.personal_mode {
            return;
        }
        self.save_composer_draft(cx);
        self.personal_mode = false;
        self.messaging.epoch = self.messaging.epoch.wrapping_add(1);
        self.messaging.clear_search();
        self.messaging.clear_leave();
        self.messaging.creator = None;
        self._creator_subscriptions.clear();
        self.messaging.notifications = None;
        self.messaging.older_request = None;
        self.messaging.older_loading = false;
        self.messaging.older_error.clear();
        self.messaging.open_request = None;
        self.conversations_reconcile = None;
        self.conversation = None;
        self.channel = 0;
        self.reset_personal_chat(window, cx);
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        let (saved_server, saved_channel) = self.personal_return.take().unwrap_or((self.server, 0));
        if !self.servers.iter().any(|server| server.id == saved_server) {
            if let Some(first) = self.servers.first().map(|server| server.id) {
                self.select_server(first, window, cx);
            } else {
                cx.notify();
            }
            return;
        }
        if saved_server != self.server {
            self.select_server(saved_server, window, cx);
            return;
        }
        if saved_channel == 0 {
            // The user entered the personal surface without a channel; do not
            // invent one on the way back.
            cx.notify();
            return;
        }
        let allowed = |channel: &Channel| {
            channel.id == saved_channel && channel.readable() && !channel.e2ee_required
        };
        if self.channels.iter().any(allowed) {
            self.select_channel(saved_channel, window, cx);
            return;
        }
        let fallback = self
            .channels
            .iter()
            .find(|channel| channel.readable() && !channel.e2ee_required)
            .map(|channel| channel.id);
        if let Some(id) = fallback {
            self.select_channel(id, window, cx);
        } else if self.channels.is_empty() {
            self.select_server(saved_server, window, cx);
        } else {
            cx.notify();
        }
    }

    pub(super) fn save_composer_draft(&mut self, cx: &mut Context<Self>) {
        let key = self.chat_key();
        if key.channel != 0 {
            let text = self.composer.read(cx).value().to_string();
            self.drafts.borrow_mut().set_text(key, &text);
        }
    }

    /// Drop everything that describes the currently open chat, including
    /// in-flight answer slots, without touching the server or the index.
    pub(super) fn reset_personal_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_thread(window, cx);
        self.close_viewer(window);
        self.reset_personal_chat_state(cx);
        self.close_stale_settings(self.server, 0, window, cx);
        self.pending_personal_cleanup = false;
    }

    /// Window-dependent leftovers of a clear that ran inside a background
    /// answer. Called at the top of `render`.
    pub(super) fn finish_personal_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_personal_cleanup {
            return;
        }
        self.pending_personal_cleanup = false;
        self.close_thread(window, cx);
        self.close_viewer(window);
        self.close_stale_settings(self.server, 0, window, cx);
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// The window-independent part of `reset_personal_chat`. It is also used
    /// from a background answer where no `Window` is available; the next
    /// render finishes the viewer/thread/settings cleanup.
    pub(super) fn reset_personal_chat_state(&mut self, cx: &mut Context<Self>) {
        self.show_search = false;
        self.message_menu = None;
        self.member_menu = None;
        self.forward = None;
        self.editing = None;
        self.confirm_delete = None;
        self.action_requests.clear();
        self.selected_messages.clear();
        self.highlighted_message = None;
        self.hovered_message = 0;
        self.hovered_action = None;
        self.messages.clear();
        self.history_keys.clear();
        self.history_list = Self::new_history_list(ListAlignment::Bottom, cx);
        self.scrollbar_drag = None;
        self.before = None;
        self.has_more = false;
        self.history_has_newer = false;
        self.loading_older = false;
        self.older_request = 0;
        self.older_error.clear();
        self.older_retry_at = None;
        self.older_scheduled = false;
        self.read_scheduled = false;
        self.read_pending = false;
        self.read_retry_at = None;
        self.reads.clear();
        self.read_sent = 0;
        self.unread_after = 0;
        self.request_seq = self.request_seq.wrapping_add(1);
        self.refresh_request = None;
        self.refresh_again = false;
        self.refresh_new_ids.clear();
        self.context_request = None;
        self.pending_jump = None;
        self.history_reset_pending = false;
        self.pending_edits.clear();
        self.pending_reactions.clear();
        self.reaction_requests.clear();
        self.conversation_reaction_requests.clear();
        self.deleted_ids.clear();
        self.pending_anchor = None;
        self.access_generation = self.access_generation.wrapping_add(1);
        self.access_pending = false;
        self.access_reload_in_flight = false;
        self.access_refresh_again = false;
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
        self.channel_snapshot_request = None;
        self.search_permissions_request = None;
        self.search_permissions_ready = false;
        self.composer_error.clear();
        self.error.clear();
        self.pinned_ids.clear();
        self.pinned_request = None;
        self.show_channels = false;
        self.roster.reset_channel();
        self.roster.loading.clear();
        self.pending_personal_cleanup = true;
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| panel.set_scope(self.server, 0, cx));
        }
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |panel, cx| panel.set_scope(None, false, cx));
        }
        self.update_channel_subscription(false);
    }

    /// A leave, 403 or missing index row owns the next frame. Pending sends for
    /// this ChatKey are aborted and its draft/attachment scope is removed so a
    /// late history page or POST answer cannot repaint the conversation.
    pub(super) fn clear_active_conversation(
        &mut self,
        reason: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_active_conversation_state(reason, cx);
        self.finish_personal_cleanup(window, cx);
        cx.notify();
    }

    pub(super) fn clear_active_conversation_state(
        &mut self,
        reason: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(conversation) = self.conversation.take() else {
            return;
        };
        let key = draft::ChatKey::conversation(conversation.id);
        if let Some(pending) = self.pending_sends.remove(&key) {
            pending.abort.abort();
        }
        self.drafts.borrow_mut().clear(key);
        self.conversations_reconcile = None;
        self.messaging.notifications = None;
        self.messaging.clear_leave();
        self.reset_personal_chat_state(cx);
        self.channel = 0;
        if let Some(reason) = reason {
            self.error = reason.into();
        }
        cx.notify();
    }

    /// First index page. The popup/rail recents and the personal sidebar share
    /// this list, so a `conversation.changed` refresh cannot leave two views
    /// out of step.
    pub(super) fn refresh_recents(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.recents_request.is_some() {
            self.recents_again = true;
            return;
        }
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.recents_request = Some(request);
        self.messaging.loading = true;
        self.messaging.older_error.clear();
        let select_seq = self.conversation_select_seq;
        if let Some(inbox) = &self.inbox {
            inbox.update(cx, |inbox, cx| inbox.set_loading(true, cx));
        }
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime.block_on(client.conversations_page(PAGE_SIZE, None))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || s.recents_request != Some(request) {
                        return;
                    }
                    s.recents_request = None;
                    s.messaging.loading = false;
                    let mut retry_reconcile = false;
                    match result {
                        Ok(page) => {
                            let has_more = page.has_more;
                            messages::merge_first_page(
                                &mut s.conversations,
                                page.conversations,
                                has_more,
                            );
                            s.messaging.has_more = has_more;
                            s.messaging.cursor = page.next_cursor;
                            s.messaging.error.clear();
                            let reconcile = s.conversations_reconcile.take();
                            let active = s.conversation.as_ref().map(|row| row.id);
                            if let Some(id) = active {
                                if let Some(mut row) =
                                    s.conversations.iter().find(|row| row.id == id).cloned()
                                {
                                    if let Some(note) =
                                        s.messaging.notifications.as_ref().filter(|note| {
                                            note.id == id && (note.loaded || note.pending)
                                        })
                                    {
                                        row.muted = note.muted;
                                        if let Some(stored) =
                                            s.conversations.iter_mut().find(|row| row.id == id)
                                        {
                                            stored.muted = note.muted;
                                        }
                                    }
                                    s.conversation = Some(row);
                                } else if let Some(marker) = reconcile.filter(|marker| {
                                    s.personal_mode
                                        && marker.conversation == id
                                        && marker.session == session
                                        && marker.epoch == s.messaging.epoch
                                        && marker.select_seq == s.conversation_select_seq
                                }) {
                                    if s.conversation_select_seq > select_seq {
                                        // The page was requested before this
                                        // conversation was opened and cannot
                                        // prove anything about it. The next
                                        // page owns the reconciliation.
                                        s.conversations_reconcile = Some(marker);
                                        retry_reconcile = true;
                                    } else if has_more {
                                        // A partial page never proves deletion;
                                        // one explicit lookup decides instead.
                                        s.verify_conversation(marker, cx);
                                    } else {
                                        // A complete index proves the row is
                                        // gone. No `Window` is available in
                                        // this answer; the next render finishes
                                        // the viewer/composer cleanup.
                                        s.clear_active_conversation_state(
                                            Some("Диалог недоступен."),
                                            cx,
                                        );
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            let text = error.to_string();
                            if let Some(inbox) = &s.inbox {
                                inbox.update(cx, |inbox, cx| {
                                    inbox.set_loading(false, cx);
                                    inbox.set_error(text.clone(), cx);
                                });
                            }
                            s.messaging.error = text;
                        }
                    }
                    if std::mem::take(&mut s.recents_again) || retry_reconcile {
                        s.refresh_recents(cx);
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// A `conversation.changed` for an open row that a partial index cannot
    /// confirm: one explicit `GET /conversations/{id}` decides between a fresh
    /// row and a confirmed removal. The page itself is never treated as proof.
    fn verify_conversation(
        &mut self,
        marker: messages::ConversationReconcile,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.messaging.reconcile_request = Some(request);
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(client.conversation(marker.conversation))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.messaging.reconcile_request != Some(request) {
                        return;
                    }
                    s.messaging.reconcile_request = None;
                    let still_open = s.personal_mode
                        && marker.session == s.session_generation
                        && marker.epoch == s.messaging.epoch
                        && marker.select_seq == s.conversation_select_seq
                        && s.conversation.as_ref().map(|row| row.id) == Some(marker.conversation);
                    if !still_open {
                        return;
                    }
                    match result {
                        Ok(row) => {
                            if let Some(existing) = s
                                .conversations
                                .iter_mut()
                                .find(|current| current.id == row.id)
                            {
                                *existing = row.clone();
                            } else {
                                s.conversations.push(row.clone());
                                messages::sort_index(&mut s.conversations);
                            }
                            s.conversation = Some(row);
                        }
                        Err(voxhold_api::Error::Http(403 | 404)) => {
                            s.conversations.retain(|row| row.id != marker.conversation);
                            // No `Window` in this answer; the next render
                            // finishes the window-owned cleanup.
                            s.clear_active_conversation_state(Some("Диалог недоступен."), cx);
                        }
                        // A transient failure is not proof of deletion.
                        Err(_) => {}
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Append exactly one older page. A page requested before the personal
    /// surface closed is ignored by the epoch guard.
    /// Guard for one older index page. Closing/changing the personal surface
    /// bumps the epoch and clears the slot, so a page requested before the
    /// switch cannot append.
    pub(super) fn accepts_older_conversations(
        &self,
        request: u64,
        epoch: u64,
        session: u64,
    ) -> bool {
        self.personal_mode
            && self.session_generation == session
            && self.messaging.older_request == Some(request)
            && self.messaging.epoch == epoch
    }

    pub(super) fn load_older_conversations(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.personal_mode
            || self.recents_request.is_some()
            || self.messaging.older_loading
            || !self.messaging.has_more
            || self.messaging.cursor.is_none()
        {
            return;
        }
        let cursor = self.messaging.cursor.clone().unwrap_or_default();
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.messaging.older_request = Some(request);
        self.messaging.older_loading = true;
        self.messaging.older_error.clear();
        let epoch = self.messaging.epoch;
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(client.conversations_page(PAGE_SIZE, Some(&cursor)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if !s.accepts_older_conversations(request, epoch, session) {
                        return;
                    }
                    s.messaging.older_request = None;
                    s.messaging.older_loading = false;
                    match result {
                        Ok(page) => {
                            messages::append_page(&mut s.conversations, page.conversations);
                            // The first page may have been merged with a tail
                            // from older pages, so restore the backend order
                            // across the whole index after appending.
                            messages::sort_index(&mut s.conversations);
                            s.messaging.has_more = page.has_more;
                            s.messaging.cursor = page.next_cursor;
                        }
                        Err(error) => s.messaging.older_error = error.to_string(),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn toggle_messages_search_mode(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.messaging.global = !self.messaging.global;
        self.messaging.search.invalidate();
        let placeholder = if self.messaging.global {
            "Поиск по сообщениям"
        } else {
            "Имя или название беседы"
        };
        self.messaging.query.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder(placeholder, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn submit_messages_search(&mut self, cx: &mut Context<Self>) {
        let query = self.messaging.query.read(cx).value().trim().to_string();
        if query.is_empty() || query.chars().count() > 200 {
            self.messaging.search.request = self.messaging.search.request.wrapping_add(1);
            self.messaging.search.results.clear();
            self.messaging.search.has_more = false;
            self.messaging.search.next_before = None;
            self.messaging.search.loading = false;
            self.messaging.search.loading_more = false;
            self.messaging.search.submitted.clear();
            self.messaging.search.error = if query.is_empty() {
                "Введите запрос.".into()
            } else {
                "Запрос длиннее 200 символов.".into()
            };
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        let session = self.session_generation;
        {
            let search = &mut self.messaging.search;
            search.request = request;
            search.submitted = query.clone();
            search.results.clear();
            search.has_more = false;
            search.next_before = None;
            search.loading = true;
            search.loading_more = false;
            search.error.clear();
        }
        let runtime = self.runtime.clone();
        let task =
            cx.background_spawn(
                async move { runtime.block_on(client.search_conversations(&query, 0)) },
            );
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session
                        || s.messaging.search.request != request
                        || !s.messaging.global
                    {
                        return;
                    }
                    match result {
                        Ok(page) => s.messaging.search.apply_page(page, false),
                        Err(error) => {
                            s.messaging.search.loading = false;
                            s.messaging.search.error = error.to_string();
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn load_more_messages_search(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.messaging.search.loading
            || self.messaging.search.loading_more
            || !self.messaging.search.has_more
            || self.messaging.search.next_before.is_none()
        {
            return;
        }
        let before = self.messaging.search.next_before.unwrap_or(0);
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        let submitted = self.messaging.search.submitted.clone();
        self.messaging.search.request = request;
        self.messaging.search.loading_more = true;
        self.messaging.search.error.clear();
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(client.search_conversations(&submitted, before))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session
                        || s.messaging.search.request != request
                        || !s.messaging.global
                    {
                        return;
                    }
                    match result {
                        Ok(page) => s.messaging.search.apply_page(page, true),
                        Err(error) => {
                            s.messaging.search.loading_more = false;
                            s.messaging.search.error = error.to_string();
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// A search hit may point to a conversation outside the loaded page. Load
    /// that row first, then open a bounded window around the message.
    pub(super) fn open_conversation_message(
        &mut self,
        conversation: i64,
        message: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if conversation <= 0 || message <= 0 {
            return;
        }
        if let Some(row) = self
            .conversations
            .iter()
            .find(|row| row.id == conversation)
            .cloned()
        {
            self.select_conversation(row, window, cx);
            self.jump_to_conversation_message(conversation, message, window, cx);
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.messaging.open_request = Some(request);
        self.messaging.search.error.clear();
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let task =
            cx.background_spawn(async move { runtime.block_on(client.conversation(conversation)) });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session
                    || s.messaging.open_request != Some(request)
                    || !s.personal_mode
                {
                    return;
                }
                s.messaging.open_request = None;
                match result {
                    Ok(row) if row.saved => {
                        // `/me/conversations/search` also returns saved
                        // favorites. Favorites are W08b, so never offer such a
                        // hit as a normal chat again in this result set.
                        s.messaging
                            .search
                            .results
                            .retain(|hit| hit.conversation_id != row.id);
                        s.messaging.search.error =
                            "Избранные сообщения откроются в разделе «Избранное».".into();
                    }
                    Ok(row) => {
                        if let Some(existing) = s
                            .conversations
                            .iter_mut()
                            .find(|current| current.id == row.id)
                        {
                            *existing = row.clone();
                        } else {
                            s.conversations.insert(0, row.clone());
                        }
                        s.select_conversation(row, window, cx);
                        s.jump_to_conversation_message(conversation, message, window, cx);
                    }
                    Err(voxhold_api::Error::Http(403 | 404)) => {
                        s.messaging.search.error = "Диалог недоступен.".into();
                    }
                    Err(error) => s.messaging.search.error = error.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn jump_to_conversation_message(
        &mut self,
        conversation: i64,
        message: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.in_conversation() || self.channel != conversation {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        // A jump owns the next window; the initial latest-page refresh started
        // by `select_conversation` must not replace it afterwards.
        self.refresh_request = None;
        self.refresh_again = false;
        self.refresh_new_ids.clear();
        self.older_request = 0;
        self.loading_older = false;
        self.context_request = None;
        self.request_seq = self.request_seq.wrapping_add(1);
        let request = self.request_seq;
        self.refresh_request = Some(request);
        let (generation, access, chat) = (self.generation, self.access_generation, self.chat_key());
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let before = message.saturating_add(1);
        let task = cx.background_spawn(async move {
            runtime.block_on(client.conversation_messages(conversation, before))
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session
                    || s.generation != generation
                    || s.access_generation != access
                    || s.refresh_request != Some(request)
                    || s.chat_key() != chat
                    || s.channel != conversation
                {
                    return;
                }
                s.refresh_request = None;
                match result {
                    Ok(page) => {
                        let found = page.messages.iter().any(|row| row.id == message);
                        s.messages = page.messages;
                        s.messages.sort_by_key(|row| row.id);
                        s.history_keys.clear();
                        s.history_list.reset(0);
                        s.before = page.pagination.next_before_id;
                        s.has_more = page.pagination.has_more;
                        s.history_has_newer = true;
                        s.pending_anchor = None;
                        s.older_request = 0;
                        s.loading_older = false;
                        s.older_error.clear();
                        s.older_retry_at = None;
                        s.pending_jump = Some(message);
                        if found {
                            s.highlight_message(message, cx);
                        } else {
                            s.error = "Сообщение недоступно.".into();
                        }
                    }
                    Err(voxhold_api::Error::Http(403 | 404)) => {
                        s.clear_active_conversation(Some("Диалог недоступен."), window, cx);
                    }
                    Err(error) => s.error = error.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_conversation_notifications(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.messaging.notifications = Some(Notifications {
            id: conversation.id,
            muted: conversation.muted,
            loaded: false,
            pending: false,
            error: String::new(),
            request,
        });
        let id = conversation.id;
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let task =
            cx.background_spawn(
                async move { runtime.block_on(client.conversation_notification(id)) },
            );
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    let Some(state) = s
                        .messaging
                        .notifications
                        .as_mut()
                        .filter(|state| state.id == id && state.request == request)
                    else {
                        return;
                    };
                    if s.session_generation != session
                        || s.conversation.as_ref().map(|row| row.id) != Some(id)
                    {
                        return;
                    }
                    match result {
                        Ok(value) => {
                            state.loaded = true;
                            state.muted = value.muted;
                            state.error.clear();
                            s.apply_conversation_muted(id, value.muted);
                        }
                        Err(error) => state.error = error.to_string(),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// PUT only after the GET confirmed the current mode. A failed write keeps
    /// the last confirmed value and shows the error next to the action.
    pub(super) fn toggle_conversation_mute(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let (id, next, request) = {
            let Some(state) = self.messaging.notifications.as_mut() else {
                return;
            };
            if !state.loaded || state.pending {
                return;
            }
            state.pending = true;
            state.error.clear();
            state.request = state.request.wrapping_add(1);
            (state.id, !state.muted, state.request)
        };
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(client.set_conversation_notification(id, next))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    let Some(state) = s
                        .messaging
                        .notifications
                        .as_mut()
                        .filter(|state| state.id == id && state.request == request)
                    else {
                        return;
                    };
                    if s.session_generation != session {
                        return;
                    }
                    state.pending = false;
                    match result {
                        Ok(value) => {
                            state.loaded = true;
                            state.muted = value.muted;
                            state.error.clear();
                            s.apply_conversation_muted(id, value.muted);
                        }
                        Err(error) => state.error = error.to_string(),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// One click on the header mute action. A failed GET leaves `loaded=false`;
    /// the click then retries that GET instead of staying disabled forever.
    pub(super) fn activate_conversation_mute(&mut self, cx: &mut Context<Self>) {
        match self.messaging.notifications.as_ref() {
            Some(note) if note.pending => {}
            Some(note) if note.loaded => self.toggle_conversation_mute(cx),
            Some(_) => self.load_conversation_notifications(cx),
            None => self.load_conversation_notifications(cx),
        }
    }

    pub(super) fn apply_conversation_muted(&mut self, id: i64, muted: bool) {
        if let Some(row) = self.conversations.iter_mut().find(|row| row.id == id) {
            row.muted = muted;
        }
        if let Some(row) = self.conversation.as_mut().filter(|row| row.id == id) {
            row.muted = muted;
        }
    }

    /// `conversation.settings` is authoritative for the current account.
    pub(super) fn apply_conversation_settings(&mut self, id: i64, muted: bool) {
        self.apply_conversation_muted(id, muted);
        if let Some(state) = self
            .messaging
            .notifications
            .as_mut()
            .filter(|state| state.id == id && !state.pending)
        {
            state.loaded = true;
            state.muted = muted;
            state.error.clear();
            state.request = state.request.wrapping_add(1);
        }
    }

    /// A message by another account increments the badge and moves the row to
    /// the front, like `Community.tsx`; the active history window is untouched.
    pub(super) fn note_conversation_message(&mut self, message: &Message) {
        let id = message.conversation_id;
        if id <= 0 {
            return;
        }
        let mine = message.author.user_id == self.self_user_id;
        if let Some(position) = self.conversations.iter().position(|row| row.id == id) {
            let mut row = self.conversations.remove(position);
            row.last_message_id = row.last_message_id.max(message.id);
            if !mine {
                row.unread_count = row.unread_count.saturating_add(1);
            }
            self.conversations.insert(0, row);
        }
    }

    pub(super) fn note_conversation_read(&mut self, id: i64) {
        if let Some(row) = self.conversations.iter_mut().find(|row| row.id == id) {
            row.unread_count = 0;
        }
    }

    pub(super) fn request_leave_conversation(&mut self, cx: &mut Context<Self>) {
        // Product decision for W08a: only a group has "leave"; a direct (1:1)
        // conversation has no leave action, matching the web contract.
        if self
            .conversation
            .as_ref()
            .is_none_or(|conversation| conversation.direct)
        {
            return;
        }
        self.messaging.leave_confirm = true;
        self.messaging.leave_error.clear();
        cx.notify();
    }

    pub(super) fn cancel_leave_conversation(&mut self, cx: &mut Context<Self>) {
        self.messaging.clear_leave();
        cx.notify();
    }

    pub(super) fn confirm_leave_conversation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        if conversation.direct || self.messaging.leave_request.is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        self.messaging.leave_request = Some(request);
        self.messaging.leave_error.clear();
        let id = conversation.id;
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let task =
            cx.background_spawn(async move { runtime.block_on(client.leave_conversation(id)) });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || s.messaging.leave_request != Some(request) {
                    return;
                }
                s.messaging.leave_request = None;
                match result {
                    Ok(()) => {
                        // An older page requested before the DELETE must not
                        // put the left row back into the index.
                        s.messaging.epoch = s.messaging.epoch.wrapping_add(1);
                        s.messaging.older_request = None;
                        s.messaging.older_loading = false;
                        s.conversations.retain(|row| row.id != id);
                        s.messaging.clear_leave();
                        if s.conversation.as_ref().map(|row| row.id) == Some(id) {
                            s.clear_active_conversation(Some("Вы покинули беседу."), window, cx);
                        }
                        s.refresh_recents(cx);
                    }
                    Err(error) => {
                        s.messaging.leave_error = error.to_string();
                        s.messaging.leave_confirm = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
