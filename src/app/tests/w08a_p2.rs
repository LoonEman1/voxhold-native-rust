//! W08a P2 regressions: creator answer identity, conversation 403/404 cleanup,
//! mute retry, first-page loading/retry, drafts across server switches, personal
//! events independent of channel ACL and saved favorites in message search.

use super::super::*;
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{px, size, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;
use serde_json::json;
use std::time::Duration;

fn make_app(
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

fn draw(visual: &mut VisualTestContext) {
    visual.simulate_resize(size(px(1280.), px(900.)));
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
}

fn conversation(id: i64, name: &str, direct: bool, muted: bool) -> voxhold_api::Conversation {
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

fn page(value: Vec<voxhold_api::Conversation>) -> String {
    json!({
        "conversations": value,
        "has_more": false,
        "next_cursor": null
    })
    .to_string()
}

fn empty_page() -> String {
    page(Vec::new())
}

fn message(id: i64, conversation_id: i64) -> voxhold_api::Message {
    serde_json::from_value(json!({
        "id": id, "channel_id": 0, "conversation_id": conversation_id,
        "content": "row", "author": {"user_id": 1, "username": "me"},
        "created_at": 1, "revision": 1, "reactions": [], "attachments": []
    }))
    .unwrap()
}

fn direct_client(server: &TestServer) -> voxhold_api::Client {
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

/// A conversation POST rejected with 403 removes the ChatKey and reloads the
/// index instead of leaving a writable window with only text in the composer.
#[gpui::test]
async fn conversation_send_403_clears_the_active_chat_key(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(403, r#"{"error":"forbidden"}"#),
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
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.channel, 0);
        assert_eq!(v.chat_key().channel, 0, "no writable ChatKey points at 7");
        assert!(v.composer.read(cx).value().is_empty());
        assert!(
            v.conversations.is_empty(),
            "the index reload removed the row"
        );
    });
    let requests = server.requests();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/conversations/7/messages");
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
