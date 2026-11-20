//! W08a P2 regressions: creator answer identity, conversation 403/404 cleanup,
//! mute retry, first-page loading/retry, drafts across server switches, personal
//! events independent of channel ACL and saved favorites in message search.

use super::super::*;
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{px, size, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use serde_json::json;
use std::time::Duration;

pub(super) fn make_app(
    cx: &mut TestAppContext,
    client: voxhold_api::Client,
) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-w08a-p2-test-{}-{}",
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

pub(super) fn draw(visual: &mut VisualTestContext) {
    draw_at(visual, 1280., 900.);
}

pub(super) fn draw_at(visual: &mut VisualTestContext, width: f32, height: f32) {
    visual.simulate_resize(size(px(width), px(height)));
    for _ in 0..2 {
        visual.update(|window, app| {
            window.refresh();
            window.draw(app).clear();
        });
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

pub(super) fn page(value: Vec<voxhold_api::Conversation>) -> String {
    json!({
        "conversations": value,
        "has_more": false,
        "next_cursor": null
    })
    .to_string()
}

pub(super) fn empty_page() -> String {
    page(Vec::new())
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

/// A creator closed and reopened after a submit gets a fresh request id. The
/// older answer must not select its row or close the new form.
#[gpui::test]
async fn stale_creator_answer_does_not_touch_the_reopened_form(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let old = conversation(8, "", true, false);
    let new = conversation(9, "", true, false);
    let server = TestServer::spawn(vec![
        Reply::new(200, &serde_json::to_string(&old).unwrap()),
        Reply::new(200, &serde_json::to_string(&new).unwrap())
            .with_delay(Duration::from_millis(300)),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (old_request, new_request) = visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.open_conversation_creator(window, cx);
            v.toggle_creator_member(2, cx);
            v.submit_conversation_creator(window, cx);
            let old_request = v.messaging.creator.as_ref().unwrap().request;
            // Everything below runs before the first background callback can
            // leave the UI loop, so the old answer is still pending while the
            // new form submits its own request.
            v.close_conversation_creator(cx);
            v.open_conversation_creator(window, cx);
            v.toggle_creator_member(3, cx);
            v.submit_conversation_creator(window, cx);
            let new_request = v.messaging.creator.as_ref().unwrap().request;
            (old_request, new_request)
        })
    });
    assert_ne!(old_request, new_request, "global request ids never collide");
    // The old answer is immediate and the new one is delayed. Both callbacks
    // run when the executor is pumped, old first: with a per-creator counter it
    // would select row 8 and close the new form before row 9 arrives.
    wait_for(&mut visual, &view, |v| {
        v.messaging.creator.is_none() && v.conversation.is_some()
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(9),
            "the old answer cannot select its row or close the new form"
        );
        assert_eq!(v.conversations[0].id, 9, "only the new row was inserted");
    });
}

/// A conversation send `403` may be a `canContact` refusal while both members
/// stay in the conversation. One explicit GET keeps the chat, its draft and
/// its composer text when it confirms the row.
#[gpui::test]
async fn conversation_send_403_keeps_the_chat_when_the_get_confirms_it(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let row = conversation(7, "Проект", true, false);
    let server = TestServer::spawn(vec![
        Reply::new(403, r#"{"error":"forbidden"}"#),
        Reply::new(200, &serde_json::to_string(&row).unwrap()),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Проект", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("hello", window, cx));
            v.send(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging.conversation_verify_request.is_none() && !v.composer_error.is_empty()
    });
    view.read_with(&*visual, |v, cx| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(7),
            "a contact refusal keeps the conversation open"
        );
        assert_eq!(v.channel, 7);
        assert_eq!(v.chat_key(), draft::ChatKey::conversation(7));
        assert_eq!(v.composer.read(cx).value(), "hello");
        assert_eq!(
            v.drafts.borrow().text(draft::ChatKey::conversation(7)),
            "hello",
            "the draft survives the ambiguity"
        );
        assert_eq!(v.composer_error, "Недостаточно прав доступа.");
        assert_eq!(v.conversations.len(), 1, "the row stays in the index");
    });
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "one POST and one explicit GET");
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/conversations/7/messages");
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/api/v1/conversations/7");
}

