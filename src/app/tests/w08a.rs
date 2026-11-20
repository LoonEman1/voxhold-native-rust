//! W08a regressions for the personal "Сообщения" surface: bounded index
//! paging, creation, notification mode, leaving/re-invite and the return to
//! the channel context. HTTP is the real `TestServer`/`Gate` path.

use super::super::*;
use crate::messages;
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use serde_json::json;
use std::time::Duration;

fn make_app(
    cx: &mut TestAppContext,
    client: voxhold_api::Client,
) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-w08a-test-{}-{}",
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

fn wait_for(
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

fn release_on_arrival(gate: std::sync::Arc<Gate>) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let arrived = gate.wait_arrived(Duration::from_secs(5));
        gate.release();
        arrived
    })
}

fn conversation(
    id: i64,
    name: &str,
    direct: bool,
    unread: i64,
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
        "unread_count": unread,
        "saved": false,
        "muted": muted,
        "last_message_id": id * 10
    }))
    .unwrap()
}

fn rows(ids: impl IntoIterator<Item = i64>) -> Vec<voxhold_api::Conversation> {
    ids.into_iter()
        .map(|id| conversation(id, "", true, 0, false))
        .collect()
}

fn page(value: Vec<voxhold_api::Conversation>, has_more: bool, next: Option<&str>) -> String {
    json!({
        "conversations": value,
        "has_more": has_more,
        "next_cursor": next
    })
    .to_string()
}

fn empty_page() -> String {
    page(Vec::new(), false, None)
}

fn direct_client(server: &TestServer) -> voxhold_api::Client {
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    client
}

/// 13 rows in the first page plus an older page with one duplicate: every row
/// appears once and a second older request is refused while the first is live.
#[gpui::test]
async fn thirteen_conversations_page_without_duplicates(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let first = (1..=13).rev().collect::<Vec<_>>();
    let mut older = rows([1, 14, 15, 16, 17]);
    older[0] = conversation(1, "duplicate", true, 9, true);
    // The older page is below every row of the first page, exactly like the
    // backend's activity ordering.
    for row in &mut older {
        row.last_message_id = 0;
    }
    let server = TestServer::spawn(vec![
        Reply::new(200, &page(rows(first), true, Some("MTM6MQ"))),
        Reply::new(200, &page(older, false, None)),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.refresh_recents(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.messaging.loading && v.conversations.len() == 13
    });
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.load_older_conversations(cx);
            v.load_older_conversations(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.messaging.older_loading && v.conversations.len() == 17
    });
    view.read_with(&*visual, |v, _| {
        let ids: std::collections::HashSet<i64> =
            v.conversations.iter().map(|row| row.id).collect();
        assert_eq!(ids.len(), 17, "no duplicate row is appended");
        assert_eq!(v.conversations[0].id, 13);
        assert_eq!(
            v.conversations.iter().find(|row| row.id == 1).unwrap().name,
            "",
            "the existing row keeps its fields"
        );
        assert!(!v.messaging.has_more);
        assert!(v.messaging.cursor.is_none());
    });
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "the second older click is coalesced");
    assert!(requests[0]
        .path
        .starts_with("/api/v1/me/conversations?limit=30"));
    assert!(requests[1].path.contains("before=MTM6MQ"));
}

/// Leaving the personal surface invalidates an older page that was already in
/// flight. The callback's guard is tested directly so the ordering is stable.
#[gpui::test]
async fn stale_older_page_after_leaving_messages_is_ignored(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &page(rows((1..=13).rev()), true, Some("MTM6MQ")),
    )]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.refresh_recents(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.messaging.loading && v.conversations.len() == 13
    });
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.messaging.older_request = Some(41);
            v.messaging.older_loading = true;
        });
    });
    let (request, epoch, session) = view.read_with(&*visual, |v, _| {
        (41, v.messaging.epoch, v.session_generation)
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.accepts_older_conversations(request, epoch, session));
    });
    visual.update(|_, app| {
        view.update(app, |v, _| {
            // The real return/close path clears the slot and bumps the epoch.
            v.messaging.older_request = None;
            v.messaging.older_loading = false;
            v.messaging.epoch = v.messaging.epoch.wrapping_add(1);
            v.personal_mode = false;
        });
    });
    view.read_with(&*visual, |v, _| {
        assert!(
            !v.accepts_older_conversations(request, epoch, session),
            "the stale page did not append"
        );
        assert_eq!(v.conversations.len(), 13);
        assert!(!v.messaging.older_loading);
    });
}

