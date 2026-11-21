//! CF-05 regressions: a deferred navigation started from a message-search hit
//! must lose its right to open a conversation as soon as the user changes the
//! intent (picks a chat, another hit, a new query, closes the search or leaves
//! the personal surface). A late answer may neither select its own row, show
//! its error, drop fresh results nor clear the slot of a newer request.

use super::super::*;
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

pub(super) fn make_app(
    cx: &mut TestAppContext,
    client: voxhold_api::Client,
) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-cf05-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let shell = dir.join("workspace-layout.json");
    let instance = dir.join("instance.json");
    let sessions = dir.join("sessions.bin");
    let slot: std::rc::Rc<std::cell::RefCell<Option<Entity<Voxhold>>>> = Default::default();
    let captured = slot.clone();
    let window = cx.add_window(move |window, cx| {
        let view = cx.new(|cx| {
            let mut view = Voxhold::new_with_startup(
                window,
                cx,
                "",
                Some(shell),
                Some(instance),
                Some(sessions),
                false,
            );
            view.client = Some(client);
            view.self_user_id = 1;
            view.servers = vec![voxhold_api::Server {
                id: 1,
                name: "QA".into(),
                role: "member".into(),
            }];
            view.server = 1;
            view
        });
        *captured.borrow_mut() = Some(view.clone());
        Root::new(view.into(), window, cx)
    });
    let view = slot.borrow().clone().expect("view");
    (window, view)
}

/// Release the gate as soon as the request reaches the server, without
/// blocking the GPUI test thread that has to drive the background task.
pub(super) fn release_on_arrival(gate: Arc<Gate>) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let arrived = gate.wait_arrived(Duration::from_secs(5));
        gate.release();
        arrived
    })
}

pub(super) fn wait_for(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    mut done: impl FnMut(&Voxhold) -> bool,
) {
    for _ in 0..1000 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| done(v)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("condition did not become true");
}

pub(super) fn conversation(
    id: i64,
    name: &str,
    direct: bool,
    muted: bool,
) -> voxhold_api::Conversation {
    serde_json::from_value(json!({
        "id": id,
        "name": name,
        "direct": direct,
        "members": [
            {"user_id": 1, "username": "me"},
            {"user_id": id + 10, "username": if name.is_empty() { format!("user{id}") } else { name.into() }}
        ],
        "unread_count": 0,
        "saved": false,
        "muted": muted,
        "last_message_id": id * 10
    }))
    .unwrap()
}

pub(super) fn message(id: i64, conversation_id: i64) -> voxhold_api::Message {
    serde_json::from_value(json!({
        "id": id, "channel_id": 0, "conversation_id": conversation_id,
        "content": "row", "author": {"user_id": 1, "username": "me"},
        "created_at": 1, "revision": 1, "reactions": [], "attachments": []
    }))
    .unwrap()
}

pub(super) fn direct_client(server: &TestServer) -> voxhold_api::Client {
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    client
}

fn messages_page(rows: Vec<voxhold_api::Message>) -> String {
    json!({
        "messages": rows,
        "has_more": false
    })
    .to_string()
}

fn conversation_json(id: i64, name: &str) -> String {
    serde_json::to_string(&conversation(id, name, true, false)).unwrap()
}

/// A pinned conversation that would only be loaded by a stale navigation.
fn saved_conversation_json(id: i64) -> String {
    let mut row = conversation(id, "Избранное", false, false);
    row.saved = true;
    serde_json::to_string(&row).unwrap()
}

/// The stale hit would refresh its history and notifications if it ever opened.
fn assert_no_open_side_effects(server: &TestServer) {
    let requests = server.requests();
    let paths: Vec<_> = requests.iter().map(|row| row.path.clone()).collect();
    assert!(
        !requests.iter().any(|row| {
            row.path.starts_with("/api/v1/conversations/50/messages")
                || row
                    .path
                    .starts_with("/api/v1/conversations/50/notifications")
        }),
        "a cancelled hit must not load its conversation: {paths:?}"
    );
}

