//! Review follow-up regressions for forwarding, search deferral, websocket
//! thread deletion, thread paging/send lifetime, pins and direct reactions.

use super::super::*;
use super::{add_auth_window, message_json, wait_until};
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

/// Forwarding must post the source message id, never the source channel id.
#[gpui::test]
async fn forward_posts_the_source_message_id_for_channel_and_conversation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let channel_reply = message_json(500, 1, "channel forward").to_string();
    let mut direct_reply = message_json(501, 1, "conversation forward");
    direct_reply["channel_id"] = 0.into();
    direct_reply["conversation_id"] = 5.into();
    let direct_reply = direct_reply.to_string();
    let server = TestServer::spawn(vec![
        Reply::new(201, &channel_reply),
        Reply::new(201, &direct_reply),
    ]);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 7;
            v.channels = vec![Channel {
                id: 7,
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            }];
            let mut source = message_json(42, 1, "source");
            source["channel_id"] = 7.into();
            v.messages.push(serde_json::from_value(source).unwrap());
            v.forward = Some(ForwardState {
                message_id: 42,
                source: draft::ChatKey::channel(1, 7),
                source_kind: "channel",
                request: 1,
                destinations: vec![voxhold_api::ForwardDestination {
                    kind: "channel".into(),
                    id: 99,
                    name: "destination".into(),
                    server_id: 1,
                }],
                loading: false,
                sending: false,
                error: String::new(),
            });
            v.forward_to(0, window, cx);
        });
    });
    wait_until(&mut visual, || !server.requests().is_empty());
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.conversation = Some(conversation(5));
            v.channel = 5;
            v.messages.clear();
            let mut source = message_json(43, 1, "source direct");
            source["channel_id"] = 0.into();
            source["conversation_id"] = 5.into();
            v.messages.push(serde_json::from_value(source).unwrap());
            v.forward = Some(ForwardState {
                message_id: 43,
                source: draft::ChatKey::conversation(5),
                source_kind: "conversation",
                request: 2,
                destinations: vec![voxhold_api::ForwardDestination {
                    kind: "conversation".into(),
                    id: 100,
                    name: "direct destination".into(),
                    server_id: 0,
                }],
                loading: false,
                sending: false,
                error: String::new(),
            });
            v.forward_to(0, window, cx);
        });
    });
    wait_until(&mut visual, || {
        server
            .requests()
            .iter()
            .filter(|request| request.path == "/api/v1/me/forward")
            .count()
            >= 2
    });
    let requests = server.requests();
    let forwards: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "/api/v1/me/forward")
        .collect();
    assert_eq!(forwards.len(), 2, "{requests:?}");
    assert_eq!(forwards[0].method, "POST");
    let first: serde_json::Value = serde_json::from_slice(&forwards[0].body).unwrap();
    assert_eq!(first["source"]["kind"], "channel");
    assert_eq!(first["source"]["id"], 42);
    assert_eq!(first["destination"]["id"], 99);
    assert_eq!(first["destination"]["kind"], "channel");

    let second: serde_json::Value = serde_json::from_slice(&forwards[1].body).unwrap();
    assert_eq!(second["source"]["kind"], "conversation");
    assert_eq!(second["source"]["id"], 43);
    assert_eq!(second["destination"]["id"], 100);
    assert_eq!(second["destination"]["kind"], "conversation");
}

/// A search click is deferred out of `SearchPanel::update`; navigation runs
/// only while the click's session/server/access scope is still current.
#[gpui::test]
async fn search_click_defers_navigation_and_respects_the_captured_scope(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let server = TestServer::spawn(Vec::new());
    let panel_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = panel_slot.clone();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new(&server.base).unwrap());
            // `select_server(0)` installs the real deferred select handler.
            v.select_server(0, window, cx);
            v.server = 1;
            v.channel = 7;
            v.show_search = true;
            v.channels = vec![
                Channel {
                    id: 7,
                    kind: "text".into(),
                    permissions: 1,
                    ..Default::default()
                },
                Channel {
                    id: 8,
                    kind: "text".into(),
                    permissions: 1,
                    ..Default::default()
                },
            ];
            *captured.borrow_mut() = v.search.clone();
        });
        let panel = captured.borrow().clone().expect("search panel");
        panel.update(app, |panel, cx| {
            panel.select_for_test(8, 42, window, cx);
        });
        // The handler only registered a deferred effect: this must not panic
        // and must not navigate while the panel update is still on the stack.
        assert_eq!(view.read_with(app, |v, _| v.channel), 7);
    });
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.channel, 8,
            "the deferred click selects the result channel"
        );
        assert!(!v.show_search);
    });
    assert!(
        server
            .requests()
            .iter()
            .any(|request| request.path.contains("/messages/42/context")),
        "the deferred click requested context for the clicked message: {:?}",
        server.requests()
    );

    // Change the server after the click but before the deferred effect: the
    // visible screen must stay where it was.
    let panel = panel_slot.borrow().clone().expect("search panel");
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.show_search = true;
            v.access_generation = 0;
        });
        panel.update(app, |panel, cx| {
            panel.select_for_test(8, 43, window, cx);
        });
        view.update(app, |v, _| {
            v.server = 9;
        });
    });
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.server, 9);
        assert_eq!(v.channel, 7, "a server switch cancels the deferred jump");
        assert!(v.show_search, "the old search popup stays open");
    });
}

