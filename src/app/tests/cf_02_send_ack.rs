//! CF-02: an own message echoed by the chat socket is a confirmation of the
//! in-flight send, exactly like the HTTP answer. The first confirmation settles
//! the attempt; a late `504` cannot bring the error back. A confirmation for a
//! chat the user already left still consumes its own draft, and a confirmation
//! of an older attempt never touches a newer composition.

use super::super::*;
use super::message_json;
use super::w08a_p2::{
    channel_row, conversation as conversation_row, direct_client, make_app, message as dm_message,
    page, wait_for,
};
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Deliver one batch of realtime events through the real pump path, which has
/// the `Window` needed to clear the composer.
fn queue(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    events: Vec<voxhold_api::RealtimeEvent>,
) {
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    for event in events {
        tx.send(event).expect("queue event");
    }
    drop(tx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.events = Some(Arc::new(Mutex::new(rx)));
            v.events_overflow = Default::default();
            v.process_realtime_batch(window, cx);
        });
    });
}

/// Pump the UI executor for a bounded time so a background HTTP answer (for
/// example the late `504`) reaches its callback even when it changes no state.
fn settle(visual: &mut VisualTestContext) {
    for _ in 0..100 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn created(id: i64, channel: i64, key: &str, author: i64, content: &str) -> serde_json::Value {
    let mut row = message_json(id, 1, content);
    row["channel_id"] = json!(channel);
    row["client_message_id"] = json!(key);
    row["author"]["user_id"] = json!(author);
    row
}

fn event(kind: &str, data: serde_json::Value) -> voxhold_api::RealtimeEvent {
    voxhold_api::RealtimeEvent {
        kind: kind.into(),
        data,
        ..Default::default()
    }
}

fn channel_key() -> draft::ChatKey {
    draft::ChatKey::channel(1, 7)
}

fn empty_history() -> String {
    json!({"messages": [], "has_more": false}).to_string()
}

fn open_channel(v: &mut Voxhold, ids: &[i64]) {
    v.server = 1;
    v.channel = 7;
    v.personal_mode = false;
    v.conversation = None;
    v.access_pending = false;
    v.self_user_id = 1;
    v.channels = ids
        .iter()
        .map(|id| channel_row(*id, "test"))
        .collect::<Vec<_>>();
}

fn compose(v: &mut Voxhold, text: &'static str, window: &mut Window, cx: &mut Context<Voxhold>) {
    v.composer
        .update(cx, |input, cx| input.set_value(text, window, cx));
}

fn send_key(v: &Voxhold, key: draft::ChatKey) -> String {
    v.drafts
        .borrow()
        .get(key)
        .map(|draft| draft.send_key.clone())
        .unwrap_or_default()
}

/// (a) The echo arrives while the POST is still held. The send settles from the
/// echo, and the late `504` must not restore either the error or the draft.
#[gpui::test]
async fn websocket_echo_before_a_late_504_confirms_the_send(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn(vec![
        Reply::new(504, r#"{"error":"gateway timeout"}"#).with_gate(gate.clone())
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7]);
            compose(v, "первое", window, cx);
            v.send(window, cx);
            assert!(v.is_sending());
        });
    });
    assert!(
        gate.wait_arrived(Duration::from_secs(5)),
        "the POST started"
    );
    let key = view.read_with(&*visual, |v, _| send_key(v, channel_key()));
    assert_eq!(key.len(), 32);

    queue(
        &mut visual,
        &view,
        vec![event("message.created", created(70, 7, &key, 1, "первое"))],
    );
    let settled = view.read_with(&*visual, |v, cx| {
        (
            v.composer_error.clone(),
            v.composer.read(cx).value().to_string(),
            v.drafts.borrow().text(channel_key()),
            v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
            v.is_sending(),
        )
    });
    gate.release();
    settle(&mut visual);
    let after_late_504 = view.read_with(&*visual, |v, cx| {
        (
            v.composer_error.clone(),
            v.composer.read(cx).value().to_string(),
            v.drafts.borrow().text(channel_key()),
            v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
        )
    });
    assert_eq!(settled.0, "", "the echo confirms before any 504");
    assert_eq!(settled.1, "", "the confirmed composer is cleared");
    assert_eq!(settled.2, "", "the confirmed draft is consumed");
    assert_eq!(settled.3, vec![70], "the echo is one history row");
    assert!(!settled.4, "the attempt is no longer in flight");
    assert_eq!(after_late_504.0, "", "the late 504 is ignored");
    assert_eq!(after_late_504.1, "");
    assert_eq!(after_late_504.2, "");
    assert_eq!(after_late_504.3, vec![70], "the history still has one row");
}