/// `GET /conversations/50` is held; chat 8 is selected by hand. The answer is
/// released only afterwards and must not replace chat 8.
#[gpui::test]
async fn manual_chat_selection_keeps_itself_when_the_search_open_arrives_late(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page(vec![message(800, 8)]))
            .with_path("/api/v1/conversations/8/messages"),
        Reply::new(200, r#"{"conversation_id":8,"muted":false}"#)
            .with_path("/api/v1/conversations/8/notifications"),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.conversations = vec![conversation(8, "Восемь", true, false)];
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some(), "the hit owns a slot");
            assert!(v.conversation.is_none(), "nothing opened before the answer");
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_conversation(conversation(8, "Восемь", true, false), window, cx);
            assert!(
                v.messaging.open_request.is_none(),
                "a manual selection invalidates the deferred search open"
            );
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(
        waiter.join().unwrap(),
        "the held search open reached the server"
    );
    wait_for(&mut visual, &view, |v| {
        v.messages.iter().any(|row| row.id == 800)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(8),
            "the late search answer must not replace the manually chosen chat"
        );
        assert_eq!(v.channel, 8);
        assert_eq!(v.chat_key(), draft::ChatKey::conversation(8));
        assert!(v.messaging.open_request.is_none());
        assert!(
            v.messaging.search.error.is_empty(),
            "a stale success must not show an error"
        );
        assert!(
            v.conversations.iter().any(|row| row.id == 8),
            "chat 8 remains in the index"
        );
    });
    assert_no_open_side_effects(&server);
}

/// A held `GET /conversations/{id}` that ends with a `500` after the manual
/// selection must not paint its error over the chosen chat.
#[gpui::test]
async fn stale_search_open_error_does_not_replace_the_manually_chosen_chat(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(500, r#"{"error":"failed"}"#)
            .with_path("/api/v1/conversations/50")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page(vec![message(800, 8)]))
            .with_path("/api/v1/conversations/8/messages"),
        Reply::new(200, r#"{"conversation_id":8,"muted":false}"#)
            .with_path("/api/v1/conversations/8/notifications"),
        Reply::new(200, &messages_page(Vec::new())),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.conversations = vec![conversation(8, "Восемь", true, false)];
            v.open_conversation_message(50, 300, window, cx);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_conversation(conversation(8, "Восемь", true, false), window, cx);
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(
        waiter.join().unwrap(),
        "the held search open reached the server"
    );
    wait_for(&mut visual, &view, |v| {
        v.messages.iter().any(|row| row.id == 800)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(8));
        assert_eq!(v.channel, 8);
        assert!(
            v.messaging.search.error.is_empty(),
            "the stale error must not be painted over the chosen chat"
        );
        assert!(v.messaging.open_request.is_none());
    });
    assert_no_open_side_effects(&server);
}

/// Two hits clicked in a row: the second click owns the navigation. The answer
/// for the first hit is released afterwards and must not replace the second
/// chat.
#[gpui::test]
async fn two_search_hits_in_a_row_leave_navigation_to_the_last_one(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page(vec![message(400, 60)]))
            .with_path("/api/v1/conversations/60/messages?before=401"),
        Reply::new(200, &messages_page(Vec::new())).with_path("/api/v1/conversations/60/messages"),
        Reply::new(200, r#"{"conversation_id":60,"muted":false}"#)
            .with_path("/api/v1/conversations/60/notifications"),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            // The first hit is outside the index and starts the deferred open.
            v.open_conversation_message(50, 300, window, cx);
            assert!(
                v.messaging.open_request.is_some(),
                "the first hit owns a slot"
            );
            // The second hit points at a chat the index already knows, so it
            // opens immediately and must own the navigation.
            v.conversations = vec![conversation(60, "Шесть", true, false)];
            v.open_conversation_message(60, 400, window, cx);
            assert!(
                v.messaging.open_request.is_none(),
                "an immediate hit cannot leave a deferred slot behind"
            );
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(
        waiter.join().unwrap(),
        "the first search open reached the server"
    );
    wait_for(&mut visual, &view, |v| {
        v.conversation.as_ref().map(|row| row.id) == Some(60)
            && v.messages.iter().any(|row| row.id == 400)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(60),
            "the later hit owns the opened conversation"
        );
        assert_eq!(v.channel, 60);
        assert!(v.messages.iter().all(|row| row.id != 300));
        assert_eq!(v.messaging.open_request, None);
        assert!(v.messaging.search.error.is_empty());
    });
    assert_no_open_side_effects(&server);
}

