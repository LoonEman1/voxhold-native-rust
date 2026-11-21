//! Cached stable row data for the virtualized history list.
//!
//! `render_history` runs on every frame, but GPUI asks the list closure for a
//! row only when it is measured or painted. This cache keeps the per-message
//! derivation (grouping, date/unread dividers, permissions, read marks, member
//! role and the row-local edit/delete state) out of the steady-state frame
//! path: it is rebuilt only when an input changes, and one `Arc<HistoryRow>`
//! is cloned per visible row instead of the whole `Message` vector.
//!
//! Avatars are deliberately not stored here. Their images are owned by the
//! bounded `members::Roster`, which may evict and `drop_image` a texture while
//! a cached row still referenced it; the render resolves the image for the
//! visible rows from the roster snapshot instead. `Member::avatar_version` is
//! still a cache input, so a changed version rebuilds the rows.

use super::*;
use crate::history;
use crate::message_tools::{self, MessagePermissions};

/// One prepared row. Stable data only: the render reads it as an `Arc`, so a
/// frame never clones the whole history.
pub(super) struct HistoryRow {
    pub(super) message: std::sync::Arc<Message>,
    pub(super) meta: history::RowMeta,
    pub(super) permissions: MessagePermissions,
    pub(super) read_by_other: bool,
    pub(super) own: bool,
    pub(super) role: String,
    pub(super) selected: bool,
    pub(super) pinned: bool,
    pub(super) highlighted: bool,
    pub(super) editing: bool,
    pub(super) delete_confirm: bool,
    pub(super) error: String,
}

/// A cheap identity stamp for one message. The body revision covers content,
/// replies, forwards and attachments; the reaction revision covers reactions;
/// `created_at`/author participate in grouping and the own-row mark.
#[derive(Clone, Copy)]
struct MessageStamp {
    id: i64,
    revision: i64,
    reactions_revision: i64,
    created_at: i64,
    author: i64,
    reply: bool,
    forward: bool,
}

impl MessageStamp {
    fn of(message: &Message) -> Self {
        Self {
            id: message.id,
            revision: message.revision,
            reactions_revision: message.reactions_revision,
            created_at: message.created_at,
            author: message.author.user_id,
            reply: message.reply.is_some(),
            forward: message.forward.is_some(),
        }
    }

    fn matches(&self, message: &Message) -> bool {
        self.id == message.id
            && self.revision == message.revision
            && self.reactions_revision == message.reactions_revision
            && self.created_at == message.created_at
            && self.author == message.author.user_id
            && self.reply == message.reply.is_some()
            && self.forward == message.forward.is_some()
    }
}

/// The channel fields `permissions_for`/`Channel::chat_*` read, so a channel
/// snapshot change rebuilds permissions without keeping the whole row.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ChannelScope {
    id: i64,
    permissions: i32,
    kind: u8,
    e2ee_required: bool,
    comments_enabled: bool,
    can_publish: bool,
}

impl ChannelScope {
    fn of(channel: Option<&Channel>) -> Option<Self> {
        channel.map(|channel| Self {
            id: channel.id,
            permissions: channel.permissions,
            kind: kind_code(&channel.kind),
            e2ee_required: channel.e2ee_required,
            comments_enabled: channel.comments_enabled,
            can_publish: channel.can_publish,
        })
    }
}

fn kind_code(kind: &str) -> u8 {
    match kind {
        "text" => 1,
        "news" => 2,
        "voice" => 3,
        _ => 0,
    }
}

/// Everything one frame actually needs from the view. Borrowed so the cache
/// check allocates nothing in the steady state.
pub(super) struct HistoryInput<'a> {
    pub(super) messages: &'a [Message],
    pub(super) channel_kind: &'a str,
    pub(super) unread_after: i64,
    pub(super) conversation: bool,
    pub(super) channel: Option<&'a Channel>,
    pub(super) self_user_id: i64,
    pub(super) role: &'a str,
    pub(super) reads: &'a std::collections::HashMap<i64, i64>,
    pub(super) members: &'a [Member],
    pub(super) selected: &'a [i64],
    pub(super) pinned: &'a std::collections::HashSet<i64>,
    pub(super) highlighted: Option<i64>,
    pub(super) editing: Option<i64>,
    pub(super) edit_open_seq: u64,
    pub(super) edit_error: Option<&'a (i64, u64, String)>,
    pub(super) action_error: Option<&'a (i64, String)>,
    pub(super) confirm_delete: Option<i64>,
    /// `(message_id, confirm_delete)` of the open context menu.
    pub(super) menu: Option<(i64, bool)>,
    pub(super) attachment_epoch: u64,
}

