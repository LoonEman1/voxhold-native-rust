//! CF-07: text typed after «Сохранить» must survive the late PATCH answer.
//!
//! The editor closes only when the message id, the editor-open generation and
//! the current input text still match the request that was sent. Reopening the
//! same message while an older PATCH is in flight installs a new generation, so
//! that answer cannot close the new editor or replace its text.

use super::super::*;
use super::w08a_p2::{direct_client, make_app, wait_for};
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use serde_json::json;
use std::time::Duration;

const PATCH_PATH: &str = "/api/v1/servers/1/channels/7/messages/5";

fn channel_row(id: i64) -> voxhold_api::Channel {
    voxhold_api::Channel {
        id,
        name: "general".into(),
        kind: "text".into(),
        permissions: 3,
        ..Default::default()
    }
}

fn message_row(id: i64, revision: i64, content: &str) -> voxhold_api::Message {
    serde_json::from_value(json!({
        "id": id,
        "channel_id": 7,
        "conversation_id": 0,
        "content": content,
        "author": {"user_id": 1, "username": "me"},
        "created_at": 1,
        "revision": revision,
        "reactions": [],
        "attachments": []
    }))
    .unwrap()
}

fn patch_body(content: &str) -> String {
    serde_json::to_string(&message_row(5, 2, content)).unwrap()
}

/// The channel/message state an edit starts from: an own row in a writable
/// text channel with the member role.
fn writable_history(v: &mut Voxhold) {
    v.server = 1;
    v.channel = 7;
    v.channels = vec![channel_row(7)];
    v.messages = vec![message_row(5, 1, "версия 0")];
    v.shell_initialized = true;
    v.show_members = false;
    v.search = None;
}

fn editor_text(v: &Voxhold, cx: &gpui::App) -> String {
    v.edit_input.read(cx).value().to_string()
}

fn set_editor(v: &mut Voxhold, text: &str, window: &mut Window, cx: &mut Context<Voxhold>) {
    let text = text.to_string();
    v.edit_input
        .update(cx, |input, cx| input.set_value(text, window, cx));
}

/// Release the held PATCH from a separate thread so pumping the test executor
/// (which polls the request inline) can finish. Returns whether the request
/// actually reached the server.
fn release_on_arrival(gate: std::sync::Arc<Gate>) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let arrived = gate.wait_arrived(Duration::from_secs(5));
        gate.release();
        arrived
    })
}

/// Acceptance: «версия 1» is sent, «версия 2» is typed before the answer.
/// The confirmed history keeps «версия 1», the editor stays open with
/// «версия 2» and the next save sends exactly that text.
#[gpui::test]
async fn typed_after_save_stays_in_the_open_editor_and_is_sent_next(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &patch_body("версия 1"))
            .with_path(PATCH_PATH)
            .with_gate(gate.clone()),
        Reply::new(200, &patch_body("версия 2")).with_path(PATCH_PATH),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            v.start_edit(5, window, cx);
            set_editor(v, "версия 1", window, cx);
            v.save_edit(window, cx);
        });
    });
    // The PATCH is queued; the user keeps typing before its answer is applied.
    visual.update(|window, app| {
        view.update(app, |v, cx| set_editor(v, "версия 2", window, cx));
    });
    let waiter = release_on_arrival(gate);
    wait_for(&mut visual, &view, |v| {
        v.messages
            .iter()
            .any(|message| message.id == 5 && message.content == "версия 1")
            && !v.action_pending(5, "edit")
    });
    assert!(waiter.join().unwrap(), "the first PATCH reached the server");
    view.read_with(&*visual, |v, cx| {
        assert_eq!(
            v.messages
                .iter()
                .find(|message| message.id == 5)
                .map(|message| message.content.as_str()),
            Some("версия 1"),
            "the server revision is merged into history"
        );
        assert_eq!(v.editing, Some(5), "the editor stays open");
        assert_eq!(
            editor_text(v, cx),
            "версия 2",
            "the text typed during the request is kept"
        );
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| v.save_edit(window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        v.editing.is_none()
            && v.messages
                .iter()
                .any(|message| message.id == 5 && message.content == "версия 2")
    });
    let patches: Vec<serde_json::Value> = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "PATCH")
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(patches.len(), 2, "one PATCH per save");
    assert_eq!(patches[0]["content"], "версия 1");
    assert_eq!(
        patches[1]["content"], "версия 2",
        "the next save sends the kept text"
    );
}