/// Two unloaded hits clicked in a row: the newer `open_request` replaces the
/// older one, so only the last answer may open its conversation.
#[gpui::test]
async fn two_unloaded_search_hits_keep_only_the_last_navigation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let older = Gate::new();
    let newer = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_gate(older.clone()),
        Reply::new(200, &conversation_json(60, "Шесть"))
            .with_path("/api/v1/conversations/60")
            .with_gate(newer.clone()),
        Reply::new(200, &messages_page(Vec::new())).with_path("/api/v1/conversations/60/messages"),
        Reply::new(200, r#"{"conversation_id":60,"muted":false}"#)
            .with_path("/api/v1/conversations/60/notifications"),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    older.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.open_conversation_message(50, 300, window, cx);
            assert!(
                v.messaging.open_request.is_some(),
                "the first hit owns a slot"
            );
        });
    });
    newer.arm();
    let slots = visual.update(|window, app| {
        view.update(app, |v, cx| {
            let older_slot = v.messaging.open_request.expect("the first slot");
            v.open_conversation_message(60, 400, window, cx);
            (
                older_slot,
                v.messaging
                    .open_request
                    .expect("the second hit owns a slot"),
            )
        })
    });
    assert_ne!(slots.0, slots.1, "each hit gets a fresh request id");
    let waiter_older = release_on_arrival(older.clone());
    let waiter_newer = release_on_arrival(newer.clone());
    visual.run_until_parked();
    assert!(
        waiter_older.join().unwrap(),
        "the older hit reached the server"
    );
    assert!(
        waiter_newer.join().unwrap(),
        "the newer hit reached the server"
    );
    wait_for(&mut visual, &view, |v| {
        v.conversation.as_ref().map(|row| row.id) == Some(60)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(60),
            "only the last hit owns the navigation"
        );
        assert_eq!(v.channel, 60);
        assert_eq!(
            v.messaging.open_request, None,
            "a late answer must not clear a newer slot"
        );
        assert!(
            v.messaging.search.error.is_empty(),
            "the older answer must not be reported as a failure"
        );
    });
    assert_no_open_side_effects(&server);
}

/// The normal path stays intact: one search hit loads its row outside the index
/// and jumps to the message.
#[gpui::test]
async fn a_single_search_hit_still_opens_its_conversation_and_message(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять")).with_path("/api/v1/conversations/50"),
        Reply::new(200, &messages_page(vec![message(300, 50)]))
            .with_path("/api/v1/conversations/50/messages?before=301"),
        Reply::new(200, &messages_page(Vec::new())).with_path("/api/v1/conversations/50/messages"),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#)
            .with_path("/api/v1/conversations/50/notifications"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some());
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.as_ref().map(|row| row.id) == Some(50)
            && v.messages.iter().any(|row| row.id == 300)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.channel, 50);
        assert_eq!(v.chat_key(), draft::ChatKey::conversation(50));
        assert!(v.messaging.open_request.is_none());
        assert!(v.messaging.search.error.is_empty());
        assert!(
            v.conversations.iter().any(|row| row.id == 50),
            "the loaded row joins the index"
        );
    });
    assert!(server
        .requests()
        .iter()
        .any(|row| row.path == "/api/v1/conversations/50"));
}

/// Switching the search to the name filter hides the message results; a hit
/// clicked before the switch must not open or report anything.
#[gpui::test]
async fn switching_the_search_mode_cancels_a_deferred_open(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some());
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.toggle_messages_search_mode(window, cx);
            assert!(!v.messaging.global);
            assert!(
                v.messaging.open_request.is_none(),
                "the mode switch cancels the deferred open"
            );
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the held hit reached the server");
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation.is_none());
        assert!(v.messaging.search.error.is_empty());
        assert!(v.messaging.open_request.is_none());
    });
    assert_no_open_side_effects(&server);
}

