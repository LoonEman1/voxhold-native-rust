//! W08a P2 follow-up regressions: return fallback during a gated ACL reload
//! (including the preserved channel draft and a failed reload), a replacement
//! reload for a second `channels.changed`, the mobile personal entry,
//! banner/F5 DM behaviour, stale older pages, post-switch 403/404 cleanup,
//! DM reaction verification, leave after a switch and members restore.

use super::super::*;
use super::w08a_p2::{
    channel_row, conversation, direct_client, draw, draw_at, empty_page, make_app, message, page,
    wait_for,
};
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn apply_event_now(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    kind: &str,
    data: serde_json::Value,
) {
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    tx.send(voxhold_api::RealtimeEvent {
        kind: kind.into(),
        data,
        ..Default::default()
    })
    .unwrap();
    drop(tx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.events = Some(Arc::new(Mutex::new(rx)));
            v.events_overflow = Default::default();
            v.process_realtime_batch(window, cx);
        });
    });
}

fn release_on_arrival(gate: Arc<Gate>) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let arrived = gate.wait_arrived(Duration::from_secs(5));
        gate.release();
        arrived
    })
}

fn messages_page() -> String {
    json!({
        "messages": [],
        "pagination": {"next_before_id": null, "has_more": false}
    })
    .to_string()
}

/// "К каналам" during a gated ACL reload restores the saved channel; when the
/// fresh list no longer contains it, the first readable channel is selected
/// exactly like the non-pending return path.
#[gpui::test]
async fn return_during_pending_acl_reload_uses_the_first_readable_channel(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let channels = serde_json::to_string(&vec![channel_row(5, "kept")]).unwrap();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &channels)
            .with_path("/api/v1/servers/1/channels")
            .with_gate(gate.clone()),
        Reply::new(200, "[]").with_path("/api/v1/servers/1/members"),
        Reply::new(200, &empty_page()).with_path("/api/v1/me/conversations"),
        Reply::new(200, &messages_page()).with_path("/api/v1/servers/1/channels/5/messages"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channels = vec![channel_row(9, "revoked")];
            v.channel = 9;
            gate.arm();
            v.invalidate_access(window, cx);
            assert!(v.access_pending && v.access_reload_in_flight);
            v.enter_personal_mode(window, cx);
            v.return_to_channels(window, cx);
            assert!(!v.personal_mode);
            assert_eq!(
                v.channel, 9,
                "the saved channel waits for the authoritative answer"
            );
            assert_eq!(v.pending_return_channel, Some(9));
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the reload reached the server");
    wait_for(&mut visual, &view, |v| {
        !v.access_pending && !v.access_reload_in_flight && v.channel == 5
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.personal_mode);
        assert_eq!(v.channel, 5, "revoked saved channel falls back");
        assert_eq!(v.pending_return_channel, None);
    });
    visual.run_until_parked();
    assert!(
        server.requests().iter().any(|request| request
            .path
            .starts_with("/api/v1/servers/1/channels/5/messages")),
        "the fallback channel actually opened"
    );
}

/// "К каналам" during a gated ACL reload must not overwrite the saved channel
/// draft: the pending branch restores the text before the callback selects the
/// channel, so the empty composer from the personal surface never clobbers it.
#[gpui::test]
async fn return_during_pending_acl_reload_keeps_the_channel_draft(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let channels = serde_json::to_string(&vec![channel_row(9, "kept")]).unwrap();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &channels)
            .with_path("/api/v1/servers/1/channels")
            .with_gate(gate.clone()),
        Reply::new(200, "[]").with_path("/api/v1/servers/1/members"),
        Reply::new(200, &empty_page()).with_path("/api/v1/me/conversations"),
        Reply::new(200, &messages_page()).with_path("/api/v1/servers/1/channels/9/messages"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    let key = draft::ChatKey::channel(1, 9);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channels = vec![channel_row(9, "kept")];
            v.channel = 9;
            v.composer
                .update(cx, |input, cx| input.set_value("hello", window, cx));
            v.drafts.borrow_mut().set_text(key, "hello");
            gate.arm();
            v.invalidate_access(window, cx);
            assert!(v.access_pending && v.access_reload_in_flight);
            v.enter_personal_mode(window, cx);
            assert_eq!(v.drafts.borrow().text(key), "hello");
            v.return_to_channels(window, cx);
            assert!(!v.personal_mode);
            assert_eq!(v.channel, 9);
            assert_eq!(v.pending_return_channel, Some(9));
            assert_eq!(
                v.composer.read(cx).value(),
                "hello",
                "the pending return restores the saved channel text"
            );
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the reload reached the server");
    wait_for(&mut visual, &view, |v| {
        !v.access_pending && !v.access_reload_in_flight && v.channel == 9
    });
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.pending_return_channel, None);
        assert_eq!(
            v.drafts.borrow().text(key),
            "hello",
            "the callback did not write the empty composer over the draft"
        );
        assert_eq!(v.composer.read(cx).value(), "hello");
    });
}