/// A websocket deletion of the open thread root closes the panel through the
/// workspace and saves the draft; a comment deletion leaves the panel open.
#[gpui::test]
async fn websocket_root_deletion_closes_the_thread_without_reentering(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.channels = vec![Channel {
                id: 7,
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            }];
            v.messages
                .push(serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap());
        });
        let client = Client::new("http://127.0.0.1:1").unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        drafts
            .borrow_mut()
            .set_text(draft::ChatKey::thread(1, 7, 5), "thread draft");
        let root = view.read_with(app, |v, _| v.messages[0].clone());
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
        view.update(app, |v, _| {
            v.thread = Some(panel);
            v.show_thread = true;
        });
        let event = voxhold_api::RealtimeEvent {
            kind: "message.deleted".into(),
            data: serde_json::json!({"channel_id": 7, "message_id": 5}),
            ..Default::default()
        };
        view.update(app, |v, cx| v.forward_thread_event(&event, window, cx));
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.thread.is_none());
        assert!(!v.show_thread);
        assert!(v.messages.is_empty());
        assert!(v.deleted_ids.contains(&5));
        assert_eq!(
            v.drafts.borrow().text(draft::ChatKey::thread(1, 7, 5)),
            "thread draft"
        );
    });

    // A normal comment deletion stays inside the open panel.
    visual.update(|window, app| {
        let client = Client::new("http://127.0.0.1:1").unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let root = serde_json::from_value::<Message>(message_json(6, 1, "second root")).unwrap();
        let panel = app
            .new(|cx| ThreadPanel::new(client, runtime, 1, 7, root, true, 0, drafts, window, cx));
        let mut comment = message_json(8, 1, "comment");
        comment["thread_root_id"] = 6.into();
        panel.update(app, |panel, cx| {
            panel.handle_event("message.created", &comment, 0, cx);
        });
        view.update(app, |v, _| {
            v.thread = Some(panel.clone());
            v.show_thread = true;
        });
        let event = voxhold_api::RealtimeEvent {
            kind: "message.deleted".into(),
            data: serde_json::json!({"channel_id": 7, "message_id": 8}),
            ..Default::default()
        };
        view.update(app, |v, cx| v.forward_thread_event(&event, window, cx));
        assert!(view.read_with(app, |v, _| v.thread.is_some()));
        assert!(panel.read_with(app, |panel, _| panel.messages().is_empty()));
    });
}

/// A thread GET cannot resurrect a deleted comment or discard newer WS rows.
#[gpui::test]
async fn late_thread_page_keeps_tombstones_and_newer_rows(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        let client = Client::new("http://127.0.0.1:1").unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let root = serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap();
        let panel = app
            .new(|cx| ThreadPanel::new(client, runtime, 1, 7, root, true, 0, drafts, window, cx));
        let row = |id: i64, revision: i64| {
            let mut value = message_json(id, revision, "comment");
            value["thread_root_id"] = 5.into();
            serde_json::from_value::<Message>(value).unwrap()
        };
        panel.update(app, |panel, cx| {
            panel.apply_page_for_test(
                voxhold_api::MessagePage {
                    messages: vec![row(10, 1), row(11, 1)],
                    pagination: voxhold_api::Pagination {
                        next_before_id: Some(10),
                        has_more: true,
                    },
                },
                0,
                None,
                false,
            );
            let deleted = serde_json::json!({"channel_id": 7, "message_id": 10});
            panel.handle_event("message.deleted", &deleted, 0, cx);
            let mut edit = message_json(11, 2, "edited");
            edit["thread_root_id"] = 5.into();
            panel.handle_event("message.updated", &edit, 0, cx);
            let mut created = message_json(12, 1, "new");
            created["thread_root_id"] = 5.into();
            panel.handle_event("message.created", &created, 0, cx);
            // A page requested before those events still contains the deleted
            // row and the old revision, and omits the new row.
            panel.apply_page_for_test(
                voxhold_api::MessagePage {
                    messages: vec![row(10, 1), row(11, 1)],
                    pagination: voxhold_api::Pagination {
                        next_before_id: Some(10),
                        has_more: false,
                    },
                },
                0,
                None,
                false,
            );
        });
        panel.read_with(app, |panel, _| {
            let ids: Vec<i64> = panel.messages().iter().map(|row| row.id).collect();
            assert_eq!(ids, [11, 12]);
            assert_eq!(panel.messages()[0].revision, 2);
            assert_eq!(panel.messages()[0].content, "edited");
        });
    });
}

