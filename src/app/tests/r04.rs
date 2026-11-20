//! R04 regressions: message action scope, stale hover/query answers, threads,
//! personal conversations and pin/reaction revisions. Kept in a separate file
//! for the 1000-line limit.

use super::super::*;
use super::{add_auth_window, message_json};
use crate::search;
use crate::thread_panel::ThreadPanel;
use gpui::{TestAppContext, VisualTestContext};

fn dto(mut value: serde_json::Value) -> Message {
    serde_json::from_value(value.take()).unwrap()
}

fn conversation(id: i64) -> voxhold_api::Conversation {
    voxhold_api::Conversation {
        id,
        direct: true,
        ..Default::default()
    }
}

/// A request started in a channel must not paint after switching to a
/// conversation with the same numeric id. The action token stores `ChatKey`,
/// not the bare id.
#[gpui::test]
async fn action_and_history_scope_is_keyed_by_chat_not_numeric_id(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.messages.push(dto(message_json(5, 1, "channel row")));
            let request = v.begin_action(5, "edit").expect("action");
            assert!(v.accepts_action(5, "edit", &request));
            assert_eq!(v.chat_key(), draft::ChatKey::channel(1, 7));

            // Same number, different scope.
            v.conversation = Some(conversation(7));
            assert_eq!(v.chat_key(), draft::ChatKey::conversation(7));
            assert!(
                !v.accepts_action(5, "edit", &request),
                "a channel answer must not land on the personal screen"
            );

            v.conversation = None;
            assert!(
                v.accepts_action(5, "edit", &request),
                "returning to the same channel restores only its own token"
            );
            v.server = 2;
            v.access_generation = v.access_generation.wrapping_add(1);
            assert!(
                !v.accepts_action(5, "edit", &request),
                "a server switch invalidates the channel answer"
            );
        });
    });
}

/// A late history/older answer also carries `(server, channel, generation,
/// access)`; the conversation switch bumps the generation and the chat key.
#[gpui::test]
async fn late_history_answer_after_a_conversation_switch_is_rejected(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.before = Some(100);
            v.older_request = 9;
            let (generation, access) = (v.generation, v.access_generation);
            assert!(v.accepts_older_reply(9, 100, 1, 7, generation, access));

            v.conversation = Some(conversation(7));
            v.generation += 1;
            v.access_generation = v.access_generation.wrapping_add(1);
            assert!(
                !v.accepts_older_reply(9, 100, 1, 7, generation, access),
                "the channel page cannot repaint the personal timeline"
            );
        });
    });
}

/// A refresh started in a channel cannot insert its rows into a personal
/// conversation, and a conversation row cannot enter the channel window.
#[gpui::test]
async fn channel_and_conversation_rows_do_not_cross_the_scope(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.messages.push(dto(message_json(5, 1, "канал")));
            v.conversation = Some(conversation(7));
            v.messages.clear();

            let mut direct = message_json(6, 1, "личное");
            direct["channel_id"] = 0.into();
            direct["conversation_id"] = 7.into();
            v.merge_message(true, dto(direct.clone()));
            assert_eq!(v.messages.len(), 1);
            assert_eq!(v.messages[0].id, 6);

            // The numeric id matches both scopes; only the conversation row
            // may be accepted here.
            v.merge_message(true, dto(message_json(7, 1, "канал с тем же id")));
            assert_eq!(v.messages.len(), 1);
            assert_eq!(v.messages[0].id, 6);

            let mut foreign = direct;
            foreign["id"] = 8.into();
            foreign["conversation_id"] = 8.into();
            v.merge_message(true, dto(foreign));
            assert_eq!(v.messages.len(), 1);
        });
    });
}

/// A comment is never accepted into the main timeline, while the thread panel
/// accepts it for its root.
#[gpui::test]
async fn a_comment_never_becomes_a_main_channel_row(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
        });
        let client = Client::new("http://127.0.0.1:1").unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let root = dto(message_json(5, 1, "root"));
        let panel = app
            .new(|cx| ThreadPanel::new(client, runtime, 1, 7, root, true, 0, drafts, window, cx));
        let mut comment = message_json(6, 1, "comment");
        comment["thread_root_id"] = 5.into();
        view.update(app, |v, _| {
            v.merge_message(true, dto(comment.clone()));
            assert!(v.messages.is_empty(), "a comment stays out of the channel");
        });
        panel.update(app, |panel, cx| {
            panel.handle_event("message.created", &comment, 0, cx);
            assert_eq!(panel.messages().len(), 1);
            assert_eq!(panel.messages()[0].id, 6);
        });
    });
}

/// Editing the root is visible in the open thread; deleting it closes the
/// panel and removes the row from the channel window.
#[gpui::test]
async fn edit_and_delete_of_the_root_keep_an_open_thread_consistent(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.messages.push(dto(message_json(5, 1, "root")));
        });
        let client = Client::new("http://127.0.0.1:1").unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let root = view.read_with(app, |v, _| v.messages[0].clone());
        let panel = app
            .new(|cx| ThreadPanel::new(client, runtime, 1, 7, root, true, 0, drafts, window, cx));
        view.update(app, |v, _| {
            v.thread = Some(panel.clone());
            v.show_thread = true;
        });

        view.update(app, |v, cx| {
            v.merge_message(false, dto(message_json(5, 2, "edited root")));
            v.sync_thread_root(window, cx);
        });
        assert_eq!(panel.read_with(app, |panel, _| panel.root().revision), 2);
        assert_eq!(
            panel.read_with(app, |panel, _| panel.root().content.clone()),
            "edited root"
        );

        view.update(app, |v, cx| v.note_message_deleted(5, window, cx));
        view.read_with(app, |v, _| {
            assert!(v.messages.is_empty());
            assert!(v.thread.is_none());
            assert!(!v.show_thread);
        });
    });
}