/// A non-403 reload failure releases the pending return so a later unrelated
/// success cannot auto-fallback into the channel.
#[gpui::test]
async fn failed_acl_reload_clears_the_pending_return(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(500, r#"{"error":"failed"}"#)
            .with_path("/api/v1/servers/1/channels")
            .with_gate(gate.clone()),
        Reply::new(200, "[]").with_path("/api/v1/servers/1/members"),
        Reply::new(200, &empty_page()).with_path("/api/v1/me/conversations"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channels = vec![channel_row(9, "kept")];
            v.channel = 9;
            gate.arm();
            v.invalidate_access(window, cx);
            v.enter_personal_mode(window, cx);
            v.return_to_channels(window, cx);
            assert_eq!(v.pending_return_channel, Some(9));
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the reload reached the server");
    wait_for(&mut visual, &view, |v| {
        !v.access_reload_in_flight && !v.error.is_empty()
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.pending_return_channel, None,
            "a failed reload releases the pending channel"
        );
        assert_eq!(v.channel, 9, "the saved channel stays the open target");
    });
}

/// A second `channels.changed` while a reload and a DM are both open replaces
/// the stale request instead of clearing the flag into a no-op.
#[gpui::test]
async fn second_channels_changed_while_a_dm_is_open_restarts_the_reload(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate_first = Gate::new();
    let first = serde_json::to_string(&vec![channel_row(5, "old")]).unwrap();
    let second = serde_json::to_string(&vec![channel_row(6, "fresh")]).unwrap();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &first)
            .with_path("/api/v1/servers/1/channels")
            .with_gate(gate_first.clone()),
        Reply::new(200, &second)
            .with_path("/api/v1/servers/1/channels")
            .with_delay(Duration::from_millis(400)),
        Reply::new(200, "[]").with_path("/api/v1/servers/1/members"),
        Reply::new(200, &messages_page()).with_path("/api/v1/conversations/7/messages"),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#)
            .with_path("/api/v1/conversations/7/notifications"),
        Reply::new(200, &empty_page()).with_path("/api/v1/me/conversations"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.channels = vec![channel_row(5, "old")];
            v.channel = 5;
            v.server = 1;
            gate_first.arm();
        });
    });
    apply_event_now(
        &mut visual,
        &view,
        "channels.changed",
        json!({"server_id": 1}),
    );
    view.read_with(&*visual, |v, _| {
        assert!(v.access_pending && v.access_reload_in_flight);
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.select_conversation(conversation(7, "Нева", true, false), window, cx);
            assert!(v.in_conversation());
        });
    });
    apply_event_now(
        &mut visual,
        &view,
        "channels.changed",
        json!({"server_id": 1}),
    );
    view.read_with(&*visual, |v, _| {
        assert!(
            v.access_refresh_again,
            "the in-flight answer is stale after the second event"
        );
        assert!(v.access_pending);
    });
    let waiter = release_on_arrival(gate_first.clone());
    visual.run_until_parked();
    assert!(
        waiter.join().unwrap(),
        "the first reload reached the server"
    );
    wait_for(&mut visual, &view, |v| {
        !v.access_pending && !v.access_reload_in_flight && v.channels[0].id == 6
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(7));
        assert!(!v.access_refresh_again);
    });
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.path == "/api/v1/servers/1/channels")
            .count(),
        2,
        "exactly one replacement reload"
    );
}