/// News comments need the base Publish bit and enabled comments; the payload
/// carries the root as both thread root and reply target.
#[gpui::test]
async fn news_comment_permissions_and_payload(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let mut reply = message_json(30, 1, "comment");
    reply["thread_root_id"] = 20.into();
    let reply = reply.to_string();
    let server = TestServer::spawn(vec![Reply::new(201, &reply), Reply::new(201, &reply)]);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 7;
            v.channels = vec![Channel {
                id: 7,
                kind: "news".into(),
                permissions: 3,
                comments_enabled: true,
                ..Default::default()
            }];
            assert!(
                !v.channels[0].writable(),
                "can_publish is false, so the publication composer is hidden"
            );
            assert!(v.thread_writable(), "news reader may comment");
        });
        view.update(app, |v, _| {
            v.channels[0].comments_enabled = false;
            assert!(!v.thread_writable(), "disabled comments hide the composer");
            v.channels[0].kind = "text".into();
            assert!(v.thread_writable(), "text comments keep the base rule");
            v.channels[0].permissions = 1;
            assert!(!v.thread_writable(), "read-only text stays read-only");
        });
    });
    let runtime = view.read_with(&*visual, |v, _| v.runtime.clone());
    let client = Client::new(&server.base).unwrap();
    runtime
        .block_on(client.send_thread_message(1, 7, 20, "news", "key-news", true))
        .unwrap();
    runtime
        .block_on(client.send_thread_message(1, 7, 20, "text", "key-text", false))
        .unwrap();
    let requests = server.requests();
    let news: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(news["thread_root_id"], 20);
    assert_eq!(news["reply_to_message_id"], 20);
    assert_eq!(news["client_message_id"], "key-news");
    let text: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(text["thread_root_id"], 20);
    assert!(text.get("reply_to_message_id").is_none());
    assert_eq!(text["client_message_id"], "key-text");
}

/// Paging while a thread POST is in flight must not leave "Отправка…" stuck,
/// and a late confirmation must not clear an edited draft.
#[gpui::test]
async fn thread_send_is_independent_from_paging_and_keeps_edits(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate = Gate::new();
    let empty = serde_json::json!({
        "messages": [],
        "pagination": {"next_before_id": null, "has_more": false}
    })
    .to_string();
    let message = |id: i64, content: &str| {
        let mut value = message_json(id, 1, content);
        value["thread_root_id"] = 5.into();
        value.to_string()
    };
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &empty),
        Reply::new(201, &message(11, "first")).with_gate(gate.clone()),
        Reply::new(200, &empty),
        Reply::new(201, &message(12, "second")),
        Reply::new(500, "{\"error\":\"boom\"}"),
        Reply::new(201, &message(13, "edited send")).with_gate(gate.clone()),
    ]);
    let key = draft::ChatKey::thread(1, 7, 5);
    let panel_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = panel_slot.clone();
    visual.update(|window, app| {
        let client = Client::new(&server.base).unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let root = serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap();
        let panel = app
            .new(|cx| ThreadPanel::new(client, runtime, 1, 7, root, true, 0, drafts, window, cx));
        let owner = view.downgrade();
        panel.update(app, |panel, _| {
            panel.set_context(false, owner, 0);
            panel.set_read_for_test(1000);
        });
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.thread = Some(panel.clone());
            v.show_thread = true;
        });
        panel.update(app, |panel, cx| panel.load_for_test(0, cx));
        *captured.borrow_mut() = Some(panel);
    });
    let panel = panel_slot.borrow().clone().expect("thread panel");
    wait_entity(&mut visual, &panel, |panel| !panel.loading());

    // Hold the first POST, then page older while it is in flight.
    gate.arm();
    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_composer_for_test("first", window, cx);
            panel.send_for_test(window, cx);
        });
    });
    panel.read_with(&*visual, |panel, app| {
        assert_eq!(panel.composer_text(app), "first");
        assert!(
            panel.sending(),
            "send did not start: {}",
            panel.send_error()
        );
    });
    assert!(
        gate.wait_arrived(Duration::from_secs(3)),
        "the POST did not reach the gate: {:?}",
        server.requests()
    );
    visual.update(|_window, app| {
        panel.update(app, |panel, cx| panel.load_for_test(10, cx));
    });
    gate.release();
    wait_entity(&mut visual, &panel, |panel| !panel.sending());
    panel.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel
                .messages()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            [11]
        );
        assert!(panel.send_error().is_empty());
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.drafts.borrow().text(key), "");
    });

    // The next send still works.
    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_composer_for_test("second", window, cx);
            panel.send_for_test(window, cx);
        });
    });
    wait_entity(&mut visual, &panel, |panel| !panel.sending());
    panel.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel
                .messages()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            [11, 12]
        );
    });

    // An error settles the composer and keeps the draft for a retry.
    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_composer_for_test("error", window, cx);
            panel.send_for_test(window, cx);
        });
    });
    wait_entity(&mut visual, &panel, |panel| !panel.sending());
    panel.read_with(&*visual, |panel, app| {
        assert!(!panel.send_error().is_empty());
        assert_eq!(panel.composer_text(app), "error");
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.drafts.borrow().text(key), "error");
    });

    // Editing during a held POST keeps the new text but consumes the send.
    gate.arm();
    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_composer_for_test("Sent", window, cx);
            panel.send_for_test(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(3)));
    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_composer_for_test("new text", window, cx);
        });
    });
    gate.release();
    wait_entity(&mut visual, &panel, |panel| !panel.sending());
    panel.read_with(&*visual, |panel, app| {
        assert_eq!(
            panel.messages().iter().filter(|row| row.id == 13).count(),
            1
        );
        assert_eq!(panel.composer_text(app), "new text");
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.drafts.borrow().text(key), "new text");
    });
}