/// A hover bar from the previous frame is inert after a scroll notification.
#[gpui::test]
async fn a_hover_click_from_before_the_scroll_is_ignored(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 7;
            v.messages.push(dto(message_json(5, 1, "row")));
            v.hovered_message = 5;
            v.hovered_action = Some((5, "more".into()));
            v.scroll_epoch = 4;
            v.dispatch_hover_action(5, message_tools::HoverAction::More, 3, 0., 0., window, cx);
            assert!(
                v.message_menu.is_none(),
                "the old frame's bar must not open a menu"
            );
            v.dispatch_hover_action(5, message_tools::HoverAction::More, 4, 0., 0., window, cx);
            assert!(v.message_menu.is_some(), "the current frame still works");
            v.message_menu = None;
            v.hovered_action = Some((5, "emoji".into()));
            v.dispatch_hover_action(
                5,
                message_tools::HoverAction::Picker,
                3,
                10.,
                20.,
                window,
                cx,
            );
            assert!(
                v.message_menu.is_none(),
                "the picker from before the scroll is ignored too"
            );
            v.dispatch_hover_action(
                5,
                message_tools::HoverAction::Picker,
                4,
                10.,
                20.,
                window,
                cx,
            );
            let menu = v.message_menu.as_ref().expect("picker menu");
            assert!(menu.expanded, "the emoji button opens the full catalogue");
            assert_eq!((menu.x, menu.y), (10., 20.));
        });
    });
}

/// Search targets are resolved against the readable channel snapshot; a fake
/// result for a closed channel never switches the screen.
#[gpui::test]
async fn search_jump_rejects_a_closed_channel_and_accepts_an_open_one(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new("http://127.0.0.1:1").unwrap());
            v.server = 1;
            v.channel = 7;
            v.channels = vec![Channel {
                id: 7,
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            }];
            assert_eq!(v.search_target(7, 42), Ok(7));
            assert_eq!(v.search_target(8, 42), Err("Канал недоступен."));
            v.open_search_hit(8, 42, window, cx);
            assert_eq!(v.channel, 7, "a closed channel is not selected");
            assert_eq!(v.error, "Канал недоступен.");
        });
    });
}

/// Stale search pages use the same request/scope/query identity as the popup.
#[test]
fn a_stale_search_page_is_rejected() {
    assert!(search::accepts_page(4, 4, 2, 2, "query", "query"));
    assert!(
        !search::accepts_page(3, 4, 2, 2, "query", "query"),
        "a new query owns the popup"
    );
    assert!(
        !search::accepts_page(4, 4, 1, 2, "query", "query"),
        "a channel switch owns the popup"
    );
    assert!(
        !search::accepts_page(4, 4, 2, 2, "old", "new"),
        "the submitted text must still match"
    );
}

/// Reaction revisions are applied independently from the pin snapshot.
#[gpui::test]
async fn reaction_revision_does_not_reset_the_pin_state(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            let mut row = dto(message_json(5, 6, "row"));
            row.reactions_revision = 6;
            v.messages.push(row);
            v.pinned_ids.insert(5);

            v.apply_reaction_update(voxhold_api::ReactionsUpdate {
                channel_id: 7,
                message_id: 5,
                revision: 5,
                reactions: Vec::new(),
            });
            assert_eq!(v.messages[0].reactions_revision, 6, "stale revision");

            v.apply_reaction_update(voxhold_api::ReactionsUpdate {
                channel_id: 7,
                message_id: 5,
                revision: 7,
                reactions: vec![voxhold_api::Reaction {
                    emoji: "🔥".into(),
                    count: 1,
                    me: true,
                }],
            });
            assert_eq!(v.messages[0].reactions_revision, 7);
            assert!(v.pinned_ids.contains(&5), "pin state is separate");
        });
    });
}

/// Two rows with the same URL and quote text keep separate row ids; the link
/// target is identical, but the click surface is owned by the row.
#[test]
fn identical_urls_and_quotes_keep_their_row_identity() {
    let reference = voxhold_api::MessageReference {
        message_id: 3,
        author: voxhold_api::Author {
            user_id: 9,
            username: "same".into(),
        },
        content: "same quote".into(),
        deleted: false,
    };
    let mut first = dto(message_json(10, 1, "https://example.com/story"));
    let mut second = dto(message_json(11, 1, "https://example.com/story"));
    first.reply = Some(reference.clone());
    second.reply = Some(reference);
    let (_, first_links) = crate::links::scan(&first.content);
    let (_, second_links) = crate::links::scan(&second.content);
    assert_eq!(first_links[0].open_href, second_links[0].open_href);
    assert_ne!(first.id, second.id);
    assert_eq!(message_tools::quote_target(&first), Some(3));
    assert_eq!(message_tools::quote_target(&second), Some(3));
}

/// The permission snapshot re-evaluates a role change immediately, which is
/// what the `403` recheck relies on.
#[test]
fn permissions_follow_the_current_role_snapshot() {
    let channel = Channel {
        id: 7,
        kind: "text".into(),
        permissions: 3,
        ..Default::default()
    };
    let row = dto(message_json(5, 1, "row"));
    let member = message_tools::permissions_for(2, "member", Some(&channel), &row, false);
    assert!(!member.pin && !member.delete);
    let admin = message_tools::permissions_for(2, "admin", Some(&channel), &row, false);
    assert!(admin.pin && admin.delete);
}
