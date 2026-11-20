//! Bounded replay set for rows that arrive while a page replaces a gapped
//! window.
//!
//! A loaded window that ends before the live tail has an unloaded range after
//! its right edge. Message ids are not contiguous, so one fresh row cannot
//! prove that the range is closed: it is kept out of the visible window. When a
//! refresh owns the next window — "К последним" (reset) or the ordinary
//! reconnect/resync refresh started while `history_has_newer` is set — the row
//! is held here and replayed into the confirmed page instead of into the old
//! fragment. The set is bound to one request id and one chat, so a late answer
//! or an event of another chat can never import it. On overflow the rows are
//! dropped and the normal refresh repeat re-reads the latest page.

use super::*;
use crate::history::merge_revision;

/// Rows received after a refresh took its request slot, before its answer.
#[derive(Debug)]
pub(super) struct LatestBuffer {
    request: u64,
    chat: draft::ChatKey,
    rows: Vec<Message>,
    /// The bounded set overflowed and the normal refresh repeat must re-read
    /// the latest page instead of replaying this buffer.
    overflowed: bool,
}

impl LatestBuffer {
    /// Keeps the replay set in the same order of magnitude as the visible
    /// window.
    const MAX_ROWS: usize = 1000;
}

impl Voxhold {
    /// Open the bounded replay set for a refresh that will replace the window.
    /// Any refresh started while the window is separated from the tail binds
    /// one, not only "К последним": an ordinary resync also replaces the
    /// fragment with the latest page and must replay the rows it raced. Only
    /// rows of this exact request and chat are replayed, so a late answer of
    /// another scope cannot import them.
    pub(super) fn bind_latest_buffer(&mut self, request: u64) {
        if !self.history_reset_pending && !self.history_has_newer {
            return;
        }
        let chat = self.chat_key();
        let rows = match self.latest_buffer.take() {
            Some(buffer) if buffer.request == request && buffer.chat == chat => buffer.rows,
            _ => Vec::new(),
        };
        self.latest_buffer = Some(LatestBuffer {
            request,
            chat,
            rows,
            overflowed: false,
        });
    }

    /// Take the replay set that belongs to this answer. A set of another
    /// request or chat can no longer reach its own answer, so it is dropped
    /// instead of keeping up to `MAX_ROWS` rows alive until the next cleanup.
    pub(super) fn take_latest_buffer(&mut self, request: u64) -> Vec<Message> {
        let chat = self.chat_key();
        match self.latest_buffer.take() {
            Some(buffer) if buffer.request == request && buffer.chat == chat => buffer.rows,
            _ => Vec::new(),
        }
    }

    pub(super) fn clear_latest_buffer(&mut self) {
        self.latest_buffer = None;
    }

    /// Hold one created row that arrived behind the known gap of the window an
    /// in-flight refresh is about to replace. Any other request or chat drops
    /// the row. A full buffer is cleared and the normal refresh repeat re-reads
    /// the latest page.
    pub(super) fn buffer_behind_gap(&mut self, message: Message) {
        let chat = self.chat_key();
        let Some(request) = self.refresh_request else {
            return;
        };
        let Some(buffer) = self.latest_buffer.as_mut() else {
            return;
        };
        if buffer.request != request || buffer.chat != chat || buffer.overflowed {
            return;
        }
        let id = message.id;
        match buffer.rows.binary_search_by_key(&id, |row| row.id) {
            Ok(index) => merge_revision(&mut buffer.rows[index], message),
            Err(_) if buffer.rows.len() >= LatestBuffer::MAX_ROWS => {
                buffer.rows.clear();
                buffer.overflowed = true;
                self.refresh_again = true;
            }
            Err(index) => buffer.rows.insert(index, message),
        }
    }

    /// Combine the confirmed latest page with the replay set using the same
    /// revision and tombstone rules as every other history path, so each id
    /// appears exactly once and a deleted row cannot return.
    pub(super) fn merge_latest_page(
        &self,
        messages: Vec<Message>,
        buffered: Vec<Message>,
    ) -> Vec<Message> {
        let mut combined: std::collections::BTreeMap<i64, Message> =
            messages.into_iter().map(|row| (row.id, row)).collect();
        for row in buffered {
            if self.deleted_ids.contains(&row.id) {
                continue;
            }
            match combined.entry(row.id) {
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    merge_revision(slot.get_mut(), row);
                }
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(row);
                }
            }
        }
        combined
            .into_values()
            .filter(|row| !self.deleted_ids.contains(&row.id))
            .collect()
    }

    /// Does the loaded conversation page end before the tail that the index
    /// records in `last_message_id`? A page that already reaches it, an empty
    /// page or a missing index row is not a gap: numeric id differences alone
    /// prove nothing.
    pub(super) fn conversation_has_newer_tail(&self, conversation: i64) -> bool {
        let tail = self
            .conversations
            .iter()
            .find(|row| row.id == conversation)
            .or_else(|| {
                self.conversation
                    .as_ref()
                    .filter(|row| row.id == conversation)
            })
            .map(|row| row.last_message_id)
            .unwrap_or(0);
        let last_loaded = self.messages.last().map(|row| row.id).unwrap_or(0);
        last_loaded > 0 && tail > last_loaded
    }

    /// Repeat a "К последним" click that arrived while a conversation jump held
    /// the request slot. The jump owns that answer, so the deferred reset can
    /// only start after it; a dialogue closed by a 403/404 stays closed.
    pub(super) fn retry_refresh_after_jump(
        &mut self,
        chat: draft::ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let refresh_again = std::mem::take(&mut self.refresh_again);
        if refresh_again && self.conversation.is_some() && self.chat_key() == chat {
            self.refresh_history(window, cx);
        }
    }
}