/// (b) The `504` arrives first and shows the error. The echo that follows must
/// remove it and consume the draft, exactly like a successful POST would.
#[gpui::test]
async fn websocket_echo_after_a_504_removes_the_error(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn(vec![
        Reply::new(504, r#"{"error":"gateway timeout"}"#).with_gate(gate.clone())
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7]);
            compose(v, "первое", window, cx);
            v.send(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(5)));
    gate.release();
    wait_for(&mut visual, &view, |v| {
        !v.is_sending() && !v.composer_error.is_empty()
    });
    let key = view.read_with(&*visual, |v, _| send_key(v, channel_key()));
    assert_eq!(key.len(), 32, "the retry keeps the idempotency key");

    queue(
        &mut visual,
        &view,
        vec![event("message.created", created(71, 7, &key, 1, "первое"))],
    );
    view.read_with(&*visual, |v, cx| {
        assert!(
            v.composer_error.is_empty(),
            "the echo removed the 504 error"
        );
        assert_eq!(v.composer.read(cx).value(), "");
        assert_eq!(v.drafts.borrow().text(channel_key()), "");
        assert_eq!(
            v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![71],
            "the confirmed row appears once"
        );
        assert!(!v.is_sending());
    });
}

/// (c) A foreign row may reuse the same `client_message_id` (for example a
/// forwarded row); only the signed-in author confirms the attempt.
#[gpui::test]
async fn foreign_author_with_the_same_key_does_not_confirm(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn(vec![
        Reply::new(504, r#"{"error":"gateway timeout"}"#).with_gate(gate.clone())
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7]);
            compose(v, "первое", window, cx);
            v.send(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(5)));
    gate.release();
    wait_for(&mut visual, &view, |v| {
        !v.is_sending() && !v.composer_error.is_empty()
    });
    let error = view.read_with(&*visual, |v, _| v.composer_error.clone());
    let key = view.read_with(&*visual, |v, _| send_key(v, channel_key()));

    queue(
        &mut visual,
        &view,
        vec![event("message.created", created(72, 7, &key, 2, "чужое"))],
    );
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.composer_error, error, "a foreign author is not an ack");
        assert_eq!(v.composer.read(cx).value(), "первое");
        assert_eq!(v.drafts.borrow().text(channel_key()), "первое");
        assert!(
            v.messages.iter().any(|row| row.id == 72),
            "the foreign row still follows the normal history path"
        );
    });
}

/// (d) The user switched to another channel before the echo. The echo consumes
/// the draft of the channel that sent the message and leaves the open composer
/// alone.
#[gpui::test]
async fn an_echo_after_a_channel_switch_clears_the_original_draft(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let mut answered = message_json(73, 1, "первое");
    answered["channel_id"] = json!(7);
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(201, &answered.to_string()).with_gate(gate.clone()),
        Reply::new(200, &empty_history()),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7, 8]);
            compose(v, "первое", window, cx);
            v.send(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(5)));
    let key = view.read_with(&*visual, |v, _| send_key(v, channel_key()));

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_channel(8, window, cx);
            assert_eq!(v.chat_key(), draft::ChatKey::channel(1, 8));
            compose(v, "черновик 8", window, cx);
        });
    });
    queue(
        &mut visual,
        &view,
        vec![event("message.created", created(73, 7, &key, 1, "первое"))],
    );
    let state = view.read_with(&*visual, |v, cx| {
        (
            v.composer.read(cx).value().to_string(),
            v.drafts.borrow().text(channel_key()),
        )
    });
    gate.release();
    settle(&mut visual);
    let after_late_answer = view.read_with(&*visual, |v, cx| {
        (
            v.composer.read(cx).value().to_string(),
            v.drafts.borrow().text(channel_key()),
            v.composer_error.clone(),
        )
    });
    assert_eq!(
        state.1, "",
        "the draft of the channel that sent the message is consumed"
    );
    assert_eq!(state.0, "черновик 8", "the open composer is not cleared");
    assert_eq!(
        after_late_answer.1, "",
        "the late HTTP answer does not restore the consumed draft"
    );
    assert_eq!(after_late_answer.0, "черновик 8");
    assert!(after_late_answer.2.is_empty());
}