/// Leaving the personal surface drops the search and its deferred navigation.
#[gpui::test]
async fn leaving_the_personal_surface_cancels_a_deferred_open(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.personal_return = Some((1, 0));
            v.messaging.global = true;
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some());
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.return_to_channels(window, cx);
            assert!(!v.personal_mode);
            assert!(v.messaging.open_request.is_none());
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the held hit reached the server");
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation.is_none());
        assert!(v.messaging.search.error.is_empty());
        assert!(v.messaging.open_request.is_none());
    });
    assert_no_open_side_effects(&server);
}

/// A hit belongs to the query it was found with. Typing a new query cancels it
/// before the answer arrives.
#[gpui::test]
async fn editing_the_search_query_cancels_a_deferred_open(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.messaging.search.results = vec![voxhold_api::ConversationSearchHit {
                id: 3,
                conversation_id: 50,
                author_id: 2,
                author_username: "neva".into(),
                content: "hit".into(),
                created_at: 1,
            }];
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some());
        });
    });
    visual.update(|window, app| {
        let query = view.read_with(app, |v, _| v.messaging.query.clone());
        query.update(app, |input, cx| input.set_value("neva", window, cx));
    });
    // The change is delivered as an effect after the update that emitted it.
    visual.update(|_, app| {
        view.update(app, |v, _| {
            assert!(
                v.messaging.open_request.is_none(),
                "editing the query cancels the deferred open"
            );
            assert!(
                v.messaging.search.results.is_empty(),
                "the old result set is dropped with the query"
            );
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the held hit reached the server");
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation.is_none());
        assert!(
            v.messaging.search.error.is_empty(),
            "the stale saved answer must not repaint the error"
        );
        assert!(v.messaging.search.results.is_empty());
        assert!(v.messaging.open_request.is_none());
    });
    assert_no_open_side_effects(&server);
}

/// A new submit replaces the query and its results. A held hit of the previous
/// query must not report a failure or drop the fresh rows (even when it points
/// at the same conversation and is a saved favorite).
#[gpui::test]
async fn a_new_search_submit_cancels_the_previous_hit(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let fresh = json!({
        "results": [{
            "id": 7, "conversation_id": 50, "author_id": 2,
            "author_username": "neva", "content": "fresh", "created_at": 2
        }],
        "has_more": false,
        "next_before": null
    })
    .to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, &saved_conversation_json(50))
            .with_path("/api/v1/conversations/50")
            .with_gate(gate.clone()),
        Reply::new(200, &fresh).with_path("/api/v1/me/conversations/search"),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            // The query is set before the hit, so only the submit can cancel it.
            v.messaging
                .query
                .update(cx, |input, cx| input.set_value("ol", window, cx));
        });
    });
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some());
        });
    });
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.submit_messages_search(cx);
            assert!(
                v.messaging.open_request.is_none(),
                "a new submit cancels the previous hit"
            );
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the stale hit reached the server");
    wait_for(&mut visual, &view, |v| !v.messaging.search.loading);
    view.read_with(&*visual, |v, _| {
        assert!(
            v.messaging.search.error.is_empty(),
            "the stale saved answer must not repaint the error"
        );
        assert_eq!(
            v.messaging.search.results.len(),
            1,
            "the fresh result set survives the stale answer"
        );
        assert_eq!(v.messaging.search.results[0].id, 7);
        assert!(v.conversation.is_none());
        assert!(v.messaging.open_request.is_none());
    });
    assert_no_open_side_effects(&server);
}

/// Invariant guard: an answer never navigates when the search surface is no
/// longer showing message results, even if the slot survived a mode change.
#[gpui::test]
async fn a_hit_answer_is_ignored_when_the_search_surface_is_hidden(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some());
            // The surface stops showing message results while the slot is
            // still set; the callback must not trust the hit any more.
            v.messaging.global = false;
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the held hit reached the server");
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation.is_none());
        assert!(v.messaging.search.error.is_empty());
        assert!(
            v.messaging.open_request.is_none(),
            "the hidden surface releases its own stale slot"
        );
    });
    assert_no_open_side_effects(&server);
}
