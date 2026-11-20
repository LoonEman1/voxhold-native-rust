//! Second review follow-up: confirmed search permissions and thread send/page
//! ownership, plus repeatable reaction refreshes.

use super::super::*;
use super::{add_auth_window, message_json};
use crate::test_support::{Gate, Reply, TestServer};
use crate::thread_panel::ThreadPanel;
use gpui::{Entity, TestAppContext, VisualTestContext};
use std::time::Duration;

fn conversation(id: i64) -> voxhold_api::Conversation {
    voxhold_api::Conversation {
        id,
        direct: true,
        ..Default::default()
    }
}

fn wait_entity<T: 'static>(
    visual: &mut VisualTestContext,
    entity: &Entity<T>,
    mut done: impl FnMut(&T) -> bool,
) {
    for _ in 0..600 {
        visual.run_until_parked();
        if entity.read_with(&*visual, |value, _| done(value)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the asynchronous operation did not finish");
}

/// A search started from a personal conversation waits for a fresh channel
/// snapshot before it accepts any query or discloses an allowed id.
#[gpui::test]
async fn dm_search_waits_for_a_fresh_channel_snapshot(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let gate = Gate::new();
    gate.arm();
    let channels = serde_json::json!([{
        "id": 8,
        "name": "fresh",
        "kind": "text",
        "permissions": 1,
        "e2ee_required": false
    }])
    .to_string();
    let server =
        TestServer::spawn_concurrent(vec![Reply::new(200, &channels).with_gate(gate.clone())]);
    let client = Client::new(&server.base).unwrap();
    let panel_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = panel_slot.clone();
    visual.update(|window, app| {
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let panel =
            app.new(|cx| crate::search::SearchPanel::new(client.clone(), runtime, window, cx));
        view.update(app, |v, cx| {
            v.client = Some(client);
            v.server = 1;
            v.channel = 5;
            v.conversation = Some(conversation(5));
            v.channels = vec![Channel {
                id: 7,
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            }];
            v.search = Some(panel.clone());
            v.open_search_popup(false, window, cx);
        });
        *captured.borrow_mut() = Some(panel);
    });
    let panel = panel_slot.borrow().clone().expect("search panel");
    panel.read_with(&*visual, |panel, _| {
        assert!(!panel.permissions_ready());
        assert!(panel.allowed_for_test().is_empty());
    });
    assert!(gate.wait_arrived(Duration::from_secs(3)));
    gate.release();
    wait_entity(&mut visual, &panel, |panel| panel.permissions_ready());
    panel.read_with(&*visual, |panel, _| {
        assert!(panel.allowed_for_test().contains(&8));
        assert!(
            !panel.allowed_for_test().contains(&7),
            "the revoked channel is not part of the confirmed snapshot"
        );
    });
}

/// A POST kept in `pending_sends` under the thread key is visible to a panel
/// reopened before the answer arrives, and the answer consumes it once.
#[gpui::test]
async fn reopening_a_thread_sees_the_pending_post(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate = Gate::new();
    let empty = serde_json::json!({
        "messages": [],
        "pagination": {"next_before_id": null, "has_more": false}
    })
    .to_string();
    let mut reply = message_json(31, 1, "pending send");
    reply["thread_root_id"] = 5.into();
    let reply = reply.to_string();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &empty),
        Reply::new(201, &reply).with_gate(gate.clone()),
        Reply::new(200, &empty),
    ]);
    let key = draft::ChatKey::thread(1, 7, 5);
    let first_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let first_captured = first_slot.clone();
    visual.update(|window, app| {
        let client = Client::new(&server.base).unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let root = serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap();
        let panel = app.new(|cx| {
            ThreadPanel::new(
                client.clone(),
                runtime,
                1,
                7,
                root,
                true,
                0,
                drafts,
                window,
                cx,
            )
        });
        let owner = view.downgrade();
        panel.update(app, |panel, _| {
            panel.set_context(false, owner, 0);
            panel.set_read_for_test(1000);
        });
        view.update(app, |v, _| {
            v.client = Some(client);
            v.server = 1;
            v.channel = 7;
            v.channels = vec![Channel {
                id: 7,
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            }];
            v.thread = Some(panel.clone());
            v.show_thread = true;
        });
        panel.update(app, |panel, cx| panel.load_for_test(0, cx));
        *first_captured.borrow_mut() = Some(panel);
    });
    let first = first_slot.borrow().clone().expect("first panel");
    wait_entity(&mut visual, &first, |panel| !panel.loading());
    gate.arm();
    visual.update(|window, app| {
        first.update(app, |panel, cx| {
            panel.set_composer_for_test("pending draft", window, cx);
            panel.send_for_test(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(3)));
    let attempt = first.read_with(&*visual, |panel, _| panel.attempt());
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.pending_sends.get(&key).map(|pending| pending.attempt),
            Some(attempt)
        );
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.close_thread(window, cx));
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.thread.is_none());
        assert!(v.pending_sends.contains_key(&key));
    });

    let root = serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.open_thread(root, 0, window, cx));
    });
    let reopened = view
        .read_with(&*visual, |v, _| v.thread.clone())
        .expect("reopened panel");
    reopened.read_with(&*visual, |panel, _| {
        assert!(panel.sending(), "a reopened panel sees the active POST");
        assert_eq!(panel.attempt(), attempt);
    });
    gate.release();
    wait_entity(&mut visual, &reopened, |panel| !panel.sending());
    reopened.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel
                .messages()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            [31]
        );
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.pending_sends.contains_key(&key));
        assert_eq!(v.drafts.borrow().text(key), "");
    });
}