/// The mobile bottom bar is the only personal entry while the rail is hidden.
#[gpui::test]
async fn mobile_bottom_bar_enters_and_leaves_personal_mode(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(200, &empty_page())]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, _app| window.activate_window());
    draw_at(&mut visual, 390., 760.);
    let width = view.read_with(&*visual, |v, _| v.viewport.0);
    assert!(
        width <= crate::layout::MOBILE_MAX_WIDTH,
        "the test window is at mobile width ({width})"
    );
    let button = visual
        .debug_bounds("bottom-messages")
        .expect("the mobile bar offers Сообщения");
    visual.simulate_click(button.center(), Modifiers::default());
    wait_for(&mut visual, &view, |v| {
        v.personal_mode && !v.messaging.loading
    });
    draw_at(&mut visual, 390., 760.);
    view.read_with(&*visual, |v, _| {
        assert!(v.personal_mode);
        assert!(v.show_channels, "the personal list is visible after entry");
    });
    assert!(
        visual.debug_bounds("messages-return-footer").is_some(),
        "the mobile personal panel opened"
    );
    let button = visual
        .debug_bounds("bottom-messages")
        .expect("the bar now offers the return path");
    visual.simulate_click(button.center(), Modifiers::default());
    wait_for(&mut visual, &view, |v| !v.personal_mode);
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.channel, 0);
        assert!(v.conversation.is_none());
    });
}

/// The channel ACL banner belongs to the channel surface and must not paint
/// under an open DM.
#[gpui::test]
async fn acl_banner_is_hidden_in_an_open_dm(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, _| {
            let row = conversation(7, "Нева", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.access_pending = true;
            v.access_reload_in_flight = true;
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("channel-access-pending").is_none(),
        "no channel ACL block below personal history"
    );
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.conversation = None;
            v.channel = 0;
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("channel-access-pending").is_none(),
        "the empty personal center is not a channel surface"
    );
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.personal_mode = false;
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("channel-access-pending").is_some(),
        "the channel surface still shows the pending ACL state"
    );
}

/// F5 in a DM refreshes the conversation even when a channel reload is pending.
#[gpui::test]
async fn f5_refreshes_an_open_dm_while_access_is_pending(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(200, &messages_page())]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Нева", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.access_pending = true;
            v.composer.update(cx, |input, cx| input.focus(window, cx));
        });
        window.activate_window();
    });
    draw(&mut visual);
    visual.simulate_keystrokes("f5");
    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none()
            && server
                .requests()
                .iter()
                .any(|request| request.path == "/api/v1/conversations/7/messages")
    });
    assert!(
        server
            .requests()
            .iter()
            .any(|request| request.path == "/api/v1/conversations/7/messages"),
        "F5 issued the conversation history request"
    );
    view.read_with(&*visual, |v, _| {
        assert!(v.access_pending, "the channel ACL state is unchanged");
        assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(7));
    });
}

/// A send `403` that arrives after the user switched away still verifies and
/// removes the revoked row, without touching the new active chat.
#[gpui::test]
async fn send_403_after_a_switch_removes_the_revoked_row(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(403, r#"{"error":"forbidden"}"#)
            .with_path("/api/v1/conversations/7/messages")
            .with_gate(gate.clone()),
        Reply::new(403, r#"{"error":"forbidden"}"#).with_path("/api/v1/conversations/7"),
        Reply::new(200, &messages_page()).with_path("/api/v1/conversations/8/messages"),
        Reply::new(200, r#"{"conversation_id":8,"muted":false}"#)
            .with_path("/api/v1/conversations/8/notifications"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let seven = conversation(7, "Старый", true, false);
            v.personal_mode = true;
            v.conversations = vec![seven.clone(), conversation(8, "Новый", true, false)];
            v.conversation = Some(seven);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("hello", window, cx));
            v.send(window, cx);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_conversation(conversation(8, "Новый", true, false), window, cx);
            assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(8));
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the send reached the server");
    wait_for(&mut visual, &view, |v| {
        v.messaging.conversation_verify_request.is_none()
            && v.conversations.len() == 1
            && v.conversations[0].id == 8
    });
    assert!(server
        .requests()
        .iter()
        .any(|request| request.method == "POST"
            && request.path == "/api/v1/conversations/7/messages"));
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversation.as_ref().map(|row| row.id),
            Some(8),
            "the new active chat is untouched"
        );
        assert_eq!(v.channel, 8);
        assert!(v.composer_error.is_empty());
        assert!(
            v.drafts
                .borrow()
                .text(draft::ChatKey::conversation(7))
                .is_empty(),
            "the confirmed removal drops the revoked row's own draft"
        );
    });
    assert!(server
        .requests()
        .iter()
        .any(|request| request.method == "GET" && request.path == "/api/v1/conversations/7"));
}

