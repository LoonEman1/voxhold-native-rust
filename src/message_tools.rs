//! Action model for a message row, ported from Go `history_actions.go`,
//! `history_selection.go`, `popup.go::openMessageMenu` and the frontend
//! `MessageContextMenu.tsx`.
//!
//! The UI reads this model twice: when it paints hover buttons and when the
//! user clicks the context menu. Permissions are always derived from the
//! current message/channel/role snapshot, and the dispatcher re-reads the row
//! before a request starts.

use voxhold_api::{Channel, Message};

/// Quick reactions shown in the hover bar, in Go order.
pub const HOVER_REACTIONS: [&str; 3] = ["🤣", "❤️", "👍"];

/// Permissions derived from the selected channel, effective server role and
/// current user. Defaults deny every operation until the caller has
/// established those facts.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct MessagePermissions {
    pub reply: bool,
    pub edit: bool,
    pub delete: bool,
    pub react: bool,
    pub pin: bool,
    pub forward: bool,
    pub save: bool,
    pub comments: bool,
    pub block: bool,
}

/// One hover-bar entry. Reactions carry the exact Unicode sequence so two
/// rows never share state even when their content is identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverAction {
    Quick(&'static str),
    Picker,
    Reply,
    Forward,
    More,
}

impl HoverAction {
    pub fn key(self) -> &'static str {
        match self {
            Self::Quick(emoji) => emoji,
            Self::Picker => "emoji",
            Self::Reply => "reply",
            Self::Forward => "forward",
            Self::More => "more",
        }
    }
}

/// One context-menu entry. `Pin`/`Unpin` are resolved from the current pin
/// snapshot at paint time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Discuss,
    Reply,
    Edit,
    Pin,
    Unpin,
    Copy,
    Save,
    Forward,
    Delete,
    Block,
}

impl Action {
    pub fn id(self) -> &'static str {
        match self {
            Self::Discuss => "discuss",
            Self::Reply => "reply",
            Self::Edit => "edit",
            Self::Pin => "pin",
            Self::Unpin => "unpin",
            Self::Copy => "copy",
            Self::Save => "save",
            Self::Forward => "forward",
            Self::Delete => "delete",
            Self::Block => "block",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Discuss => "Обсуждение",
            Self::Reply => "Ответить",
            Self::Edit => "Изменить",
            Self::Pin => "Закрепить",
            Self::Unpin => "Открепить",
            Self::Copy => "Копировать текст",
            Self::Save => "В избранное",
            Self::Forward => "Переслать",
            Self::Delete => "Удалить",
            Self::Block => "Заблокировать",
        }
    }
}

/// Go `history_actions.go::drawActions`: the thread row only exposes "more".
pub fn hover_actions(
    permissions: MessagePermissions,
    conversation: bool,
    in_thread: bool,
) -> Vec<HoverAction> {
    if in_thread {
        return vec![HoverAction::More];
    }
    let mut buttons: Vec<HoverAction> = HOVER_REACTIONS
        .into_iter()
        .map(HoverAction::Quick)
        .collect();
    if permissions.react {
        buttons.push(HoverAction::Picker);
    }
    if !conversation {
        if permissions.reply {
            buttons.push(HoverAction::Reply);
        }
        if permissions.forward {
            buttons.push(HoverAction::Forward);
        }
    }
    buttons.push(HoverAction::More);
    buttons
}

/// Frontend `MessageContextMenu` order, with Go's discuss label.
pub fn menu_actions(
    permissions: MessagePermissions,
    pinned: bool,
    conversation: bool,
    self_author: bool,
) -> Vec<Action> {
    let mut actions = Vec::new();
    if permissions.comments {
        actions.push(Action::Discuss);
    }
    if permissions.reply {
        actions.push(Action::Reply);
    }
    if permissions.edit {
        actions.push(Action::Edit);
    }
    if permissions.pin {
        actions.push(if pinned { Action::Unpin } else { Action::Pin });
    }
    actions.push(Action::Copy);
    if permissions.save {
        actions.push(Action::Save);
    }
    if permissions.forward {
        actions.push(Action::Forward);
    }
    if permissions.delete {
        actions.push(Action::Delete);
    }
    if permissions.block && conversation && !self_author {
        actions.push(Action::Block);
    }
    actions
}