/// Global message search uses `/me/conversations/search`; an empty query never
/// issues a request.
#[gpui::test]
async fn global_search_results_and_empty_query_error(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let hits = json!({
        "results": [
            {"id": 8, "conversation_id": 7, "author_id": 2, "author_username": "neva", "content": "budget line", "created_at": 1},
            {"id": 7, "conversation_id": 7, "author_id": 1, "author_username": "me", "content": "budget", "created_at": 2}
        ],
        "has_more": false,
        "next_before": null
    })
    .to_string();
    let server = TestServer::spawn(vec![Reply::new(200, &hits)]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.messaging.global = true;
            v.messaging
                .query
                .update(cx, |input, cx| input.set_value("budget", window, cx));
        });
    });
    visual.run_until_parked();
    visual.update(|_, app| {
        view.update(app, |v, cx| v.submit_messages_search(cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.messaging.search.loading && v.messaging.search.results.len() == 2
    });
    let before = server.requests().len();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.messaging
                .query
                .update(cx, |input, cx| input.set_value("", window, cx));
        });
    });
    visual.run_until_parked();
    visual.update(|_, app| {
        view.update(app, |v, cx| v.submit_messages_search(cx));
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.messaging.search.error, "Введите запрос.");
    });
    assert_eq!(
        server.requests().len(),
        before,
        "empty query has no request"
    );
    assert!(server.requests()[0]
        .path
        .contains("/api/v1/me/conversations/search?q=budget"));
}

/// A direct POST that the backend maps to the existing pair selects that one
/// row instead of adding a second copy.
#[gpui::test]
async fn creating_a_direct_reuses_the_existing_conversation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let existing = conversation(7, "", true, 0, false);
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &serde_json::to_string(&existing).unwrap(),
    )]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.conversations = vec![existing.clone()];
            v.open_conversation_creator(window, cx);
            v.toggle_creator_member(2, cx);
            v.submit_conversation_creator(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.as_ref().map(|row| row.id) == Some(7)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.conversations.len(), 1, "the existing direct is reused");
        assert_eq!(v.conversations[0].id, 7);
        assert!(v.messaging.creator.is_none());
        assert_eq!(v.chat_key(), draft::ChatKey::conversation(7));
    });
    let request = &server.requests()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/v1/me/conversations");
    assert!(String::from_utf8_lossy(&request.body).contains("\"user_ids\":[2]"));
}

/// A named group created from the form becomes the active conversation.
#[gpui::test]
async fn creating_a_named_group_selects_it(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let group = conversation(9, "Проект", false, 0, false);
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &serde_json::to_string(&group).unwrap(),
    )]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.open_conversation_creator(window, cx);
            v.toggle_creator_member(2, cx);
            v.toggle_creator_member(3, cx);
            let name = v.messaging.creator.as_ref().unwrap().name.clone();
            name.update(cx, |input, cx| input.set_value("Проект", window, cx));
            v.submit_conversation_creator(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.as_ref().map(|row| row.id) == Some(9)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.conversation.as_ref().unwrap().name, "Проект");
        assert_eq!(v.conversations[0].id, 9);
        assert_eq!(v.messaging.leave_confirm, false);
    });
    let body = String::from_utf8_lossy(&server.requests()[0].body).into_owned();
    assert!(body.contains("\"name\":\"Проект\""));
}

/// Invalid selection is refused locally, before any request.
#[gpui::test]
async fn invalid_creation_is_shown_without_a_request(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![]);
    let (window, view) = make_app(cx, direct_client(&server));
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.open_conversation_creator(window, cx);
            v.submit_conversation_creator(window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.messaging.creator.as_ref().unwrap().error.is_empty());
    });
    assert!(server.requests().is_empty());
    assert!(messages::validate_create(&[2], &"я".repeat(65)).is_err());
}