/// The pin list has its own request/loading/error state, and only the active
/// channel's pin events ask for one refresh.
#[gpui::test]
async fn pin_events_are_scoped_to_the_active_channel(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            let effect = v.apply_realtime_event(
                "message.pinned",
                &serde_json::json!({"channel_id": 7, "message_id": 10}),
            );
            assert!(effect.pins);
            let effect = v.apply_realtime_event(
                "message.unpinned",
                &serde_json::json!({"channel_id": 8, "message_id": 10}),
            );
            assert!(!effect.pins, "another channel does not refresh this popup");
            v.conversation = Some(conversation(7));
            let effect = v.apply_realtime_event(
                "message.pinned",
                &serde_json::json!({"channel_id": 7, "message_id": 10}),
            );
            assert!(!effect.pins, "personal chats have no pin popup");
        });
    });
}

/// `native.reconnected` refreshes an open personal chat even when no server is
/// selected or a server-channel access event is in the same batch.
#[gpui::test]
async fn reconnect_resyncs_an_open_conversation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 5;
            v.conversation = Some(conversation(5));
            let effect = v.apply_realtime_event("native.reconnected", &serde_json::json!({}));
            assert!(effect.resync);
            assert!(!effect.access);
            assert!(effect.recents);

            v.server = 0;
            let effect = v.apply_realtime_event("native.reconnected", &serde_json::json!({}));
            assert!(
                effect.resync,
                "server == 0 still refreshes the open direct chat"
            );

            v.conversation = None;
            v.server = 1;
            v.channel = 7;
            let effect = v.apply_realtime_event("native.reconnected", &serde_json::json!({}));
            assert!(effect.access);
            assert!(!effect.resync);
        });
    });
}

/// The channel-snapshot slot is owned by its request: a late answer for A does
/// not clear B's slot.
#[gpui::test]
async fn a_late_channel_snapshot_cannot_clear_a_newer_slot(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.channel_snapshot_request = Some(1);
            assert!(v.release_channel_snapshot(1));
            assert!(v.channel_snapshot_request.is_none());

            v.channel_snapshot_request = Some(2);
            assert!(!v.release_channel_snapshot(1));
            assert_eq!(v.channel_snapshot_request, Some(2));
            assert!(v.release_channel_snapshot(2));
        });
    });
}