pub(super) struct HistoryRows {
    rows: std::sync::Arc<Vec<std::sync::Arc<HistoryRow>>>,
    keys: Vec<HistoryKey>,
    stamp: Vec<MessageStamp>,
    unread_after: i64,
    channel_kind: String,
    scope: Option<ChannelScope>,
    conversation: bool,
    self_user_id: i64,
    role: String,
    reads: Vec<(i64, i64)>,
    /// `(user_id, role, avatar_version)` in roster order.
    members: Vec<(i64, String, Option<String>)>,
    selected: Vec<i64>,
    pinned: Vec<i64>,
    highlighted: Option<i64>,
    editing: Option<i64>,
    edit_open_seq: u64,
    edit_error: Option<(i64, u64, String)>,
    action_error: Option<(i64, String)>,
    confirm_delete: Option<i64>,
    menu: Option<(i64, bool)>,
    attachment_epoch: u64,
    message_epoch: u64,
    changed_messages: std::collections::HashSet<i64>,
    /// Set by an explicit external reset (context replacement, chat switch,
    /// logout). Those paths clear `history_keys`; the next render must not
    /// reuse rows even when the identity stamps happen to match.
    dirty: bool,
    #[cfg(test)]
    rebuilds: u64,
}

impl Default for HistoryRows {
    fn default() -> Self {
        Self {
            rows: std::sync::Arc::new(Vec::new()),
            keys: Vec::new(),
            stamp: Vec::new(),
            unread_after: 0,
            channel_kind: String::new(),
            scope: None,
            conversation: false,
            self_user_id: 0,
            role: String::new(),
            reads: Vec::new(),
            members: Vec::new(),
            selected: Vec::new(),
            pinned: Vec::new(),
            highlighted: None,
            editing: None,
            edit_open_seq: 0,
            edit_error: None,
            action_error: None,
            confirm_delete: None,
            menu: None,
            attachment_epoch: 0,
            message_epoch: 0,
            changed_messages: Default::default(),
            dirty: false,
            #[cfg(test)]
            rebuilds: 0,
        }
    }
}

impl HistoryRows {
    /// Rebuild the rows when any input differs from the cached snapshot.
    /// Returns `true` when the caller has to splice the list.
    pub(super) fn prepare(&mut self, input: HistoryInput<'_>) -> bool {
        if !self.stale(&input) {
            return false;
        }
        // A changed attachment epoch means the body of one or more rows may
        // have changed without a revision bump (a server-side tombstone), so
        // the old `Arc<Message>` cannot be reused by identity alone.
        let refresh = self.dirty || self.attachment_epoch != input.attachment_epoch;
        self.rebuild(&input, refresh);
        true
    }

    pub(super) fn snapshot(&self) -> std::sync::Arc<Vec<std::sync::Arc<HistoryRow>>> {
        self.rows.clone()
    }

    pub(super) fn keys(&self) -> &[HistoryKey] {
        &self.keys
    }

    /// Force the next [`Self::prepare`] to rebuild. Used when the view cleared
    /// its key mirror on a message replacement that may keep the same stamps.
    pub(super) fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// A merge can replace data at the same server revisions. Compare the
    /// payload on the merge path so ordinary paints only check stamps.
    pub(super) fn note_message_merge(&mut self, message: &Message) {
        let Ok(index) = self
            .stamp
            .binary_search_by_key(&message.id, |stamp| stamp.id)
        else {
            return;
        };
        let cached = self.rows[index].message.as_ref();
        if cached.revision == message.revision
            && cached.reactions_revision == message.reactions_revision
            && cached != message
        {
            self.changed_messages.insert(message.id);
        }
    }