/// Control: a normal save with no further typing closes the editor and shows
/// the confirmed revision.
#[gpui::test]
async fn normal_save_closes_the_editor(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &patch_body("версия 1")).with_path(PATCH_PATH)
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            v.start_edit(5, window, cx);
            set_editor(v, "версия 1", window, cx);
            v.save_edit(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.editing.is_none()
            && v.messages
                .iter()
                .any(|message| message.id == 5 && message.content == "версия 1")
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.editing, None);
    });
    let patches = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "PATCH")
        .count();
    assert_eq!(patches, 1);
}

/// Close and reopen the edit of the same row while the first PATCH is still
/// running. The late answer updates history but must not close the reopened
/// editor or replace the text typed into it.
#[gpui::test]
async fn reopening_the_same_edit_ignores_the_older_patch_answer(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![Reply::new(200, &patch_body("версия 1"))
        .with_path(PATCH_PATH)
        .with_gate(gate.clone())]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            v.start_edit(5, window, cx);
            set_editor(v, "версия 1", window, cx);
            v.save_edit(window, cx);
        });
    });
    // The user closes the form and opens the same row again before the queued
    // PATCH answer is applied.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.cancel_edit(cx);
            v.start_edit(5, window, cx);
            set_editor(v, "версия новая", window, cx);
        });
    });
    let waiter = release_on_arrival(gate);
    wait_for(&mut visual, &view, |v| {
        v.messages
            .iter()
            .any(|message| message.id == 5 && message.content == "версия 1")
            && !v.action_pending(5, "edit")
    });
    assert!(waiter.join().unwrap(), "the first PATCH reached the server");
    view.read_with(&*visual, |v, cx| {
        assert_eq!(
            v.messages
                .iter()
                .find(|message| message.id == 5)
                .map(|message| message.content.as_str()),
            Some("версия 1"),
            "the confirmed revision still reaches history"
        );
        assert_eq!(
            v.editing,
            Some(5),
            "the late answer cannot close the reopened editor"
        );
        assert_eq!(
            editor_text(v, cx),
            "версия новая",
            "the late answer cannot overwrite the new text"
        );
    });
}

/// A failed PATCH belongs to the editor generation that started it: it is shown
/// only while that editor is still active, and never on a reopened form.
#[gpui::test]
async fn stale_edit_error_does_not_paint_on_a_reopened_editor(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let failure = r#"{"error":"boom"}"#;
    let server = TestServer::spawn(vec![
        Reply::new(500, failure)
            .with_path(PATCH_PATH)
            .with_gate(gate.clone()),
        Reply::new(500, failure).with_path(PATCH_PATH),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            v.start_edit(5, window, cx);
            set_editor(v, "версия 1", window, cx);
            v.save_edit(window, cx);
        });
    });
    // Reopen the same row before the failing answer is applied.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.cancel_edit(cx);
            v.start_edit(5, window, cx);
            set_editor(v, "версия новая", window, cx);
        });
    });
    let waiter = release_on_arrival(gate);
    wait_for(&mut visual, &view, |v| !v.action_pending(5, "edit"));
    assert!(waiter.join().unwrap(), "the first PATCH reached the server");
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.editing, Some(5));
        assert_eq!(editor_text(v, cx), "версия новая");
        assert!(
            v.edit_error.is_none(),
            "the stale failure does not paint on the reopened editor"
        );
        assert!(
            v.action_error.is_none(),
            "the stale failure does not leak into the row error slot"
        );
    });

    // The same editor now fails its own request: that error stays visible.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.save_edit(window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.action_pending(5, "edit") && v.edit_error.is_some()
    });
    view.read_with(&*visual, |v, cx| {
        let (id, open, error) = v.edit_error.clone().expect("current edit error");
        assert_eq!(id, 5);
        assert_eq!(open, v.edit_open_seq, "bound to the active editor");
        assert!(!error.is_empty());
        assert_eq!(v.editing, Some(5), "a failure keeps the form open");
        assert_eq!(editor_text(v, cx), "версия новая");
        assert!(
            v.action_error.is_none(),
            "the PATCH failure is bound to edit_error, not the row slot"
        );
    });
}