/// A privacy/block refusal from `POST /me/conversations` stays in the form and
/// does not select a conversation.
#[gpui::test]
async fn a_blocked_or_private_creation_error_is_surfaced(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(403, r#"{"error":"forbidden"}"#)]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.open_conversation_creator(window, cx);
            v.toggle_creator_member(2, cx);
            v.submit_conversation_creator(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging
            .creator
            .as_ref()
            .is_some_and(|creator| !creator.error.is_empty() && !creator.busy)
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation.is_none());
        assert_eq!(server.requests()[0].method, "POST");
    });
}

/// The mute PUT is impossible until the GET confirmed the mode; a failed PUT
/// keeps the last confirmed value.
#[gpui::test]
async fn notification_mute_waits_for_get_and_keeps_state_on_failure(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, r#"{"conversation_id":7,"muted":true}"#).with_gate(gate.clone()),
        Reply::new(500, r#"{"error":"failed"}"#),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.conversations = vec![conversation(7, "Проект", false, 0, false)];
            v.conversation = Some(conversation(7, "Проект", false, 0, false));
            v.channel = 7;
            v.load_conversation_notifications(cx);
            // The GET is still unanswered, so there is no confirmed mode yet.
            v.toggle_conversation_mute(cx);
            assert!(
                !v.messaging
                    .notifications
                    .as_ref()
                    .is_some_and(|note| note.pending),
                "no PUT before the GET is confirmed"
            );
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the GET reached the server");
    gate.release();
    for _ in 0..400 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| {
            v.messaging
                .notifications
                .as_ref()
                .is_some_and(|note| !note.pending && note.loaded)
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    view.read_with(&*visual, |v, _| {
        let note = v.messaging.notifications.as_ref().unwrap();
        assert!(
            note.loaded && note.muted,
            "GET confirmed muted=true: {note:?} requests={:?}",
            server.requests()
        );
        assert!(
            v.conversations[0].muted,
            "the row follows the confirmed GET"
        );
    });
    assert_eq!(server.requests().len(), 1);
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.toggle_conversation_mute(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging
            .notifications
            .as_ref()
            .is_some_and(|note| !note.pending && !note.error.is_empty())
    });
    view.read_with(&*visual, |v, _| {
        let note = v.messaging.notifications.as_ref().unwrap();
        assert!(note.muted, "the failed PUT did not flip the confirmed mode");
        assert!(v.conversations[0].muted, "the row keeps the old state");
    });
    assert_eq!(server.requests()[1].method, "PUT");
    // A confirmed PUT updates the row; the next request carries the mode the
    // user saw after the failure.
    visual.update(|_, app| {
        view.update(app, |v, cx| v.toggle_conversation_mute(cx));
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging
            .notifications
            .as_ref()
            .is_some_and(|note| !note.pending && note.error.is_empty() && !note.muted)
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.conversations[0].muted);
    });
    assert_eq!(server.requests()[2].method, "PUT");
}

/// Leaving is confirmed, clears the active ChatKey, and a history page that
/// was requested before the DELETE cannot bring the conversation back.
#[gpui::test]
async fn leave_clears_active_chat_and_reinvite_restores_the_index(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, r#"{"ok":true}"#),
        Reply::new(200, &page(Vec::new(), false, None)),
        Reply::new(
            200,
            &page(
                vec![conversation(7, "Проект", false, 0, false)],
                false,
                None,
            ),
        ),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let mut guard = None;
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.personal_return = Some((1, 0));
            v.conversations = vec![conversation(7, "Проект", false, 0, false)];
            v.conversation = Some(conversation(7, "Проект", false, 0, false));
            v.channel = 7;
            v.messages = vec![serde_json::from_value(json!({
                "id": 3, "channel_id": 0, "conversation_id": 7,
                "content": "old", "author": {"user_id": 1, "username": "me"},
                "created_at": 1, "revision": 1, "reactions": [], "attachments": []
            }))
            .unwrap()];
            // A refresh is already in flight when the leave starts. The guard
            // values are captured before the clear changes the generation.
            let request = 99;
            v.refresh_request = Some(request);
            guard = Some((
                request,
                v.generation,
                v.access_generation,
                v.chat_key(),
                v.server,
                v.channel,
            ));
            v.request_leave_conversation(cx);
            v.confirm_leave_conversation(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.is_none() && v.messages.is_empty()
    });
    let guard = guard.expect("guard");
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation.is_none(), "the active ChatKey is cleared");
        assert!(v.conversations.is_empty());
        assert_eq!(v.channel, 0);
        assert_eq!(v.chat_key().channel, 0, "no ChatKey points at the left row");
        assert!(
            !v.accepts_refresh_reply(guard.0, guard.1, guard.2, guard.3, guard.4, guard.5),
            "a history answer requested before the DELETE cannot repaint"
        );
    });

    // A new `conversation.changed` (re-invite/create) followed by the index
    // answer restores only the list entry.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.apply_realtime_event("conversation.changed", &json!({"conversation_id": 7}));
            v.refresh_recents(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversations.iter().any(|row| row.id == 7)
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation.is_none(), "re-invite does not auto-select");
    });
}