/// (e) The `504` looks uncertain. The user starts a new composition while the
/// old POST may still have been applied. The echo of the old attempt updates
/// the history but must not touch the new text, attachments, key or send state.
#[gpui::test]
async fn a_late_echo_does_not_touch_a_newer_composition(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let path = std::env::temp_dir().join(format!(
        "voxhold-cf02-{}-{}.bin",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, vec![4u8; 16]).unwrap();
    let attachment = draft::DraftFile::from_path(path.clone()).unwrap();
    let upload_reply = json!({
        "id": attachment.id,
        "filename": attachment.name,
        "mime_type": "application/octet-stream",
        "size_bytes": attachment.size,
        "width": 0,
        "height": 0,
        "deleted": false
    })
    .to_string();
    let first = Gate::new();
    let second = Gate::new();
    first.arm();
    second.arm();
    let mut second_row = message_json(81, 1, "второй");
    second_row["channel_id"] = json!(7);
    second_row["client_message_id"] = json!("placeholder");
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &upload_reply).with_path("/api/v1/servers/1/channels/7/attachments/"),
        Reply::new(504, r#"{"error":"gateway timeout"}"#)
            .with_path("/api/v1/servers/1/channels/7/messages")
            .with_gate(first.clone()),
        Reply::new(201, &second_row.to_string())
            .with_path("/api/v1/servers/1/channels/7/messages")
            .with_gate(second.clone()),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7]);
            compose(v, "первый", window, cx);
            v.send(window, cx);
        });
    });
    assert!(first.wait_arrived(Duration::from_secs(5)));
    let old_key = view.read_with(&*visual, |v, _| send_key(v, channel_key()));
    first.release();
    wait_for(&mut visual, &view, |v| {
        !v.is_sending() && !v.composer_error.is_empty()
    });

    let attempt_after_504 = view.read_with(&*visual, |v, _| v.send_attempt);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.drafts
                .borrow_mut()
                .add_files(channel_key(), vec![attachment.clone()]);
            compose(v, "второй", window, cx);
            v.send(window, cx);
            assert!(v.is_sending(), "the second composition is in flight");
        });
    });
    assert!(
        second.wait_arrived(Duration::from_secs(5)),
        "the second POST reached the server after its upload"
    );
    let (new_key, new_attempt) = view.read_with(&*visual, |v, _| {
        (send_key(v, channel_key()), v.send_attempt)
    });
    assert!(!new_key.is_empty());
    assert_ne!(old_key, new_key, "a new composition has its own key");
    assert!(new_attempt > attempt_after_504);

    queue(
        &mut visual,
        &view,
        vec![event(
            "message.created",
            created(80, 7, &old_key, 1, "первый"),
        )],
    );
    let state = view.read_with(&*visual, |v, cx| {
        (
            v.composer.read(cx).value().to_string(),
            v.drafts.borrow().text(channel_key()),
            v.drafts.borrow().files(channel_key()),
            send_key(v, channel_key()),
            v.send_attempt,
            v.is_sending(),
            v.composer_error.clone(),
            v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
        )
    });
    assert_eq!(state.0, "второй", "the new composer text stays");
    assert_eq!(state.1, "второй", "the new draft text stays");
    assert_eq!(state.2.len(), 1, "the new attachment stays");
    assert_eq!(state.2[0].id, attachment.id);
    assert_eq!(state.3, new_key, "the new idempotency key stays");
    assert_eq!(state.4, new_attempt);
    assert!(state.5, "the second send is still in flight");
    assert!(state.6.is_empty(), "the old echo clears no new error");
    assert!(
        state.7.contains(&80),
        "the old attempt still updates the history"
    );

    second.release();
    wait_for(&mut visual, &view, |v| !v.is_sending());
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.composer.read(cx).value(), "");
        assert_eq!(v.drafts.borrow().text(channel_key()), "");
        assert!(v.drafts.borrow().files(channel_key()).is_empty());
        assert_eq!(
            v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![80, 81]
        );
    });
    std::fs::remove_file(path).ok();
}

