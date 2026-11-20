use super::*;
use crate::inbox::{conversation_peer, conversation_title, ConversationTile};

impl Voxhold {
    /// Build the popup rows, including the peer avatar requested from the
    /// shared bounded cache when the server member list exposes a version.
    pub(super) fn inbox_tiles(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<ConversationTile> {
        let self_user_id = self.self_user_id;
        let mut wanted = Vec::new();
        let mut tiles = Vec::with_capacity(self.conversations.len());
        for conversation in &self.conversations {
            let title = conversation_title(conversation, self_user_id);
            let peer_id = conversation_peer(conversation, self_user_id);
            let version = self
                .members
                .iter()
                .find(|member| member.user_id == peer_id)
                .and_then(|member| member.avatar_version.clone());
            if let Some(version) = version.as_ref().filter(|version| !version.is_empty()) {
                wanted.push((peer_id, version.clone()));
            }
            tiles.push(ConversationTile {
                id: conversation.id,
                title,
                direct: conversation.direct,
                unread: conversation.unread_count,
                muted: conversation.muted,
                peer_id,
                avatar: None,
            });
        }
        for (user_id, version) in wanted {
            self.request_avatar(user_id, version, window, cx);
        }
        for tile in &mut tiles {
            if tile.peer_id <= 0 {
                continue;
            }
            let version = self
                .members
                .iter()
                .find(|member| member.user_id == tile.peer_id)
                .and_then(|member| member.avatar_version.clone());
            let Some(version) = version else {
                continue;
            };
            if let Some((image, used)) = self
                .roster
                .avatars
                .get(&(tile.peer_id, version.clone()))
                .cloned()
            {
                if let Some((_, used)) = self.roster.avatars.get_mut(&(tile.peer_id, version)) {
                    *used = std::time::Instant::now();
                }
                let _ = used;
                tile.avatar = Some(image);
            }
        }
        tiles
    }

    /// Open a personal conversation in the same workspace. The numeric channel
    /// id may equal the conversation id, so every request/guard is keyed by
    /// `ChatKey` and the conversation flag, never by the number alone.
    pub(super) fn select_conversation(
        &mut self,
        conversation: voxhold_api::Conversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A pending channel ACL reload does not gate a personal conversation:
        // its history, drafts and key are independent of channel access.
        if conversation.id <= 0 || self.client.is_none() {
            return;
        }
        let previous = self.chat_key();
        if previous.channel != 0 {
            let text = self.composer.read(cx).value().to_string();
            self.drafts.borrow_mut().set_text(previous, &text);
        }
        self.close_viewer(window);
        self.close_thread(window, cx);
        self.show_search = false;
        self.show_inbox = false;
        self.show_channels = false;
        self.messaging.clear_leave();
        self.messaging.notifications = None;
        self.messaging.creator = None;
        self._creator_subscriptions.clear();
        self.message_menu = None;
        self.forward = None;
        self.editing = None;
        self.confirm_delete = None;
        self.action_error = None;
        self.action_notice.clear();
        self.action_requests.clear();
        self.selected_messages.clear();
        self.highlighted_message = None;
        self.hovered_message = 0;
        self.hovered_action = None;
        let id = conversation.id;
        self.conversation_select_seq = self.conversation_select_seq.wrapping_add(1);
        self.conversation = Some(conversation);
        self.channel = id;
        let key = self.chat_key();
        let draft = self.drafts.borrow().text(key);
        self.composer
            .update(cx, |input, cx| input.set_value(draft, window, cx));
        self.composer_error.clear();
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |attachments, cx| {
                attachments.set_scope(Some(key), true, cx)
            });
        }
        self.reads.clear();
        self.read_sent = 0;
        self.read_pending = false;
        self.read_retry_at = None;
        self.unread_after = 0;
        self.reaction_requests.clear();
        self.conversation_reaction_requests.clear();
        self.channel_snapshot_request = None;
        self.search_permissions_request = None;
        self.search_permissions_ready = false;
        self.context_request = None;
        self.pending_jump = None;
        self.history_reset_pending = false;
        self.deleted_ids.clear();
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
        self.close_stale_settings(self.server, 0, window, cx);
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| panel.set_scope(self.server, 0, cx));
        }
        self.messages.clear();
        self.history_keys.clear();
        self.history_list = Self::new_history_list(ListAlignment::Bottom, cx);
        self.scrollbar_drag = None;
        self.before = None;
        self.has_more = false;
        self.history_has_newer = false;
        self.loading_older = false;
        self.generation += 1;
        self.error.clear();
        self.update_channel_subscription(false);
        self.refresh_history(window, cx);
        self.load_conversation_notifications(cx);
        cx.notify();
    }
}