/// Derive the available operations from the current snapshot. `role` is the
/// effective server role, `channel` is the loaded channel row, and
/// `conversation` selects the personal-message rules.
pub fn permissions_for(
    self_user_id: i64,
    role: &str,
    channel: Option<&Channel>,
    message: &Message,
    conversation: bool,
) -> MessagePermissions {
    let own = self_user_id > 0 && self_user_id == message.author.user_id;
    if conversation {
        // Backend `conversations.go`: reactions, forward and favorites are
        // available; edit/reply/pin/threads are not part of the direct API.
        return MessagePermissions {
            reply: false,
            edit: false,
            delete: false,
            react: true,
            pin: false,
            forward: true,
            save: true,
            comments: false,
            block: !own && message.author.user_id > 0,
        };
    }
    let Some(channel) = channel else {
        return MessagePermissions::default();
    };
    let readable = channel.chat_readable();
    let writable = channel.chat_writable();
    let news = channel.kind == "news";
    let moderate = role == "owner" || role == "admin" || channel.permissions & 16 != 0;
    MessagePermissions {
        reply: readable && writable && !news,
        edit: readable && own && message.forward.is_none() && (!news || writable),
        delete: readable && (own || moderate),
        react: readable,
        pin: readable && moderate,
        forward: readable,
        save: readable,
        comments: readable
            && message.thread_root_id.is_none()
            && (channel.kind == "text" || channel.comments_enabled || message.comment_count > 0),
        block: false,
    }
}