    /// Drop every prepared row and cache input. Used by the paths that clear or
    /// replace the confirmed messages (logout, session/chat switch, access
    /// reset): the cached rows hold `Arc<Message>` bodies, including private
    /// message text, and must not outlive the screen that owned them. The next
    /// [`Self::prepare`] always rebuilds, so a later chat cannot reuse a stale
    /// row even when its identity stamps happen to match.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
        self.dirty = true;
    }

    pub(super) fn len(&self) -> usize {
        self.keys.len()
    }

    #[cfg(test)]
    pub(super) fn row(&self, index: usize) -> Option<&std::sync::Arc<HistoryRow>> {
        self.rows.get(index)
    }

    #[cfg(test)]
    pub(super) fn rebuild_count(&self) -> u64 {
        self.rebuilds
    }

    fn stale(&self, input: &HistoryInput<'_>) -> bool {
        self.dirty
            || !self.changed_messages.is_empty()
            || self.stamp.len() != input.messages.len()
            || !self
                .stamp
                .iter()
                .zip(input.messages)
                .all(|(stamp, message)| stamp.matches(message))
            || self.unread_after != input.unread_after
            || self.channel_kind != input.channel_kind
            || self.scope != ChannelScope::of(input.channel)
            || self.conversation != input.conversation
            || self.self_user_id != input.self_user_id
            || self.role != input.role
            || !self.reads_match(input.reads)
            || !self.members_match(input.members)
            || self.selected.as_slice() != input.selected
            || !self.pinned_match(input.pinned)
            || self.highlighted != input.highlighted
            || self.editing != input.editing
            || self.edit_open_seq != input.edit_open_seq
            || self.edit_error.as_ref() != input.edit_error
            || self.action_error.as_ref() != input.action_error
            || self.confirm_delete != input.confirm_delete
            || self.menu != input.menu
            || self.attachment_epoch != input.attachment_epoch
    }

    fn reads_match(&self, reads: &std::collections::HashMap<i64, i64>) -> bool {
        self.reads.len() == reads.len()
            && self
                .reads
                .iter()
                .all(|(user, last)| reads.get(user) == Some(last))
    }

    fn pinned_match(&self, pinned: &std::collections::HashSet<i64>) -> bool {
        self.pinned.len() == pinned.len() && self.pinned.iter().all(|id| pinned.contains(id))
    }

    fn members_match(&self, members: &[Member]) -> bool {
        self.members.len() == members.len()
            && self
                .members
                .iter()
                .zip(members)
                .all(|((id, role, version), member)| {
                    *id == member.user_id
                        && role == &member.role
                        && version.as_deref() == member.avatar_version.as_deref()
                })
    }

    fn rebuild(&mut self, input: &HistoryInput<'_>, refresh: bool) {
        let meta = history::row_meta(input.messages, input.channel_kind, input.unread_after);
        let member_roles: std::collections::HashMap<i64, String> = input
            .members
            .iter()
            .map(|member| (member.user_id, member.role.clone()))
            .collect();
        let old = std::mem::replace(&mut self.rows, std::sync::Arc::new(Vec::new()));
        let old_rows = old.as_slice();
        let mut old_index = 0;
        let mut rows = Vec::with_capacity(input.messages.len());
        let mut keys = Vec::with_capacity(input.messages.len());
        if !self.changed_messages.is_empty() {
            self.message_epoch = self.message_epoch.wrapping_add(1);
        }
        for (message, meta) in input.messages.iter().zip(meta) {
            while old_index < old_rows.len() && old_rows[old_index].message.id < message.id {
                old_index += 1;
            }
            let old_row = old_rows
                .get(old_index)
                .filter(|row| row.message.id == message.id);
            let changed = self.changed_messages.contains(&message.id);
            let message_epoch = if changed {
                self.message_epoch
            } else {
                old_row.map_or(0, |_| self.keys[old_index].7)
            };
            let reused = old_row
                .filter(|_| !refresh && !changed && self.stamp[old_index].matches(message))
                .map(|row| row.message.clone());
            let message = reused.unwrap_or_else(|| std::sync::Arc::new(message.clone()));
            let own = input.self_user_id > 0 && input.self_user_id == message.author.user_id;
            let permissions = message_tools::permissions_for(
                input.self_user_id,
                input.role,
                input.channel,
                &message,
                input.conversation,
            );
            let read_by_other = input
                .reads
                .iter()
                .any(|(user, last)| *user != input.self_user_id && *last >= message.id);
            let selected = input.selected.contains(&message.id);
            let pinned = input.pinned.contains(&message.id);
            let highlighted = input.highlighted == Some(message.id);
            let editing = input.editing == Some(message.id);
            let delete_confirm = input.confirm_delete == Some(message.id)
                || input
                    .menu
                    .is_some_and(|(id, confirm)| id == message.id && confirm);
            let error = {
                let edit_error = input
                    .edit_error
                    .filter(|(row, open, _)| {
                        *row == message.id && *open == input.edit_open_seq && editing
                    })
                    .map(|(_, _, error)| error.clone())
                    .unwrap_or_default();
                if edit_error.is_empty() {
                    input
                        .action_error
                        .filter(|(id, _)| *id == message.id)
                        .map(|(_, error)| error.clone())
                        .unwrap_or_default()
                } else {
                    edit_error
                }
            };
            let role = member_roles
                .get(&message.author.user_id)
                .cloned()
                .unwrap_or_default();
            keys.push((
                message.id,
                message.revision,
                message.reactions_revision,
                meta.date.clone().unwrap_or_default(),
                meta.grouped,
                meta.unread,
                input.attachment_epoch,
                message_epoch,
            ));
            rows.push(std::sync::Arc::new(HistoryRow {
                message,
                meta,
                permissions,
                read_by_other,
                own,
                role,
                selected,
                pinned,
                highlighted,
                editing,
                delete_confirm,
                error,
            }));
        }
        self.stamp = input.messages.iter().map(MessageStamp::of).collect();
        self.keys = keys;
        self.rows = std::sync::Arc::new(rows);
        self.unread_after = input.unread_after;
        self.channel_kind = input.channel_kind.to_owned();
        self.scope = ChannelScope::of(input.channel);
        self.conversation = input.conversation;
        self.self_user_id = input.self_user_id;
        self.role = input.role.to_owned();
        self.reads = input
            .reads
            .iter()
            .map(|(user, last)| (*user, *last))
            .collect();
        self.reads.sort_unstable();
        self.members = input
            .members
            .iter()
            .map(|member| {
                (
                    member.user_id,
                    member.role.clone(),
                    member.avatar_version.clone(),
                )
            })
            .collect();
        self.selected = input.selected.to_vec();
        self.pinned = input.pinned.iter().copied().collect();
        self.pinned.sort_unstable();
        self.highlighted = input.highlighted;
        self.editing = input.editing;
        self.edit_open_seq = input.edit_open_seq;
        self.edit_error = input.edit_error.cloned();
        self.action_error = input.action_error.cloned();
        self.confirm_delete = input.confirm_delete;
        self.menu = input.menu;
        self.attachment_epoch = input.attachment_epoch;
        self.changed_messages.clear();
        self.dirty = false;
        #[cfg(test)]
        {
            self.rebuilds += 1;
        }
    }
}