/// A message for another conversation changes its badge/order but not the
/// active window; a missing row after `conversation.changed` clears the key.
#[gpui::test]
async fn other_device_events_update_the_index_and_missing_row_clears_active(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &page(vec![conversation(2, "other", true, 0, false)], false, None),
    )]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.conversations = vec![
                conversation(2, "other", true, 0, false),
                conversation(1, "active", true, 0, false),
            ];
            v.conversation = Some(conversation(1, "active", true, 0, false));
            v.channel = 1;
            v.messages = vec![serde_json::from_value(json!({
                "id": 3, "channel_id": 0, "conversation_id": 1,
                "content": "keep", "author": {"user_id": 1, "username": "me"},
                "created_at": 1, "revision": 1, "reactions": [], "attachments": []
            }))
            .unwrap()];
            let effect = v.apply_realtime_event(
                "conversation.message",
                &json!({
                    "id": 50, "channel_id": 0, "conversation_id": 2,
                    "content": "new", "author": {"user_id": 2, "username": "neva"},
                    "created_at": 2, "revision": 1, "reactions": [], "attachments": []
                }),
            );
            assert!(effect.recents);
            assert_eq!(v.conversation.as_ref().unwrap().id, 1);
            assert_eq!(v.messages.len(), 1, "the active window is untouched");
            assert_eq!(v.conversations[0].id, 2, "the other row moved to the front");
            assert_eq!(v.conversations[0].unread_count, 1);
            assert_eq!(v.conversations[1].id, 1);

            v.apply_realtime_event("conversation.changed", &json!({"conversation_id": 1}));
            v.refresh_recents(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.is_none() && v.conversations_reconcile.is_none()
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.messages.is_empty());
        assert_eq!(v.channel, 0);
        assert!(v.conversations.iter().all(|row| row.id != 1));
    });
}

/// "К каналам" restores the saved channel only while it is still readable;
/// a revoked channel falls back instead of selecting it.
#[gpui::test]
async fn returning_to_channels_restores_only_permitted_context(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &empty_page()),
        Reply::new(200, &empty_page()),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
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
            v.channel = 7;
            v.enter_personal_mode(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| !v.messaging.loading);
    view.read_with(&*visual, |v, _| {
        assert!(v.personal_mode);
        assert_eq!(v.personal_return, Some((1, 7)));
        assert_eq!(v.channel, 0, "personal mode has no channel key");
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.return_to_channels(window, cx));
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.personal_mode);
        assert_eq!(v.channel, 7);
        assert_eq!(v.chat_key(), draft::ChatKey::channel(1, 7));
        assert!(v.conversation.is_none());
    });

    // Revoke 7, open the personal surface again, and return: only the first
    // permitted channel may be restored.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channels.retain(|channel| channel.id != 7);
            v.enter_personal_mode(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| !v.messaging.loading);
    visual.update(|window, app| {
        view.update(app, |v, cx| v.return_to_channels(window, cx));
    });
    view.read_with(&*visual, |v, _| {
        assert_ne!(v.channel, 7, "a revoked channel is not restored blindly");
        assert_eq!(v.channel, 8);
        assert_eq!(v.chat_key(), draft::ChatKey::channel(1, 8));
    });
}