/// WS/POST rows that arrive during a latest-page GET survive an empty answer
/// and a deletion tombstone still removes the old row.
#[gpui::test]
async fn empty_thread_pages_keep_concurrent_comments(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        let client = Client::new("http://127.0.0.1:1").unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let root = serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap();
        let panel = app.new(|cx| {
            ThreadPanel::new(
                client.clone(),
                runtime.clone(),
                1,
                7,
                root,
                true,
                0,
                drafts.clone(),
                window,
                cx,
            )
        });
        let row = |id: i64, revision: i64| {
            let mut value = message_json(id, revision, "comment");
            value["thread_root_id"] = 5.into();
            serde_json::from_value::<Message>(value).unwrap()
        };
        let empty = || voxhold_api::MessagePage {
            messages: Vec::new(),
            pagination: voxhold_api::Pagination {
                next_before_id: None,
                has_more: false,
            },
        };
        panel.update(app, |panel, cx| {
            panel.apply_page_for_test(
                voxhold_api::MessagePage {
                    messages: vec![row(10, 1)],
                    pagination: voxhold_api::Pagination {
                        next_before_id: Some(10),
                        has_more: false,
                    },
                },
                0,
                None,
                false,
            );
            panel.begin_latest_for_test();
            let mut created = message_json(11, 1, "ws during get");
            created["thread_root_id"] = 5.into();
            panel.handle_event("message.created", &created, 0, cx);
            let mut posted = message_json(12, 1, "post during get");
            posted["thread_root_id"] = 5.into();
            panel.handle_event("message.created", &posted, 0, cx);
            panel.handle_event(
                "message.deleted",
                &serde_json::json!({"channel_id": 7, "message_id": 10}),
                0,
                cx,
            );
            panel.apply_latest_page_for_test(empty());
        });
        panel.read_with(app, |panel, _| {
            assert_eq!(
                panel
                    .messages()
                    .iter()
                    .map(|row| row.id)
                    .collect::<Vec<_>>(),
                [11, 12]
            );
        });

        // First page (initial == false): a WS comment is not erased by an
        // empty latest answer either.
        let root = serde_json::from_value::<Message>(message_json(6, 1, "root two")).unwrap();
        let first = app
            .new(|cx| ThreadPanel::new(client, runtime, 1, 7, root, true, 0, drafts, window, cx));
        first.update(app, |panel, cx| {
            panel.begin_latest_for_test();
            let mut created = message_json(21, 1, "first ws");
            created["thread_root_id"] = 6.into();
            panel.handle_event("message.created", &created, 0, cx);
            panel.apply_latest_page_for_test(empty());
            assert_eq!(
                panel
                    .messages()
                    .iter()
                    .map(|row| row.id)
                    .collect::<Vec<_>>(),
                [21]
            );
        });
    });
}

/// Two reaction events for the same row produce one active GET and exactly one
/// repeat; the later answer clears its own slot.
#[gpui::test]
async fn two_reaction_events_share_one_active_get_and_one_repeat(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate_first = Gate::new();
    let gate_repeat = Gate::new();
    gate_first.arm();
    gate_repeat.arm();
    let page = |revision: i64, emoji: &str| {
        let mut row = message_json(15, 1, "old direct");
        row["channel_id"] = 0.into();
        row["conversation_id"] = 5.into();
        row["reactions_revision"] = revision.into();
        row["reactions"] = serde_json::json!([
            {"emoji": emoji, "count": 1, "me": true}
        ]);
        serde_json::json!({"messages": [row], "has_more": false}).to_string()
    };
    let first = page(2, "first");
    let second = page(3, "second");
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &first).with_gate(gate_first.clone()),
        Reply::new(200, &second).with_gate(gate_repeat.clone()),
    ]);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 5;
            v.conversation = Some(conversation(5));
            for id in 1..=60 {
                let mut row = message_json(id, 1, "direct row");
                row["channel_id"] = 0.into();
                row["conversation_id"] = 5.into();
                v.messages.push(serde_json::from_value(row).unwrap());
            }
            v.before = Some(1);
            let effect = v.apply_realtime_event(
                "conversation.reactions",
                &serde_json::json!({"conversation_id": 5, "message_id": 15}),
            );
            for (conversation, message) in effect.conversation_reactions {
                v.refresh_conversation_reaction(conversation, message, cx);
            }
        });
    });
    assert!(gate_first.wait_arrived(Duration::from_secs(3)));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let effect = v.apply_realtime_event(
                "conversation.reactions",
                &serde_json::json!({"conversation_id": 5, "message_id": 15}),
            );
            for (conversation, message) in effect.conversation_reactions {
                v.refresh_conversation_reaction(conversation, message, cx);
            }
        });
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.conversation_reaction_requests.len(), 1);
        assert!(v
            .conversation_reaction_requests
            .values()
            .all(|entry| entry.again));
    });
    gate_first.release();
    wait_entity(&mut visual, &view, |_| server.requests().len() >= 2);
    gate_repeat.release();
    wait_entity(&mut visual, &view, |v| {
        v.messages
            .iter()
            .find(|row| row.id == 15)
            .is_some_and(|row| row.reactions_revision == 3)
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation_reaction_requests.is_empty());
        assert_eq!(v.messages.len(), 60);
        assert_eq!(v.before, Some(1));
    });
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request.path.contains("before=16")));
}