/// (f) A personal conversation echo follows the same completion path even
/// though the normal `conversation.message` merge already runs in the batch.
#[gpui::test]
async fn a_conversation_echo_confirms_the_dm_send(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let row = conversation_row(7, "Нева", true, false);
    let recents = page(vec![row.clone()]);
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(201, &serde_json::to_string(&dm_message(90, 7)).unwrap())
            .with_gate(gate.clone()),
        Reply::new(200, &recents),
        Reply::new(200, &recents),
        Reply::new(200, &recents),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let key = draft::ChatKey::conversation(7);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.conversation = Some(row);
            v.channel = 7;
            v.access_pending = false;
            compose(v, "привет", window, cx);
            v.send(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(5)));
    let send_key = view.read_with(&*visual, |v, _| send_key(v, key));
    assert_eq!(send_key.len(), 32);
    let mut echo = message_json(90, 1, "привет");
    echo["channel_id"] = json!(0);
    echo["conversation_id"] = json!(7);
    echo["client_message_id"] = json!(send_key);
    queue(
        &mut visual,
        &view,
        vec![event("conversation.message", echo)],
    );
    let state = view.read_with(&*visual, |v, cx| {
        (
            v.composer.read(cx).value().to_string(),
            v.drafts.borrow().text(key),
            v.is_sending(),
            v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
            v.composer_error.clone(),
        )
    });
    gate.release();
    settle(&mut visual);
    assert_eq!(state.0, "", "the DM composer is cleared by its echo");
    assert_eq!(state.1, "");
    assert!(!state.2);
    assert_eq!(state.3, vec![90], "the DM history has one row");
    assert!(state.4.is_empty());
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.conversations.first().map(|row| row.id),
            Some(7),
            "the index stays"
        )
    });
}

/// A refusal before the POST starts is a definite failure. Its plan is not an
/// uncertain snapshot, so a later echo with the same key must not consume the
/// draft or hide the error.
#[gpui::test]
async fn an_upload_refusal_before_the_post_keeps_no_snapshot(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let path = std::env::temp_dir().join(format!(
        "voxhold-cf02-refused-{}-{}.bin",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, vec![2u8; 8]).unwrap();
    let attachment = draft::DraftFile::from_path(path.clone()).unwrap();
    // The verified upload reopens the file; a missing file refuses the send
    // before any HTTP request starts.
    std::fs::remove_file(&path).unwrap();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7]);
            v.drafts
                .borrow_mut()
                .add_files(channel_key(), vec![attachment]);
            compose(v, "с файлом", window, cx);
            v.send(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.is_sending() && !v.composer_error.is_empty()
    });
    let (error, key) = view.read_with(&*visual, |v, _| {
        assert!(
            v.unconfirmed_sends.is_empty(),
            "an upload refusal is not an uncertain POST"
        );
        assert!(server.requests().is_empty(), "no POST was attempted");
        (v.composer_error.clone(), send_key(v, channel_key()))
    });
    queue(
        &mut visual,
        &view,
        vec![event(
            "message.created",
            created(76, 7, &key, 1, "с файлом"),
        )],
    );
    view.read_with(&*visual, |v, cx| {
        assert_eq!(
            v.composer_error, error,
            "a definite upload failure is not confirmed from the socket"
        );
        assert_eq!(v.composer.read(cx).value(), "с файлом");
        assert_eq!(v.drafts.borrow().text(channel_key()), "с файлом");
        assert_eq!(
            v.drafts.borrow().files(channel_key()).len(),
            1,
            "the attachment stays with the draft for a retry"
        );
        assert!(
            v.messages.iter().any(|row| row.id == 76),
            "the row still follows the normal history path"
        );
    });
}

/// Two copies of the same own echo in one batch are one confirmation: the
/// draft is consumed once and the history keeps one row.
#[gpui::test]
async fn a_duplicate_echo_in_one_batch_confirms_once(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let mut answered = message_json(77, 1, "двойное");
    answered["channel_id"] = json!(7);
    let server = TestServer::spawn(vec![
        Reply::new(201, &answered.to_string()).with_gate(gate.clone())
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7]);
            compose(v, "двойное", window, cx);
            v.send(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(5)));
    let key = view.read_with(&*visual, |v, _| send_key(v, channel_key()));
    queue(
        &mut visual,
        &view,
        vec![
            event("message.created", created(77, 7, &key, 1, "двойное")),
            event("message.created", created(77, 7, &key, 1, "двойное")),
        ],
    );
    let state = view.read_with(&*visual, |v, cx| {
        (
            v.composer.read(cx).value().to_string(),
            v.drafts.borrow().text(channel_key()),
            v.is_sending(),
            v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
            v.composer_error.clone(),
        )
    });
    gate.release();
    settle(&mut visual);
    assert_eq!(state.0, "");
    assert_eq!(state.1, "", "the first echo consumes the draft once");
    assert!(!state.2);
    assert_eq!(state.3, vec![77], "the duplicated echo is one history row");
    assert!(state.4.is_empty());
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![77],
            "the late HTTP answer does not add a second row"
        )
    });
}