/// 31 conversations, a partial first page and a conversation created after
/// that page was read: `conversation.changed` must not close the active row.
#[gpui::test]
async fn a_partial_index_never_closes_a_conversation_outside_the_page(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    // The first page holds 30 of 31 rows; the new conversation has activity 0
    // and therefore sits outside it.
    let created = conversation(1, "Новая", true, 0, false);
    let mut created_reply = serde_json::to_value(&created).unwrap();
    created_reply["last_message_id"] = json!(0);
    let created_reply = created_reply.to_string();
    // Parses as both the history page and the notification answer, so the two
    // post-selection requests cannot race over the queued reply.
    let history = json!({
        "conversation_id": 1,
        "muted": false,
        "messages": [],
        "pagination": {"next_before_id": null, "has_more": false}
    })
    .to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, &page(rows((2..=31).rev()), true, Some("FIRST"))),
        Reply::new(200, &created_reply),
        Reply::new(200, &history),
        Reply::new(200, &history),
        Reply::new(200, &page(rows((2..=31).rev()), true, Some("SECOND"))),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.refresh_recents(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.messaging.loading && v.conversations.len() == 30
    });

    // Create and select a direct conversation outside the loaded page.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.open_conversation_creator(window, cx);
            v.toggle_creator_member(2, cx);
            v.submit_conversation_creator(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.as_ref().map(|row| row.id) == Some(1)
    });
    for _ in 0..800 {
        visual.run_until_parked();
        if server.requests().len() >= 4 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        server.requests().len() >= 4,
        "the POST, history and notification answers settled: {:?}",
        server.requests()
    );

    visual.update(|_, app| {
        view.update(app, |v, cx| {
            let effect =
                v.apply_realtime_event("conversation.changed", &json!({"conversation_id": 1}));
            assert!(effect.recents);
            assert!(effect.resync);
            v.refresh_recents(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.messaging.loading && v.messaging.cursor.as_deref() == Some("SECOND")
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(1),
            "a partial page is not proof of deletion"
        );
        assert_eq!(v.channel, 1);
        assert!(
            v.conversations.iter().any(|row| row.id == 1),
            "the row outside the page is kept in the index"
        );
        assert_eq!(v.conversations.len(), 31, "no row is lost or duplicated");
        assert!(v.messaging.has_more);
        assert!(v.error.is_empty(), "no 'Диалог недоступен.': {}", v.error);
        assert!(
            v.conversations_reconcile.is_none(),
            "the marker is consumed"
        );
    });
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request
                .path
                .starts_with("/api/v1/me/conversations?limit=30"))
            .count(),
        2,
        "one first page per refresh: {requests:?}"
    );
}