/// A transient verification failure is not proof of revocation: the chat,
/// draft and composer stay, and the send error remains visible.
#[gpui::test]
async fn conversation_send_403_survives_a_transient_verification_failure(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(403, r#"{"error":"forbidden"}"#),
        Reply::new(500, r#"{"error":"failed"}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Проект", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("hello", window, cx));
            v.send(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging.conversation_verify_request.is_none()
            && v.composer_error == "Недостаточно прав доступа."
    });
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(7));
        assert_eq!(v.chat_key(), draft::ChatKey::conversation(7));
        assert_eq!(v.composer.read(cx).value(), "hello");
        assert_eq!(
            v.drafts.borrow().text(draft::ChatKey::conversation(7)),
            "hello"
        );
        assert_eq!(v.conversations.len(), 1);
    });
    assert_eq!(server.requests()[1].method, "GET");
    assert_eq!(server.requests()[1].path, "/api/v1/conversations/7");
}

/// A send `403` whose verification GET also answers `403` is a real
/// revocation: the ChatKey, index row and draft are dropped.
#[gpui::test]
async fn conversation_send_403_is_cleared_when_the_get_is_forbidden(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(403, r#"{"error":"forbidden"}"#),
        Reply::new(403, r#"{"error":"forbidden"}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Проект", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("hello", window, cx));
            v.send(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.is_none()
            && v.error == "Диалог недоступен."
            && v.messaging.conversation_verify_request.is_none()
    });
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.channel, 0);
        assert_eq!(v.chat_key().channel, 0);
        assert!(v.composer.read(cx).value().is_empty());
        assert!(v.conversations.is_empty(), "the revoked row is gone");
        assert!(v
            .drafts
            .borrow()
            .text(draft::ChatKey::conversation(7))
            .is_empty());
    });
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/api/v1/conversations/7");
}

/// A `404` on the send itself is already authoritative and clears at once.
#[gpui::test]
async fn conversation_send_404_clears_the_active_chat_key(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(404, r#"{"error":"missing"}"#),
        Reply::new(200, &empty_page()),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Проект", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("hello", window, cx));
            v.send(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.is_none() && v.error == "Диалог недоступен." && !v.messaging.loading
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.channel, 0);
        assert_eq!(v.chat_key().channel, 0);
        assert!(v.conversations.is_empty());
        assert!(v.messaging.conversation_verify_request.is_none());
    });
    let requests = server.requests();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/conversations/7/messages");
    assert_eq!(
        requests.len(),
        2,
        "no explicit GET after an authoritative 404"
    );
    assert!(requests[1]
        .path
        .starts_with("/api/v1/me/conversations?limit=30"));
}

/// The same rule applies to a read receipt: a revoked conversation must not
/// keep painting the writable surface.
#[gpui::test]
async fn conversation_read_403_clears_the_active_chat_key(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(403, r#"{"error":"forbidden"}"#),
        Reply::new(200, &empty_page()),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, _app| window.activate_window());
    draw(&mut visual);
    visual.run_until_parked();
    visual.update(|window, _app| window.refresh());
    visual.run_until_parked();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Проект", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.messages = vec![message(5, 7)];
            v.read_sent = 0;
            v.access_pending = false;
            v.mark_visible_read(5, window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.conversation.is_none() && v.error == "Диалог недоступен." && !v.messaging.loading
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.channel, 0);
        assert_eq!(v.chat_key().channel, 0);
        assert!(v.conversations.is_empty());
    });
    let requests = server.requests();
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].path, "/api/v1/conversations/7/read");
}