/// A conversation reaction fetches only the reacted row and leaves the loaded
/// window, cursor and anchor untouched.
#[gpui::test]
async fn old_conversation_reaction_keeps_the_loaded_window(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let mut reaction_row = message_json(15, 1, "old");
    reaction_row["channel_id"] = 0.into();
    reaction_row["conversation_id"] = 5.into();
    reaction_row["reactions_revision"] = 2.into();
    reaction_row["reactions"] = serde_json::json!([
        {"emoji": "рџ”Ґ", "count": 1, "me": true}
    ]);
    let page = serde_json::json!({
        "messages": [reaction_row],
        "has_more": false
    })
    .to_string();
    let server = TestServer::spawn(vec![Reply::new(200, &page)]);
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
            assert!(!effect.resync, "no latest-page replacement");
            for (conversation, message) in effect.conversation_reactions {
                v.refresh_conversation_reaction(conversation, message, cx);
            }
        });
    });
    wait_entity(&mut visual, &view, |v| {
        v.messages
            .iter()
            .find(|row| row.id == 15)
            .is_some_and(|row| row.reactions_revision == 2)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.messages.len(), 60);
        assert_eq!(v.before, Some(1));
        let row = v.messages.iter().find(|row| row.id == 15).unwrap();
        assert_eq!(row.reactions.len(), 1);
        assert!(row.reactions[0].me);
    });
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].path.contains("before=16"),
        "the reacted row is fetched with mid + 1: {}",
        requests[0].path
    );
}

/// Closing the panel while its POST is in flight still finishes the original
/// draft through the workspace-owned confirmation.
#[gpui::test]
async fn closing_the_thread_finishes_the_original_draft(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate = Gate::new();
    let empty = serde_json::json!({
        "messages": [],
        "pagination": {"next_before_id": null, "has_more": false}
    })
    .to_string();
    let mut reply = message_json(21, 1, "closed send");
    reply["thread_root_id"] = 5.into();
    let reply = reply.to_string();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &empty),
        Reply::new(201, &reply).with_gate(gate.clone()),
    ]);
    let key = draft::ChatKey::thread(1, 7, 5);
    let panel_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = panel_slot.clone();
    visual.update(|window, app| {
        let client = Client::new(&server.base).unwrap();
        let runtime = view.read_with(app, |v, _| v.runtime.clone());
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let root = serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap();
        let panel = app
            .new(|cx| ThreadPanel::new(client, runtime, 1, 7, root, true, 0, drafts, window, cx));
        let owner = view.downgrade();
        panel.update(app, |panel, _| {
            panel.set_context(false, owner, 0);
            panel.set_read_for_test(1000);
        });
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.thread = Some(panel.clone());
            v.show_thread = true;
        });
        panel.update(app, |panel, cx| panel.load_for_test(0, cx));
        *captured.borrow_mut() = Some(panel);
    });
    let panel = panel_slot.borrow().clone().expect("thread panel");
    wait_entity(&mut visual, &panel, |panel| !panel.loading());

    gate.arm();
    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_composer_for_test("closed draft", window, cx);
            panel.send_for_test(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(3)));
    visual.update(|window, app| {
        view.update(app, |v, cx| v.close_thread(window, cx));
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.thread.is_none());
        assert_eq!(v.drafts.borrow().text(key), "closed draft");
    });
    gate.release();
    wait_entity(&mut visual, &view, |v| {
        v.drafts.borrow().text(key).is_empty()
    });
}

/// A reconnect event queued next to a server access event still refreshes the
/// already-open direct chat. This exercises the real batch dispatcher.
#[gpui::test]
async fn reconnect_batch_refreshes_the_open_conversation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let mut new_row = message_json(7, 1, "missed while offline");
    new_row["channel_id"] = 0.into();
    new_row["conversation_id"] = 5.into();
    let page = serde_json::json!({
        "messages": [new_row],
        "has_more": false
    })
    .to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, "{\"conversations\":[]}"),
        Reply::new(200, &page),
    ]);
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    tx.send(voxhold_api::RealtimeEvent {
        kind: "channels.changed".into(),
        data: serde_json::json!({"server_id": 1}),
        ..Default::default()
    })
    .unwrap();
    tx.send(voxhold_api::RealtimeEvent {
        kind: "native.reconnected".into(),
        data: serde_json::json!({}),
        ..Default::default()
    })
    .unwrap();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 5;
            v.conversation = Some(conversation(5));
            v.access_pending = false;
            v.events = Some(std::sync::Arc::new(std::sync::Mutex::new(rx)));
            v.events_overflow = Default::default();
            v.schedule_events(v.realtime_generation, window, cx);
        });
    });
    // The batch dispatcher is driven by a 150 ms timer; advance the test clock
    // so it runs without waiting on wall time.
    visual.executor().advance_clock(Duration::from_millis(200));
    wait_entity(&mut visual, &view, |v| {
        v.messages.iter().any(|row| row.id == 7)
    });
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            assert_eq!(v.channel, 5);
            assert!(v.in_conversation());
            v.events = None;
            v.realtime_generation = v.realtime_generation.wrapping_add(1);
        });
    });
}