impl Voxhold {
    /// Drop the prepared rows together with their key mirror. Every path that
    /// clears or replaces the confirmed messages calls this, so a signed-out
    /// account cannot keep cached row copies (message bodies included) alive
    /// until a history render that never comes, and a switched chat never
    /// receives a stale row. The cache also invalidates itself, so a future
    /// caller cannot skip the rebuild by keeping the same lengths.
    pub(super) fn reset_history_cache(&mut self) {
        self.history_keys.clear();
        self.history_rows.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn message(id: i64, author: i64, created_at: i64) -> Message {
        Message {
            id,
            channel_id: 7,
            content: format!("row {id}"),
            author: voxhold_api::Author {
                user_id: author,
                username: format!("user {author}"),
            },
            created_at,
            revision: 1,
            ..Default::default()
        }
    }

    struct Fixture {
        messages: Vec<Message>,
        channel: Channel,
        members: Vec<Member>,
        reads: HashMap<i64, i64>,
        selected: Vec<i64>,
        pinned: HashSet<i64>,
        highlighted: Option<i64>,
        editing: Option<i64>,
        edit_open_seq: u64,
        edit_error: Option<(i64, u64, String)>,
        action_error: Option<(i64, String)>,
        confirm_delete: Option<i64>,
        menu: Option<(i64, bool)>,
        unread_after: i64,
        self_user_id: i64,
        role: String,
        attachment_epoch: u64,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                messages: (1..=40).map(|id| message(id, 1, id * 10)).collect(),
                channel: Channel {
                    id: 7,
                    kind: "text".into(),
                    permissions: 3,
                    ..Default::default()
                },
                members: vec![Member {
                    user_id: 1,
                    username: "neva".into(),
                    role: "admin".into(),
                    avatar_version: Some("v1".into()),
                    ..Default::default()
                }],
                reads: HashMap::new(),
                selected: Vec::new(),
                pinned: HashSet::new(),
                highlighted: None,
                editing: None,
                edit_open_seq: 0,
                edit_error: None,
                action_error: None,
                confirm_delete: None,
                menu: None,
                unread_after: 0,
                self_user_id: 1,
                role: "member".into(),
                attachment_epoch: 0,
            }
        }