/// A failed notification GET surfaces its error and the next click retries the
/// GET; only then can a PUT change the confirmed mode.
#[gpui::test]
async fn mute_get_failure_is_visible_and_retry_recovers(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(500, r#"{"error":"failed"}"#),
        Reply::new(200, r#"{"conversation_id":7,"muted":true}"#),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Проект", false, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.load_conversation_notifications(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging
            .notifications
            .as_ref()
            .is_some_and(|note| !note.pending && !note.loaded && !note.error.is_empty())
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("conversation-mute-error").is_some(),
        "the GET error is painted next to the mute action"
    );
    visual.update(|_, app| {
        view.update(app, |v, cx| v.activate_conversation_mute(cx));
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging
            .notifications
            .as_ref()
            .is_some_and(|note| note.loaded && note.muted && note.error.is_empty())
    });
    assert!(server.requests()[1]
        .path
        .ends_with("/api/v1/conversations/7/notifications"));
    visual.update(|_, app| {
        view.update(app, |v, cx| v.activate_conversation_mute(cx));
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging
            .notifications
            .as_ref()
            .is_some_and(|note| !note.pending && !note.muted && note.error.is_empty())
    });
    assert_eq!(server.requests()[2].method, "PUT");
}

/// The first index page paints "Загрузка…" while it is in flight and offers an
/// explicit retry after a failure.
#[gpui::test]
async fn first_index_page_shows_loading_and_retry(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(500, r#"{"error":"failed"}"#).with_gate(gate.clone()),
        Reply::new(200, &page(vec![conversation(7, "Проект", true, false)])),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.refresh_recents(cx);
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("messages-index-loading").is_some(),
        "the first-page loading state is painted"
    );
    gate.release();
    wait_for(&mut visual, &view, |v| {
        !v.messaging.loading && !v.messaging.error.is_empty()
    });
    draw(&mut visual);
    let retry = visual
        .debug_bounds("messages-index-retry")
        .expect("the failed first page offers Повторить");
    visual.simulate_click(retry.center(), Modifiers::default());
    wait_for(&mut visual, &view, |v| {
        !v.messaging.loading && v.messaging.error.is_empty() && v.conversations.len() == 1
    });
    assert_eq!(server.requests().len(), 2);
}

/// Leaving personal mode through the server picker saves the open DM under its
/// own key, never under a channel key derived from the conversation id.
#[gpui::test]
async fn server_switch_from_personal_mode_keeps_the_dm_draft(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &empty_page()),
        Reply::new(200, "[]"),
        Reply::new(200, "[]"),
        Reply::new(200, r#"{"messages":[],"has_more":false}"#),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Нева", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row.clone());
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("черновик ЛС", window, cx));
            v.open_server(2, window, cx);
            assert_eq!(
                v.drafts.borrow().text(draft::ChatKey::conversation(7)),
                "черновик ЛС",
                "the DM keeps its own draft"
            );
            assert!(
                v.drafts
                    .borrow()
                    .text(draft::ChatKey::channel(1, 7))
                    .is_empty(),
                "no channel draft appears from the conversation id"
            );
            // Restore the same conversation: the composer reads the DM key.
            v.personal_mode = true;
            v.conversation = None;
            v.channel = 0;
            v.select_conversation(row, window, cx);
            assert_eq!(v.composer.read(cx).value(), "черновик ЛС");
        });
    });
}

/// Removing the server while personal mode is open also preserves the DM draft.
#[gpui::test]
async fn server_removal_from_personal_mode_keeps_the_dm_draft(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(200, &empty_page())]);
    let (window, view) = make_app(cx, direct_client(&server));
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Нева", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("черновик ЛС", window, cx));
            v.remove_current_server(window, cx);
            assert_eq!(
                v.drafts.borrow().text(draft::ChatKey::conversation(7)),
                "черновик ЛС"
            );
            assert!(v
                .drafts
                .borrow()
                .text(draft::ChatKey::channel(1, 7))
                .is_empty());
        });
    });
}

/// A pending channel ACL reload never gates the personal index or selection.
#[gpui::test]
async fn conversation_events_and_selection_ignore_pending_channel_access(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, r#"{"messages":[],"has_more":false}"#),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.access_pending = true;
            v.conversations = vec![
                conversation(7, "active", true, false),
                conversation(8, "other", true, false),
            ];
            v.conversation = Some(conversation(7, "active", true, false));
            v.channel = 7;
            let effect = v.apply_realtime_event(
                "conversation.message",
                &json!({
                    "id": 50, "channel_id": 0, "conversation_id": 8,
                    "content": "new", "author": {"user_id": 2, "username": "neva"},
                    "created_at": 2, "revision": 1, "reactions": [], "attachments": []
                }),
            );
            assert!(effect.recents);
            assert_eq!(v.conversations[0].id, 8, "the index moved while ACL loads");
            let effect =
                v.apply_realtime_event("conversation.changed", &json!({"conversation_id": 8}));
            assert!(effect.recents);
            v.select_conversation(conversation(7, "active", true, false), window, cx);
            assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(7));
            assert_eq!(v.channel, 7);
        });
    });
}