/// Go `clipboardText`: selected rows in ascending id order, empty bodies
/// skipped, one newline between items and no trailing newline.
pub fn copy_text<'a>(messages: impl IntoIterator<Item = &'a Message>) -> String {
    let mut rows: Vec<&Message> = messages.into_iter().collect();
    rows.sort_by_key(|message| message.id);
    rows.into_iter()
        .map(|message| message.content.as_str())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The quote target is owned by the row, not by the quote text. Two rows can
/// carry the same author/excerpt and still jump to different ids.
pub fn quote_target(message: &Message) -> Option<i64> {
    message
        .reply
        .as_ref()
        .filter(|reply| !reply.deleted && reply.message_id > 0)
        .map(|reply| reply.message_id)
}

/// Go `dispatchHistoryAction`: map the hover entry to the same action handlers
/// the context menu uses.
pub fn hover_menu_action(action: HoverAction) -> Option<Action> {
    match action {
        HoverAction::Reply => Some(Action::Reply),
        HoverAction::Forward => Some(Action::Forward),
        HoverAction::More | HoverAction::Picker | HoverAction::Quick(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxhold_api::{Author, MessageReference};

    fn channel(kind: &str, permissions: i32) -> Channel {
        Channel {
            id: 7,
            kind: kind.into(),
            permissions,
            ..Default::default()
        }
    }

    fn message(id: i64, author: i64) -> Message {
        Message {
            id,
            channel_id: 7,
            content: format!("row {id}"),
            author: Author {
                user_id: author,
                username: "neva".into(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn channel_permissions_match_go_and_the_web_client() {
        let text = channel("text", 3);
        let own = message(10, 5);
        let other = message(11, 6);
        let owner = permissions_for(5, "member", Some(&text), &own, false);
        assert!(owner.reply && owner.edit && owner.delete && owner.react && owner.forward);
        assert!(!owner.pin, "a member cannot pin");
        assert!(owner.comments);

        let admin = permissions_for(5, "admin", Some(&text), &other, false);
        assert!(admin.pin && admin.delete, "moderation covers foreign rows");
        assert!(!admin.edit, "only the author edits");

        let reader = permissions_for(5, "member", Some(&channel("text", 1)), &other, false);
        assert!(!reader.reply && !reader.delete && reader.react);
        assert!(reader.forward);

        let news = channel("news", 3);
        let reader = permissions_for(5, "member", Some(&news), &own, false);
        assert!(!reader.reply, "news replies go through comments");
        assert!(!reader.edit, "a reader cannot edit news");
        assert!(
            !reader.comments,
            "discussion is hidden when the news channel disabled comments"
        );
        let mut comments_on = channel("news", 3);
        comments_on.comments_enabled = true;
        assert!(
            permissions_for(5, "member", Some(&comments_on), &own, false).comments,
            "an enabled news discussion is offered"
        );

        let publisher = channel("news", 3);
        let mut publisher = publisher;
        publisher.can_publish = true;
        let author = permissions_for(5, "member", Some(&publisher), &own, false);
        assert!(author.edit, "a publisher edits an own news post");
    }

    #[test]
    fn forwarded_rows_cannot_be_edited() {
        let mut own = message(10, 5);
        own.forward = Some(MessageReference::default());
        let permissions = permissions_for(5, "owner", Some(&channel("text", 3)), &own, false);
        assert!(!permissions.edit);
        assert!(permissions.delete);
    }

    #[test]
    fn conversation_permissions_follow_the_direct_api() {
        let own = message(10, 5);
        let permissions = permissions_for(5, "owner", None, &own, true);
        assert!(permissions.react && permissions.forward && permissions.save);
        assert!(!permissions.reply && !permissions.edit && !permissions.pin);
        assert!(!permissions.comments && !permissions.block);

        let foreign = permissions_for(5, "owner", None, &message(11, 6), true);
        assert!(foreign.block, "a personal row can block its author");

        let actions = menu_actions(foreign, false, true, false);
        assert_eq!(
            actions,
            [Action::Copy, Action::Save, Action::Forward, Action::Block]
        );
    }

    #[test]
    fn menu_order_matches_the_frontend() {
        let permissions = MessagePermissions {
            reply: true,
            edit: true,
            delete: true,
            react: true,
            pin: true,
            forward: true,
            save: true,
            comments: true,
            block: false,
        };
        let actions = menu_actions(permissions, true, false, true);
        assert_eq!(
            actions,
            [
                Action::Discuss,
                Action::Reply,
                Action::Edit,
                Action::Unpin,
                Action::Copy,
                Action::Save,
                Action::Forward,
                Action::Delete,
            ]
        );
    }

    #[test]
    fn hover_bar_matches_go_and_hides_reply_in_news() {
        let permissions = MessagePermissions {
            reply: false,
            react: true,
            forward: true,
            ..Default::default()
        };
        let buttons = hover_actions(permissions, false, false);
        assert_eq!(
            buttons,
            [
                HoverAction::Quick("🤣"),
                HoverAction::Quick("❤️"),
                HoverAction::Quick("👍"),
                HoverAction::Picker,
                HoverAction::Forward,
                HoverAction::More,
            ]
        );
        assert_eq!(
            hover_actions(permissions, true, false),
            [
                HoverAction::Quick("🤣"),
                HoverAction::Quick("❤️"),
                HoverAction::Quick("👍"),
                HoverAction::Picker,
                HoverAction::More,
            ]
        );
        assert_eq!(hover_actions(permissions, false, true), [HoverAction::More]);
    }

    #[test]
    fn copied_rows_keep_ascending_order_and_skip_empty_bodies() {
        let mut first = message(9, 1);
        first.content = "первый".into();
        let mut second = message(4, 1);
        second.content = "второй".into();
        let mut empty = message(7, 1);
        empty.content = String::new();
        assert_eq!(
            copy_text([&first, &second, &empty]),
            "второй\nпервый",
            "clipboard order follows ids, not click order"
        );
    }

    #[test]
    fn identical_quotes_keep_their_own_target() {
        let mut first = message(10, 1);
        let mut second = message(11, 2);
        first.reply = Some(MessageReference {
            message_id: 3,
            author: Author {
                user_id: 9,
                username: "same".into(),
            },
            content: "same text".into(),
            deleted: false,
        });
        second.reply = Some(MessageReference {
            message_id: 4,
            author: Author {
                user_id: 9,
                username: "same".into(),
            },
            content: "same text".into(),
            deleted: false,
        });
        assert_eq!(quote_target(&first), Some(3));
        assert_eq!(quote_target(&second), Some(4));
        assert_eq!((first.id, second.id), (10, 11));
    }
}
