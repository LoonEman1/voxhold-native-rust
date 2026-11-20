//! CF-05 regressions: a deferred navigation started from a message-search hit
//! must lose its right to open a conversation as soon as the user picks a chat
//! or another hit. A late answer may neither select its own row, show its
//! error nor clear the slot of a newer search request.

use super::super::*;
use crate::test_support::{Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;
use serde_json::json;
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
            let mut view =
                Voxhold::new(window, cx, "", Some(shell), Some(instance), Some(sessions));
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

pub(super) fn wait_for(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    mut done: impl FnMut(&Voxhold) -> bool,
) {
    for _ in 0..1500 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| done(v)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("condition did not become true");
}

/// Drive the request and its callback to completion. The test executor runs a
/// background request inline, so a `with_delay` answer is already late by the
/// time this returns and no manual gate can be released from this thread.
fn pump(visual: &mut VisualTestContext) {
    for _ in 0..80 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
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

/// `GET /conversations/50` is delayed; chat 8 is selected by hand before the
/// answer arrives. The late hit must not replace chat 8.
#[gpui::test]
async fn manual_chat_selection_keeps_itself_when_the_search_open_arrives_late(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_delay(Duration::from_millis(200)),
        Reply::new(200, &messages_page(vec![message(800, 8)]))
            .with_path("/api/v1/conversations/8/messages"),
        Reply::new(200, r#"{"conversation_id":8,"muted":false}"#)
            .with_path("/api/v1/conversations/8/notifications"),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.conversations = vec![conversation(8, "Восемь", true, false)];
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some(), "the hit owns a slot");
            assert!(v.conversation.is_none(), "nothing opened before the answer");
            // The user picks chat 8 in the index while the hit is in flight.
            v.select_conversation(conversation(8, "Восемь", true, false), window, cx);
            assert!(
                v.messaging.open_request.is_none(),
                "a manual selection invalidates the deferred search open"
            );
        });
    });
    pump(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(8),
            "the late search answer must not replace the manually chosen chat"
        );
        assert_eq!(v.channel, 8);
        assert_eq!(v.chat_key(), draft::ChatKey::conversation(8));
        assert!(
            v.messages.iter().any(|row| row.id == 800),
            "the chosen chat loaded its own history"
        );
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
    assert!(
        server
            .requests()
            .iter()
            .any(|row| row.path == "/api/v1/conversations/50"),
        "the delayed search open reached the server"
    );
}

/// A stale `GET /conversations/{id}` that ends with a `500` after the manual
/// selection must not show its error over the chosen chat.
#[gpui::test]
async fn stale_search_open_error_does_not_replace_the_manually_chosen_chat(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(500, r#"{"error":"failed"}"#)
            .with_path("/api/v1/conversations/50")
            .with_delay(Duration::from_millis(200)),
        Reply::new(200, &messages_page(vec![message(800, 8)]))
            .with_path("/api/v1/conversations/8/messages"),
        Reply::new(200, r#"{"conversation_id":8,"muted":false}"#)
            .with_path("/api/v1/conversations/8/notifications"),
        Reply::new(200, &messages_page(Vec::new())),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.conversations = vec![conversation(8, "Восемь", true, false)];
            v.open_conversation_message(50, 300, window, cx);
            v.select_conversation(conversation(8, "Восемь", true, false), window, cx);
        });
    });
    pump(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(8));
        assert_eq!(v.channel, 8);
        assert!(
            v.messages.iter().any(|row| row.id == 800),
            "the chosen chat loaded its own history"
        );
        assert!(
            v.messaging.search.error.is_empty(),
            "the stale error must not be painted over the chosen chat"
        );
        assert!(v.messaging.open_request.is_none());
    });
}

/// Two hits clicked in a row: the second click owns the navigation. The answer
/// for the first hit arrives later and must not replace the second chat.
#[gpui::test]
async fn two_search_hits_in_a_row_leave_navigation_to_the_last_one(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_delay(Duration::from_millis(200)),
        Reply::new(200, &messages_page(vec![message(400, 60)]))
            .with_path("/api/v1/conversations/60/messages?before=401"),
        Reply::new(200, &messages_page(Vec::new()))
            .with_path("/api/v1/conversations/60/messages"),
        Reply::new(200, r#"{"conversation_id":60,"muted":false}"#)
            .with_path("/api/v1/conversations/60/notifications"),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let second = visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            // The first hit is outside the index and starts the deferred open.
            v.open_conversation_message(50, 300, window, cx);
            assert!(v.messaging.open_request.is_some(), "the first hit owns a slot");
            // The second hit points at a chat the index already knows, so it
            // opens immediately and must own the navigation.
            v.conversations = vec![conversation(60, "Шесть", true, false)];
            v.open_conversation_message(60, 400, window, cx);
            v.messaging.open_request
        })
    });
    assert!(
        second.is_none(),
        "an immediate hit cannot leave a deferred slot behind"
    );
    pump(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(60),
            "the later hit owns the opened conversation"
        );
        assert_eq!(v.channel, 60);
        assert!(v.messages.iter().any(|row| row.id == 400));
        assert!(v.messages.iter().all(|row| row.id != 300));
        assert_eq!(v.messaging.open_request, None);
        assert!(v.messaging.search.error.is_empty());
    });
    assert!(
        server
            .requests()
            .iter()
            .any(|row| row.path == "/api/v1/conversations/50"),
        "the delayed search open reached the server"
    );
}

/// Two unloaded hits clicked in a row: the newer `open_request` replaces the
/// older one, so only the last answer may open its conversation.
#[gpui::test]
async fn two_unloaded_search_hits_keep_only_the_last_navigation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &conversation_json(50, "Пять"))
            .with_path("/api/v1/conversations/50")
            .with_delay(Duration::from_millis(100)),
        Reply::new(200, &conversation_json(60, "Шесть"))
            .with_path("/api/v1/conversations/60")
            .with_delay(Duration::from_millis(400)),
        Reply::new(200, &messages_page(Vec::new()))
            .with_path("/api/v1/conversations/60/messages"),
        Reply::new(200, r#"{"conversation_id":60,"muted":false}"#)
            .with_path("/api/v1/conversations/60/notifications"),
        Reply::new(200, &messages_page(Vec::new())),
        Reply::new(200, r#"{"conversation_id":50,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let slots = visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.open_conversation_message(50, 300, window, cx);
            let older = v.messaging.open_request.expect("the first hit owns a slot");
            v.open_conversation_message(60, 400, window, cx);
            (older, v.messaging.open_request.expect("the second hit owns a slot"))
        })
    });
    assert_ne!(slots.0, slots.1, "each hit gets a fresh request id");
    pump(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(60),
            "only the last hit owns the navigation"
        );
        assert_eq!(v.channel, 60);
        assert!(v.messaging.open_request.is_none());
        assert!(
            v.messaging.search.error.is_empty(),
            "the older answer must not be reported as a failure"
        );
    });
    let requests = server.requests();
    assert!(requests
        .iter()
        .any(|row| row.path == "/api/v1/conversations/50"));
    assert!(requests
        .iter()
        .any(|row| row.path == "/api/v1/conversations/60"));
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
        Reply::new(200, &messages_page(Vec::new()))
            .with_path("/api/v1/conversations/50/messages"),
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
    assert!(
        server
            .requests()
            .iter()
            .any(|row| row.path == "/api/v1/conversations/50"),
        "the delayed search open reached the server"
    );
}