/// A saved favorites hit from `/me/conversations/search` is not opened as a
/// normal conversation; it is removed from the current result set.
#[gpui::test]
async fn saved_favorite_search_hit_is_not_opened_as_a_normal_conversation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let mut saved = conversation(50, "Избранное", false, false);
    saved.saved = true;
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &serde_json::to_string(&saved).unwrap(),
    )]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.global = true;
            v.messaging.search.results = vec![voxhold_api::ConversationSearchHit {
                id: 3,
                conversation_id: 50,
                author_id: 2,
                author_username: "neva".into(),
                content: "saved".into(),
                created_at: 1,
            }];
            v.open_conversation_message(50, 3, window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging.open_request.is_none() && !v.messaging.search.error.is_empty()
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.conversation.is_none(), "favorites have no normal ChatKey");
        assert!(
            v.messaging.search.results.is_empty(),
            "the hit is not offered"
        );
        assert!(v.messaging.search.error.contains("Избранные сообщения"));
    });
    assert_eq!(server.requests()[0].path, "/api/v1/conversations/50");
}

/// Opening the "+" creator suspends the open chat without destroying its
/// composition; reopening restores the text and the attachment metadata.
#[gpui::test]
async fn opening_the_creator_keeps_the_suspended_draft_and_attachments(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, r#"{"messages":[],"has_more":false}"#),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let path = std::env::temp_dir().join(format!(
        "voxhold-w08a-creator-{}-{}.bin",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, vec![3u8; 32]).unwrap();
    let key = draft::ChatKey::conversation(7);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Нева", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.drafts.borrow_mut().set_text(key, "черновик ЛС");
            v.drafts.borrow_mut().add_files(
                key,
                vec![draft::DraftFile::from_path(path.clone()).unwrap()],
            );
            v.composer
                .update(cx, |input, cx| input.set_value("черновик ЛС", window, cx));

            v.open_conversation_creator(window, cx);
            assert!(v.messaging.creator.is_some(), "the creator is open");
            assert!(v.conversation.is_none(), "the chat surface is suspended");
            assert_eq!(v.channel, 0);
            assert_eq!(
                v.drafts.borrow().text(key),
                "черновик ЛС",
                "the suspended draft is saved under its conversation key"
            );
            assert_eq!(v.drafts.borrow().files(key).len(), 1);
            assert!(path.exists(), "temporary attachment files stay on disk");

            v.close_conversation_creator(cx);
            v.select_conversation(conversation(7, "Нева", true, false), window, cx);
            assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(7));
            assert_eq!(v.chat_key(), key);
            assert_eq!(v.composer.read(cx).value(), "черновик ЛС");
            assert_eq!(v.drafts.borrow().files(key).len(), 1);
            assert_eq!(v.drafts.borrow().files(key)[0].path, path);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging
            .notifications
            .as_ref()
            .is_some_and(|note| note.loaded)
    });
    std::fs::remove_file(path).ok();
}

/// An in-flight send completes normally while the creator is open; opening the
/// form neither aborts the request nor drops the draft saved on suspend.
#[gpui::test]
async fn opening_the_creator_does_not_abort_an_inflight_send(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let mut fresh = conversation(7, "Нева", true, false);
    fresh.last_message_id = 99;
    let server = TestServer::spawn(vec![
        Reply::new(200, &serde_json::to_string(&message(70, 7)).unwrap()).with_gate(gate.clone()),
        Reply::new(200, &page(vec![fresh])),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let key = draft::ChatKey::conversation(7);
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Нева", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("первая", window, cx));
            v.send(window, cx);
            assert!(v.pending_sends.contains_key(&key), "the send is in flight");

            // The user edits the draft, then opens the creator before the
            // gated answer arrives.
            v.composer
                .update(cx, |input, cx| input.set_value("вторая версия", window, cx));
            v.open_conversation_creator(window, cx);
            assert!(v.messaging.creator.is_some());
            assert!(
                v.pending_sends.contains_key(&key),
                "opening the creator must not abort the send"
            );
            assert_eq!(v.drafts.borrow().text(key), "вторая версия");
        });
    });
    assert!(
        gate.wait_arrived(Duration::from_secs(5)),
        "the send reached the server"
    );
    gate.release();
    wait_for(&mut visual, &view, |v| {
        v.pending_sends.is_empty()
            && !v.messaging.loading
            && v.conversations.first().map(|row| row.last_message_id) == Some(99)
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.messaging.creator.is_some(), "the creator stays open");
        assert!(v.conversation.is_none());
        assert_eq!(
            v.drafts.borrow().text(key),
            "вторая версия",
            "the completed send does not drop the newer saved draft"
        );
    });
    assert_eq!(server.requests()[0].method, "POST");
    assert!(
        server.requests().len() >= 2,
        "the send answer refreshed recents"
    );
}

