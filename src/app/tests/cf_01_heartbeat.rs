//! CF-01: a chat socket that misses its heartbeat becomes visible as
//! "Переподключаемся…" in the existing chat status area. The loaded history
//! stays on screen while the outer reconnect loop waits for the next socket,
//! the status is cleared only by a confirmed session, and ending the session
//! drops it with the socket.

use super::super::*;
use super::w08a_p2::{channel_row, direct_client, draw, make_app};
use crate::test_support::TestServer;
use gpui::{TestAppContext, VisualTestContext};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn event(kind: &str) -> voxhold_api::RealtimeEvent {
    voxhold_api::RealtimeEvent {
        kind: kind.into(),
        data: json!({}),
        ..Default::default()
    }
}

fn queue(
    visual: &mut gpui::VisualTestContext,
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

fn message(id: i64, channel_id: i64) -> voxhold_api::Message {
    serde_json::from_value(json!({
        "id": id,
        "channel_id": channel_id,
        "conversation_id": 0,
        "content": "row loaded before the socket went quiet",
        "author": {"user_id": 2, "username": "Нева"},
        "created_at": 1,
        "revision": 1,
        "reactions": [],
        "attachments": []
    }))
    .expect("message")
}

#[gpui::test]
async fn a_dead_socket_shows_the_status_and_keeps_the_loaded_history(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.channels = vec![channel_row(5, "general")];
            v.server = 1;
            v.channel = 5;
            v.conversation = None;
            v.personal_mode = false;
            v.messages = vec![message(70, 5)];
            // Keep the render from starting a read receipt against the stub.
            v.read_pending = true;
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("chat-reconnecting").is_none(),
        "no reconnect status while the socket is healthy"
    );

    queue(&mut visual, &view, vec![event("native.reconnecting")]);
    let (status, channel, rows) = view.read_with(&*visual, |v, _| {
        (v.realtime_reconnecting, v.channel, v.messages.len())
    });
    assert!(status, "the retry state is visible to the UI");
    assert_eq!(channel, 5, "the selection is not cleared");
    assert_eq!(rows, 1, "the loaded history stays available");
    draw(&mut visual);
    assert!(
        visual.debug_bounds("chat-reconnecting").is_some(),
        "the existing chat status area shows «Переподключаемся…»"
    );

    // A confirmed session clears the status; the history and selection are
    // untouched by the status itself. (`debug_bounds` accumulates selectors
    // for the whole test, so removal is asserted through the flag here and by
    // the conditional render in the next test.)
    queue(&mut visual, &view, vec![event("ready")]);
    view.read_with(&*visual, |v, _| {
        assert!(!v.realtime_reconnecting);
        assert_eq!(v.channel, 5);
        assert_eq!(v.messages.len(), 1);
    });
    draw(&mut visual);
}

#[gpui::test]
async fn a_reconnect_event_clears_the_status_and_a_new_session_starts_clean(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();

    // A recovery observed in the same batch never leaves the status on, and
    // nothing paints the banner when the flag stays false.
    queue(
        &mut visual,
        &view,
        vec![event("native.reconnecting"), event("ready")],
    );
    assert!(!view.read_with(&*visual, |v, _| v.realtime_reconnecting));
    draw(&mut visual);
    assert!(
        visual.debug_bounds("chat-reconnecting").is_none(),
        "a healthy socket paints no reconnect status"
    );

    // A retry is visible until the fresh socket confirms the session.
    queue(&mut visual, &view, vec![event("native.reconnecting")]);
    assert!(view.read_with(&*visual, |v, _| v.realtime_reconnecting));
    draw(&mut visual);
    assert!(
        visual.debug_bounds("chat-reconnecting").is_some(),
        "the retry status is painted in the chat area"
    );

    // The outer loop opens a fresh socket; its first `ready` is forwarded as
    // `native.reconnected`, which confirms the session again.
    queue(&mut visual, &view, vec![event("native.reconnected")]);
    assert!(!view.read_with(&*visual, |v, _| v.realtime_reconnecting));

    // Ending the session aborts the owning task and drops the status with it;
    // a stale task cannot repaint it in the next session.
    queue(&mut visual, &view, vec![event("native.reconnecting")]);
    assert!(view.read_with(&*visual, |v, _| v.realtime_reconnecting));
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.stop_realtime();
            assert!(!v.realtime_reconnecting);
        });
    });
}