/// A send `404` for a row the user already left drops that row's draft scope;
/// the open chat keeps its own draft and composer.
#[gpui::test]
async fn send_404_after_a_switch_drops_the_revoked_draft(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(404, r#"{"error":"missing"}"#)
            .with_path("/api/v1/conversations/7/messages")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page()).with_path("/api/v1/conversations/8/messages"),
        Reply::new(200, r#"{"conversation_id":8,"muted":false}"#)
            .with_path("/api/v1/conversations/8/notifications"),
        Reply::new(200, &page(vec![conversation(8, "Новый", true, false)]))
            .with_path("/api/v1/me/conversations"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let seven = conversation(7, "Старый", true, false);
            v.personal_mode = true;
            v.conversations = vec![seven.clone(), conversation(8, "Новый", true, false)];
            v.conversation = Some(seven);
            v.channel = 7;
            v.composer
                .update(cx, |input, cx| input.set_value("hello", window, cx));
            v.send(window, cx);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_conversation(conversation(8, "Новый", true, false), window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.drafts.borrow().text(draft::ChatKey::conversation(7)),
            "hello",
            "the switch saves the old row's draft first"
        );
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the send reached the server");
    wait_for(&mut visual, &view, |v| {
        v.conversations.len() == 1 && v.conversations[0].id == 8 && !v.messaging.loading
    });
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(8));
        assert_eq!(v.channel, 8);
        assert!(
            v.drafts
                .borrow()
                .text(draft::ChatKey::conversation(7))
                .is_empty(),
            "the removed non-active row keeps no stale draft"
        );
        assert!(
            v.drafts
                .borrow()
                .text(draft::ChatKey::conversation(8))
                .is_empty(),
            "another chat's draft is untouched"
        );
        assert_eq!(v.composer.read(cx).value(), "");
    });
}

/// A conversation reaction `403` is decided by the conversation GET, not the
/// channel snapshot path.
#[gpui::test]
async fn dm_reaction_403_verifies_and_clears_the_conversation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(403, r#"{"error":"forbidden"}"#)
            .with_path("/api/v1/conversations/7/messages/5/reactions"),
        Reply::new(403, r#"{"error":"forbidden"}"#).with_path("/api/v1/conversations/7"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            let row = conversation(7, "Нева", true, false);
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.messages = vec![message(5, 7)];
            v.toggle_reaction(5, "🔥".into(), false, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.messaging.conversation_verify_request.is_none()
            && v.conversation.is_none()
            && v.conversations.is_empty()
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.channel, 0);
        assert!(v.action_error.as_ref().is_some_and(|(id, _)| *id == 5));
        assert!(v.channel_snapshot_request.is_none());
    });
    let requests = server.requests();
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/api/v1/conversations/7");
}

/// An older page requested before a first-page refresh may not regress the
/// cursor or append rows the fresh page owns.
#[gpui::test]
async fn stale_older_page_after_a_first_page_refresh_is_discarded(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let refreshed = json!({
        "conversations": [conversation(9, "nine", true, false)],
        "has_more": true,
        "next_cursor": "cur2"
    })
    .to_string();
    let stale = json!({
        "conversations": [conversation(3, "three", true, false)],
        "has_more": false,
        "next_cursor": null
    })
    .to_string();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &stale)
            .with_path("/api/v1/me/conversations?limit=30&before=cur1")
            .with_delay(Duration::from_millis(500)),
        Reply::new(200, &refreshed).with_path("/api/v1/me/conversations?limit=30"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.messaging.has_more = true;
            v.messaging.cursor = Some("cur1".into());
            v.load_older_conversations(cx);
        });
    });
    visual.update(|_, app| {
        view.update(app, |v, cx| v.refresh_recents(cx));
    });
    wait_for(&mut visual, &view, |v| !v.messaging.older_loading);
    wait_for(&mut visual, &view, |v| v.recents_request.is_none());
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.messaging.cursor.as_deref(),
            Some("cur2"),
            "the older answer cannot regress the cursor"
        );
        assert!(v.messaging.has_more, "the fresh page still has more");
        assert!(
            !v.conversations.iter().any(|row| row.id == 3),
            "the stale page is not appended"
        );
        assert!(v.messaging.older_error.is_empty());
    });
}