/// A confirmed DM send merges its row while a channel ACL reload is pending.
/// A replayed send (`replayed=true`) emits no `conversation.message` echo, so
/// the client must put the row into the feed from the POST answer itself.
#[gpui::test]
async fn conversation_send_merges_while_channel_access_is_pending(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &serde_json::to_string(&message(70, 7)).unwrap()),
        Reply::new(200, &page(vec![conversation(7, "Нева", true, false)])),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let key = draft::ChatKey::conversation(7);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Нева", true, false);
            v.personal_mode = true;
            // A channel ACL reload is pending while the DM is open.
            v.access_pending = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("replay", window, cx));
            v.send(window, cx);
            assert!(v.pending_sends.contains_key(&key));
        });
    });
    // No `conversation.message` echo is delivered: this is the replay case.
    wait_for(&mut visual, &view, |v| {
        v.pending_sends.is_empty() && !v.messaging.loading
    });
    view.read_with(&*visual, |v, cx| {
        assert!(
            v.messages.iter().any(|message| message.id == 70),
            "the confirmed DM row is merged without a WS echo"
        );
        assert_eq!(v.composer.read(cx).value(), "");
        assert_eq!(v.drafts.borrow().text(key), "");
        assert!(v.composer_error.is_empty());
        assert!(
            v.access_pending,
            "the channel ACL reload is still pending, not cleared by the DM send"
        );
    });
    assert_eq!(server.requests()[0].method, "POST");
    assert_eq!(
        server.requests()[0].path,
        "/api/v1/conversations/7/messages"
    );
}

pub(super) fn channel_row(id: i64, name: &str) -> voxhold_api::Channel {
    voxhold_api::Channel {
        id,
        name: name.into(),
        kind: "text".into(),
        permissions: 3,
        ..Default::default()
    }
}

/// A channel ACL reload survives opening a DM: its answer still applies the
/// fresh channel list and clears `voice_unconfirmed`, without blocking the
/// personal selection.
#[gpui::test]
async fn channel_acl_reload_survives_opening_a_dm(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let channels = serde_json::to_string(&vec![channel_row(5, "general")]).unwrap();
    let server = TestServer::spawn(vec![
        Reply::new(200, &channels)
            .with_path("/api/v1/servers/1/channels")
            .with_gate(gate.clone()),
        Reply::new(200, "[]").with_path("/api/v1/servers/1/members"),
        Reply::new(200, r#"{"messages":[],"has_more":false}"#)
            .with_path("/api/v1/conversations/7/messages"),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#)
            .with_path("/api/v1/conversations/7/notifications"),
        Reply::new(200, &page(vec![conversation(7, "Нева", true, false)]))
            .with_path("/api/v1/me/conversations"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 5;
            v.personal_mode = false;
            #[cfg(feature = "voxhold-media")]
            v.voice_unconfirmed.insert(1);
            gate.arm();
            v.invalidate_access(window, cx);
            assert!(v.access_pending);
            assert!(v.access_reload_in_flight);
            let access = v.access_generation;
            // The reload was captured before the DM selection; opening the DM
            // must not bump it or clear the pending state.
            v.personal_mode = true;
            v.personal_return = Some((1, 5));
            v.select_conversation(conversation(7, "Нева", true, false), window, cx);
            assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(7));
            assert_eq!(v.channel, 7, "personal selection is not blocked");
            assert_eq!(
                v.access_generation, access,
                "opening a DM keeps the pending ACL generation"
            );
            assert!(v.access_pending, "the channel ACL reload is still pending");
        });
    });
    gate.release();
    wait_for(&mut visual, &view, |v| {
        !v.access_pending
            && !v.access_reload_in_flight
            && v.channels.iter().any(|channel| channel.id == 5)
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(7),
            "the ACL answer does not close the personal conversation"
        );
        #[cfg(feature = "voxhold-media")]
        assert!(
            !v.voice_unconfirmed.contains(&1),
            "the fresh channel list confirms voice access again"
        );
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.return_to_channels(window, cx));
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.personal_mode);
        assert!(v.conversation.is_none());
        assert_eq!(
            v.channel, 5,
            "the saved channel reopens from the fresh list"
        );
    });
}