/// Personal search/creator/notification state belongs to the signed-in
/// account. Logging out resets it, and an account without servers still loads
/// its conversation index.
#[gpui::test]
async fn logout_resets_personal_state_and_the_next_account_loads_its_index(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(
            200,
            r#"{"user":{"id":2,"username":"bob","created_at":1700000000},"session":{"token":"token-b","expires_at":4102444800}}"#,
        ),
        Reply::new(200, "[]"),
        Reply::new(200, &empty_page()),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            // Fill every personal slot the first account could leave behind.
            v.personal_mode = true;
            v.personal_return = Some((9, 7));
            v.open_conversation_creator(window, cx);
            v.conversations = vec![conversation(4, "старый", true, 3, true)];
            v.conversation = Some(conversation(4, "старый", true, 3, true));
            v.channel = 4;
            v.messages = vec![serde_json::from_value(json!({
                "id": 5, "channel_id": 0, "conversation_id": 4,
                "content": "old", "author": {"user_id": 1, "username": "me"},
                "created_at": 1, "revision": 1, "reactions": [], "attachments": []
            }))
            .unwrap()];
            v.messaging.global = true;
            v.messaging
                .search
                .results
                .push(voxhold_api::ConversationSearchHit {
                    id: 11,
                    conversation_id: 4,
                    author_id: 2,
                    author_username: "neva".into(),
                    content: "budget".into(),
                    created_at: 1,
                });
            v.messaging.search.submitted = "budget".into();
            v.messaging
                .query
                .update(cx, |input, cx| input.set_value("budget", window, cx));
            v.messaging.notifications = Some(messages::Notifications {
                id: 4,
                muted: true,
                loaded: true,
                pending: false,
                error: String::new(),
                request: 1,
            });
            v.messaging.leave_confirm = true;
            v.messaging.has_more = true;
            v.messaging.cursor = Some("OLD".into());
            v.messaging.error = "old error".into();
            v.conversations_reconcile = Some(messages::ConversationReconcile {
                conversation: 4,
                select_seq: v.conversation_select_seq,
                epoch: v.messaging.epoch,
                session: v.session_generation,
            });
            v.logout(window, cx);
        });
    });
    visual.run_until_parked();
    view.read_with(&*visual, |v, cx| {
        assert!(!v.personal_mode);
        assert!(v.personal_return.is_none());
        assert!(v.conversation.is_none());
        assert!(v.conversations.is_empty());
        assert!(v.messages.is_empty());
        assert!(!v.messaging.global);
        assert!(v.messaging.search.results.is_empty());
        assert!(v.messaging.search.submitted.is_empty());
        assert!(v.messaging.creator.is_none());
        assert!(v._creator_subscriptions.is_empty());
        assert!(v.messaging.notifications.is_none());
        assert!(!v.messaging.leave_confirm);
        assert!(!v.messaging.has_more);
        assert!(v.messaging.cursor.is_none());
        assert!(v.messaging.error.is_empty());
        assert!(v.messaging.query.read(cx).value().is_empty());
        assert!(v.conversations_reconcile.is_none());
        assert_eq!(v.channel, 0);
    });

    // The second account has no servers; the index request is still sent.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.auth.origin = server.base.clone();
            v.username
                .update(cx, |input, cx| input.set_value("bob", window, cx));
            v.password
                .update(cx, |input, cx| input.set_value("password123", window, cx));
            v.submit_login(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.self_user_id == 2 && v.client.is_some() && !v.messaging.loading
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.servers.is_empty());
        assert_eq!(v.server, 0);
        assert!(!v.personal_mode);
        assert!(v.conversations.is_empty());
        assert!(v.messaging.search.results.is_empty());
        assert!(v.messaging.creator.is_none());
        assert!(v.messaging.notifications.is_none());
    });
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request
                .path
                .starts_with("/api/v1/me/conversations?limit=30"))
            .count(),
        1,
        "the personal index is fetched without a selected server: {requests:?}"
    );
}

#[cfg(feature = "voxhold-media")]
fn draw_window(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

/// An active call keeps its dock and the account footer inside the personal
/// sidebar; the dock actions still control the call.
#[cfg(feature = "voxhold-media")]
#[gpui::test]
async fn personal_mode_keeps_the_voice_dock_and_account_controls(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::super::voice_tests::fixture(cx);
    draw_window(&mut visual);
    assert!(
        visual.debug_bounds("voice-dock").is_some(),
        "an active call keeps its dock under the channel list"
    );
    let server = TestServer::spawn(vec![Reply::new(200, &empty_page())]);
    visual.update(|_, app| {
        view.update(app, |v, _| v.client = Some(direct_client(&server)));
    });
    draw_window(&mut visual);
    let rail = visual
        .debug_bounds("messages-open")
        .expect("the rail Messages button");
    visual.simulate_mouse_move(rail.center(), None, Modifiers::default());
    visual.simulate_click(rail.center(), Modifiers::default());
    wait_for(&mut visual, &view, |v| {
        v.personal_mode && !v.messaging.loading
    });
    draw_window(&mut visual);
    view.read_with(&*visual, |v, app| {
        assert!(v.personal_mode);
        assert!(v.voice_dock_visible(app), "the call stays reachable");
    });
    assert!(
        visual.debug_bounds("voice-dock").is_some(),
        "the dock is painted in the personal sidebar"
    );
    assert!(
        visual.debug_bounds("account-footer").is_some(),
        "the account controls stay available"
    );
    assert!(visual.debug_bounds("messages-return-footer").is_some());

    let mic = visual
        .debug_bounds("voice-dock-action-voice-mic")
        .expect("the dock microphone action");
    let before = voice.read_with(&*visual, |panel, _| panel.action_active_flag("voice-mic"));
    visual.simulate_mouse_move(mic.center(), None, Modifiers::default());
    visual.simulate_click(mic.center(), Modifiers::default());
    draw_window(&mut visual);
    let after = voice.read_with(&*visual, |panel, _| panel.action_active_flag("voice-mic"));
    assert!(after, "the dock action still mutes the confirmed call");
    assert_ne!(before, after);
}