/// A thread comment may reuse the idempotency key only in a broken payload;
/// it must not settle the main-timeline send.
#[gpui::test]
async fn a_thread_echo_with_the_same_key_does_not_confirm(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn(vec![
        Reply::new(504, r#"{"error":"gateway timeout"}"#).with_gate(gate.clone())
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7]);
            compose(v, "первое", window, cx);
            v.send(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(5)));
    gate.release();
    wait_for(&mut visual, &view, |v| {
        !v.is_sending() && !v.composer_error.is_empty()
    });
    let (error, key) = view.read_with(&*visual, |v, _| {
        (v.composer_error.clone(), send_key(v, channel_key()))
    });
    let mut echo = created(78, 7, &key, 1, "первое");
    echo["thread_root_id"] = json!(5);
    queue(&mut visual, &view, vec![event("message.created", echo)]);
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.composer_error, error, "a thread row is not an ack");
        assert_eq!(v.composer.read(cx).value(), "первое");
        assert_eq!(v.drafts.borrow().text(channel_key()), "первое");
        assert!(
            v.messages.is_empty(),
            "a thread row stays out of the main timeline"
        );
        assert!(
            v.unconfirmed_sends.contains_key(&channel_key()),
            "the uncertain snapshot is still available"
        );
    });
}

/// The same key in another channel or on another server cannot confirm a send
/// of the selected channel: the event carries no server id.
#[gpui::test]
async fn an_echo_of_another_chat_with_the_same_key_does_not_confirm(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn(vec![
        Reply::new(504, r#"{"error":"gateway timeout"}"#).with_gate(gate.clone())
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            open_channel(v, &[7, 8]);
            compose(v, "первое", window, cx);
            v.send(window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(5)));
    gate.release();
    wait_for(&mut visual, &view, |v| {
        !v.is_sending() && !v.composer_error.is_empty()
    });
    let (error, key) = view.read_with(&*visual, |v, _| {
        (v.composer_error.clone(), send_key(v, channel_key()))
    });

    queue(
        &mut visual,
        &view,
        vec![event(
            "message.created",
            created(79, 8, &key, 1, "другой канал"),
        )],
    );
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.composer_error, error, "another channel is not the ack");
        assert_eq!(v.composer.read(cx).value(), "первое");
        assert_eq!(v.drafts.borrow().text(channel_key()), "первое");
        assert!(v.unconfirmed_sends.contains_key(&channel_key()));
    });

    // The event names only a channel id; the plan belongs to server 1. While
    // server 2 is selected, the same numeric channel is not this send.
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.server = 2;
            v.channels = vec![channel_row(7, "test")];
        });
    });
    queue(
        &mut visual,
        &view,
        vec![event(
            "message.created",
            created(80, 7, &key, 1, "другой сервер"),
        )],
    );
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.composer_error, error, "another server is not the ack");
        assert_eq!(v.composer.read(cx).value(), "первое");
        assert_eq!(v.drafts.borrow().text(channel_key()), "первое");
        assert!(v.unconfirmed_sends.contains_key(&channel_key()));
    });
}

/// Removing a server drops only that server's uncertain snapshots; personal
/// conversations and other servers keep theirs.
#[gpui::test]
async fn forgetting_a_server_drops_only_its_send_snapshots(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, _| {
            let plan = |key: draft::ChatKey, name: &str| draft::SendPlan {
                key,
                content: name.into(),
                composer_text: name.into(),
                reply: 0,
                files: Vec::new(),
                send_key: format!("{}-{}-{name}", key.server, key.channel),
                revision: 1,
            };
            let on_server_1 = draft::ChatKey::channel(1, 7);
            let on_server_2 = draft::ChatKey::channel(2, 7);
            let conversation = draft::ChatKey::conversation(9);
            v.unconfirmed_sends
                .insert(on_server_1, plan(on_server_1, "first"));
            v.unconfirmed_sends
                .insert(on_server_2, plan(on_server_2, "second"));
            v.unconfirmed_sends
                .insert(conversation, plan(conversation, "direct"));
            v.forget_server_sends(1);
            assert!(!v.unconfirmed_sends.contains_key(&on_server_1));
            assert!(v.unconfirmed_sends.contains_key(&on_server_2));
            assert!(v.unconfirmed_sends.contains_key(&conversation));
        });
    });
}