/// The server may normalize the body. The editor closes against the text that
/// was sent, while history keeps the normalized answer.
#[gpui::test]
async fn server_normalization_closes_the_editor_and_stays_in_history(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &patch_body("версия 1")).with_path(PATCH_PATH)
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            v.start_edit(5, window, cx);
            set_editor(v, "  версия 1  ", window, cx);
            v.save_edit(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.editing.is_none()
            && v.messages
                .iter()
                .any(|message| message.id == 5 && message.content == "версия 1")
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.messages
                .iter()
                .find(|message| message.id == 5)
                .map(|message| message.content.as_str()),
            Some("версия 1"),
            "history keeps the server-normalized body"
        );
    });
    let body: serde_json::Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    assert_eq!(body["content"], "  версия 1  ");
}

/// `close_edit` owns the generation: every cancel that closes an open form
/// invalidates the previous one.
#[gpui::test]
async fn closing_the_editor_bumps_the_open_generation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            let before = v.edit_open_seq;
            v.start_edit(5, window, cx);
            let opened = v.edit_open_seq;
            assert_ne!(opened, before, "opening starts a new generation");
            v.cancel_edit(cx);
            assert!(v.editing.is_none());
            assert_ne!(
                v.edit_open_seq, opened,
                "closing invalidates the closed editor"
            );
        });
    });
}

/// A second click on «Сохранить» while the first PATCH is pending must not
/// start another request; the next save is allowed only after it settles.
#[gpui::test]
async fn second_save_while_patch_is_pending_sends_nothing(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![Reply::new(200, &patch_body("версия 1"))
        .with_path(PATCH_PATH)
        .with_gate(gate.clone())]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            v.start_edit(5, window, cx);
            set_editor(v, "версия 1", window, cx);
            v.save_edit(window, cx);
            assert!(v.action_pending(5, "edit"));
            v.save_edit(window, cx);
        });
    });
    let waiter = release_on_arrival(gate);
    wait_for(&mut visual, &view, |v| {
        !v.action_pending(5, "edit") && v.editing.is_none()
    });
    assert!(waiter.join().unwrap(), "the first PATCH reached the server");
    let patches = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "PATCH")
        .count();
    assert_eq!(patches, 1, "the pending action rejects the second click");
}

/// A late success from an older generation must not clear the validation error
/// of the editor that is open now.
#[gpui::test]
async fn late_success_keeps_the_active_editors_error(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![Reply::new(200, &patch_body("версия 1"))
        .with_path(PATCH_PATH)
        .with_gate(gate.clone())]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    gate.arm();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            v.start_edit(5, window, cx);
            set_editor(v, "версия 1", window, cx);
            v.save_edit(window, cx);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.cancel_edit(cx);
            v.start_edit(5, window, cx);
            set_editor(v, "", window, cx);
            v.save_edit(window, cx);
            assert!(v.editing.is_some(), "validation keeps the form open");
            assert!(v.action_error.is_some(), "the validation error is visible");
        });
    });
    let waiter = release_on_arrival(gate);
    wait_for(&mut visual, &view, |v| !v.action_pending(5, "edit"));
    assert!(waiter.join().unwrap(), "the old PATCH reached the server");
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.editing, Some(5));
        assert!(
            v.action_error.is_some(),
            "the late success does not clear the current editor's error"
        );
        assert!(v.edit_error.is_none());
        assert_eq!(editor_text(v, cx), "");
    });
}

/// `begin_action` still clears the previous action error when a new save
/// actually starts its request.
#[gpui::test]
async fn starting_a_save_clears_the_previous_action_error(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &patch_body("версия 1")).with_path(PATCH_PATH)
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_history(v);
            v.start_edit(5, window, cx);
            set_editor(v, "", window, cx);
            v.save_edit(window, cx);
            assert!(v.action_error.is_some());
            set_editor(v, "версия 1", window, cx);
            v.save_edit(window, cx);
            assert!(
                v.action_error.is_none(),
                "a started save drops the stale validation error"
            );
        });
    });
    wait_for(&mut visual, &view, |v| v.editing.is_none());
}