        fn input(&self) -> HistoryInput<'_> {
            HistoryInput {
                messages: &self.messages,
                channel_kind: &self.channel.kind,
                unread_after: self.unread_after,
                conversation: false,
                channel: Some(&self.channel),
                self_user_id: self.self_user_id,
                role: &self.role,
                reads: &self.reads,
                members: &self.members,
                selected: &self.selected,
                pinned: &self.pinned,
                highlighted: self.highlighted,
                editing: self.editing,
                edit_open_seq: self.edit_open_seq,
                edit_error: self.edit_error.as_ref(),
                action_error: self.action_error.as_ref(),
                confirm_delete: self.confirm_delete,
                menu: self.menu,
                attachment_epoch: self.attachment_epoch,
            }
        }
    }

    #[test]
    fn plain_repaint_reuses_rows_and_real_changes_rebuild_them() {
        let mut fixture = Fixture::new();
        let mut cache = HistoryRows::default();
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.rebuild_count(), 1);
        assert_eq!(cache.len(), 40);
        assert!(cache.row(0).unwrap().meta.date.is_some());
        assert!(cache.row(1).unwrap().meta.grouped);

        assert!(!cache.prepare(fixture.input()));
        assert_eq!(
            cache.rebuild_count(),
            1,
            "a plain repaint must not rebuild the prepared rows"
        );

        fixture.messages[1].content = "edited".into();
        fixture.messages[1].revision = 2;
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.rebuild_count(), 2);
        assert_eq!(cache.row(1).unwrap().message.content, "edited");
        assert_eq!(cache.row(1).unwrap().message.revision, 2);

        fixture.messages[2].reactions = vec![voxhold_api::Reaction {
            emoji: "🔥".into(),
            count: 2,
            me: true,
        }];
        fixture.messages[2].reactions_revision = 3;
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.rebuild_count(), 3);
        assert_eq!(cache.row(2).unwrap().message.reactions.len(), 1);

        fixture.unread_after = 5;
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.rebuild_count(), 4);
        assert!(cache.row(5).unwrap().meta.unread);
        assert!(
            !cache.row(6).unwrap().meta.unread,
            "the divider is drawn once"
        );

        fixture.channel.kind = "news".into();
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.rebuild_count(), 5);
        assert!(!cache.row(1).unwrap().meta.grouped, "news never groups");
    }

    #[test]
    fn row_state_follows_selection_reads_roles_edits_and_confirmations() {
        let mut fixture = Fixture::new();
        let mut cache = HistoryRows::default();
        assert!(cache.prepare(fixture.input()));

        fixture.selected.push(2);
        fixture.pinned.insert(3);
        assert!(cache.prepare(fixture.input()));
        assert!(cache.row(1).unwrap().selected);
        assert!(!cache.row(0).unwrap().selected);
        assert!(cache.row(2).unwrap().pinned);

        fixture.reads.insert(2, 10);
        assert!(cache.prepare(fixture.input()));
        assert!(cache.row(0).unwrap().read_by_other);

        fixture.members[0].role = "owner".into();
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.row(0).unwrap().role, "owner");

        fixture.editing = Some(4);
        fixture.edit_open_seq = 7;
        fixture.edit_error = Some((4, 7, "не отправлено".into()));
        assert!(cache.prepare(fixture.input()));
        assert!(cache.row(3).unwrap().editing);
        assert_eq!(cache.row(3).unwrap().error, "не отправлено");
        assert!(cache.row(0).unwrap().error.is_empty());

        // A stale editor generation does not paint its failure anywhere.
        fixture.edit_open_seq = 8;
        assert!(cache.prepare(fixture.input()));
        assert!(cache.row(3).unwrap().error.is_empty());

        fixture.confirm_delete = Some(5);
        assert!(cache.prepare(fixture.input()));
        assert!(cache.row(4).unwrap().delete_confirm);
        fixture.menu = Some((6, true));
        assert!(cache.prepare(fixture.input()));
        assert!(cache.row(5).unwrap().delete_confirm);

        fixture.action_error = Some((7, "Нет прав".into()));
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.row(6).unwrap().error, "Нет прав");

        fixture.attachment_epoch = 9;
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.keys()[0].6, 9);

        // A context replacement can change attachment tombstones without a
        // revision bump; its explicit key reset must still refresh the rows.
        fixture.messages[0]
            .attachments
            .push(voxhold_api::Attachment {
                id: "a".repeat(32),
                filename: "photo.png".into(),
                mime_type: "image/png".into(),
                deleted: true,
                ..Default::default()
            });
        assert!(
            !cache.prepare(fixture.input()),
            "the stamp alone is unchanged"
        );
        cache.invalidate();
        assert!(cache.prepare(fixture.input()));
        assert!(cache.row(0).unwrap().message.attachments[0].deleted);
    }

    #[test]
    fn an_attachment_epoch_refreshes_the_row_body_without_a_revision_bump() {
        let mut fixture = Fixture::new();
        let mut cache = HistoryRows::default();
        assert!(cache.prepare(fixture.input()));

        fixture.messages[0]
            .attachments
            .push(voxhold_api::Attachment {
                id: "a".repeat(32),
                filename: "photo.png".into(),
                mime_type: "image/png".into(),
                ..Default::default()
            });
        fixture.attachment_epoch = 1;
        assert!(cache.prepare(fixture.input()));
        assert_eq!(
            cache.row(0).unwrap().message.attachments.len(),
            1,
            "the new attachment is not hidden behind a reused body"
        );

        fixture.messages[0].attachments[0].deleted = true;
        fixture.attachment_epoch = 2;
        assert!(cache.prepare(fixture.input()));
        assert!(
            cache.row(0).unwrap().message.attachments[0].deleted,
            "a tombstone without a revision bump reaches the prepared row"
        );
        assert_eq!(cache.keys()[0].6, 2);
    }

    #[test]
    fn reset_drops_rows_and_the_next_prepare_rebuilds() {
        let mut fixture = Fixture::new();
        let mut cache = HistoryRows::default();
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.len(), 40);

        cache.reset();
        assert_eq!(cache.len(), 0);
        assert!(cache.row(0).is_none(), "no cached row survives the reset");

        // The reset is also an explicit invalidation: even the same input
        // rebuilds instead of leaving the cache empty or reusing the dropped
        // rows.
        assert!(cache.prepare(fixture.input()));
        assert_eq!(cache.len(), 40);
        assert_eq!(cache.row(0).unwrap().message.content, "row 1");
    }
}