/// An older-page error offers a retry that appends exactly that page.
#[gpui::test]
async fn older_page_error_offers_a_retry(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let retried = json!({
        "conversations": [conversation(3, "three", true, false)],
        "has_more": false,
        "next_cursor": null
    })
    .to_string();
    let server = TestServer::spawn(vec![
        Reply::new(500, r#"{"error":"failed"}"#)
            .with_path("/api/v1/me/conversations?limit=30&before=cur1"),
        Reply::new(200, &retried).with_path("/api/v1/me/conversations?limit=30&before=cur1"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.conversations = vec![conversation(5, "five", true, false)];
            v.messaging.has_more = true;
            v.messaging.cursor = Some("cur1".into());
            v.load_older_conversations(cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.messaging.older_loading && !v.messaging.older_error.is_empty()
    });
    draw(&mut visual);
    let retry = visual
        .debug_bounds("conversation-older-retry")
        .expect("the failed older page offers Повторить");
    visual.simulate_click(retry.center(), Modifiers::default());
    wait_for(&mut visual, &view, |v| {
        !v.messaging.older_loading
            && v.messaging.older_error.is_empty()
            && v.conversations.iter().any(|row| row.id == 3)
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.messaging.has_more);
        assert_eq!(v.messaging.cursor, None);
    });
    assert_eq!(server.requests().len(), 2);
    assert_eq!(
        server.requests()[1].path,
        "/api/v1/me/conversations?limit=30&before=cur1"
    );
}

/// The DELETE answer stays owned by its conversation after a switch.
#[gpui::test]
async fn leave_success_applies_after_switching_chats(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, "")
            .with_path("/api/v1/conversations/7/members/me")
            .with_gate(gate.clone()),
        Reply::new(200, &messages_page()).with_path("/api/v1/conversations/8/messages"),
        Reply::new(200, r#"{"conversation_id":8,"muted":false}"#)
            .with_path("/api/v1/conversations/8/notifications"),
        Reply::new(200, &page(vec![conversation(8, "Новый", true, false)]))
            .with_path("/api/v1/me/conversations"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let group = conversation(7, "Группа", false, false);
            v.personal_mode = true;
            v.conversations = vec![group.clone(), conversation(8, "Новый", true, false)];
            v.conversation = Some(group);
            v.channel = 7;
            v.request_leave_conversation(cx);
            v.confirm_leave_conversation(window, cx);
            assert_eq!(v.messaging.leave_target, 7);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_conversation(conversation(8, "Новый", true, false), window, cx);
            assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(8));
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the DELETE reached the server");
    wait_for(&mut visual, &view, |v| {
        v.messaging.leave_request.is_none()
            && v.conversations.len() == 1
            && v.conversations[0].id == 8
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.conversation.as_ref().map(|row| row.id), Some(8));
        assert_eq!(v.channel, 8);
        assert!(v.messaging.leave_error.is_empty());
        assert!(!v.messaging.leave_confirm);
        assert_eq!(v.messaging.leave_target, 0);
    });
}

/// Entering personal mode hides the members drawer; returning restores the same
/// channel-surface state.
#[gpui::test]
async fn members_drawer_state_survives_personal_mode(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(200, &empty_page())]);
    let (window, view) = make_app(cx, direct_client(&server));
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 0;
            v.show_members = true;
            v.enter_personal_mode(window, cx);
            assert!(v.personal_mode);
            assert!(!v.show_members, "the drawer is hidden on the DM surface");
            v.return_to_channels(window, cx);
            assert!(!v.personal_mode);
            assert!(v.show_members, "the channel drawer is restored");
        });
    });
}
