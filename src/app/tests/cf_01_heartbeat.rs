//! CF-01: a chat socket that misses its heartbeat becomes visible as
//! "Переподключаемся…" in the existing chat status area. The connection state
//! is a shared atomic, not an event, so a full queue cannot lose the banner or
//! leave it stuck on. The loaded history and composer stay mounted while the
//! outer reconnect loop waits for the next socket.

use super::super::*;
use super::w08a_p2::{channel_row, direct_client, draw, make_app};
use crate::test_support::TestServer;
use gpui::{TestAppContext, VisualTestContext};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn event(kind: &str) -> voxhold_api::RealtimeEvent {
    voxhold_api::RealtimeEvent {
        kind: kind.into(),
        data: json!({}),
        ..Default::default()
    }
}

/// One UI tick over a fresh queue. The batch is intentionally independent of
/// the connection flag; the tick reads the flag like `schedule_events` does.
fn tick(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    events: Vec<voxhold_api::RealtimeEvent>,
    overflow: bool,
) {
    let (tx, rx) = std::sync::mpsc::sync_channel(16);
    for event in events {
        tx.send(event).expect("queue event");
    }
    drop(tx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.events = Some(Arc::new(Mutex::new(rx)));
            v.events_overflow.store(overflow, Ordering::Relaxed);
            v.process_realtime_batch(window, cx);
        });
    });
}

/// Install the live socket flag exactly like `start_realtime` does and return
/// the shared handle so the test can change it like the background task would.
fn link(visual: &mut VisualTestContext, view: &Entity<Voxhold>, down: bool) -> Arc<AtomicBool> {
    let shared = Arc::new(AtomicBool::new(down));
    let installed = shared.clone();
    visual.update(|_, app| {
        view.update(app, |v, _| v.realtime_link = Some(installed));
    });
    shared
}

fn setup_chat(visual: &mut VisualTestContext, view: &Entity<Voxhold>) {
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.channels = vec![channel_row(5, "general")];
            v.server = 1;
            v.channel = 5;
            v.conversation = None;
            v.personal_mode = false;
            v.access_pending = false;
            v.messages = vec![message(70, 5)];
            // Keep the render from starting a read receipt against the stub.
            v.read_pending = true;
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

fn assert_loaded_chat(view: &Entity<Voxhold>, visual: &VisualTestContext, rows: usize) {
    view.read_with(visual, |v, _| {
        assert_eq!(v.channel, 5, "the selection is not cleared");
        assert_eq!(v.messages.len(), rows, "the loaded history stays available");
        assert!(
            !v.access_pending,
            "the retry or its overflow does not hide the history"
        );
    });
}

#[test]
fn reconnect_status_line_is_absent_when_the_socket_is_alive() {
    assert_eq!(super::super::realtime::reconnect_status_line(false), None);
    assert_eq!(
        super::super::realtime::reconnect_status_line(true),
        Some("Переподключаемся…")
    );
}

#[gpui::test]
async fn a_dead_socket_shows_the_status_and_keeps_the_loaded_history(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_chat(&mut visual, &view);
    draw(&mut visual);
    assert!(
        visual.debug_bounds("chat-reconnecting").is_none(),
        "no reconnect status while the socket is healthy"
    );

    // The background task reports the failed attempt by setting the shared
    // flag; the next UI tick turns it into the status line.
    let shared = link(&mut visual, &view, true);
    tick(&mut visual, &view, Vec::new(), false);
    assert!(view.read_with(&*visual, |v, _| v.realtime_reconnecting));
    assert_loaded_chat(&view, &visual, 1);
    assert!(
        !view.read_with(&*visual, |v, _| v.events_overflow.load(Ordering::Relaxed)),
        "the status does not travel through the event queue"
    );
    draw(&mut visual);
    assert!(
        visual.debug_bounds("chat-reconnecting").is_some(),
        "the existing chat status area shows «Переподключаемся…»"
    );
    assert!(
        visual.debug_bounds("chat-history").is_some(),
        "the loaded history is still painted"
    );
    assert!(
        visual.debug_bounds("chat-composer").is_some(),
        "the composer is still available"
    );

    // A fresh `ready` is the confirmation: the task clears the flag before the
    // event is queued, and the next tick removes the status.
    shared.store(false, Ordering::Relaxed);
    tick(&mut visual, &view, vec![event("ready")], false);
    view.read_with(&*visual, |v, _| {
        assert!(!v.realtime_reconnecting);
    });
    assert_loaded_chat(&view, &visual, 1);
    draw(&mut visual);
    assert!(
        visual.debug_bounds("chat-history").is_some()
            && visual.debug_bounds("chat-composer").is_some(),
        "a confirmed connection still paints history and composer"
    );
}

#[gpui::test]
async fn a_full_queue_while_reconnecting_keeps_history_and_cannot_stick(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_chat(&mut visual, &view);

    // Overflow while the socket is down used to hide the history because a
    // service event was dropped. Status and overflow are now independent; a
    // dropped real event must still not hide the loaded timeline during the
    // retry, and the fresh connection resyncs it.
    let shared = link(&mut visual, &view, true);
    tick(&mut visual, &view, Vec::new(), true);
    assert!(view.read_with(&*visual, |v, _| v.realtime_reconnecting));
    assert_loaded_chat(&view, &visual, 1);
    draw(&mut visual);
    assert!(
        visual.debug_bounds("chat-reconnecting").is_some(),
        "overflow does not lose the retry status"
    );
    assert!(
        visual.debug_bounds("chat-history").is_some(),
        "overflow during the retry does not hide the history"
    );

    // `ready` flips the flag; even a queued event that was lost cannot leave
    // the status stuck because the tick reads the atomic directly.
    shared.store(false, Ordering::Relaxed);
    tick(&mut visual, &view, Vec::new(), false);
    view.read_with(&*visual, |v, _| {
        assert!(!v.realtime_reconnecting, "the status cannot stick");
        assert_eq!(v.messages.len(), 1);
        assert!(!v.access_pending);
    });

    // A full queue while connected still performs the ACL recheck: only the
    // service traffic is gone from the queue, not the real-event safety net.
    tick(&mut visual, &view, Vec::new(), true);
    view.read_with(&*visual, |v, _| {
        assert!(
            v.access_pending,
            "a dropped real event still unconfirms the channel access"
        );
        assert!(!v.realtime_reconnecting);
    });
}

#[gpui::test]
async fn starting_a_session_installs_the_live_link(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let client = v.client.clone().expect("client");
            v.start_realtime(client, window, cx);
            let link = v
                .realtime_link
                .as_ref()
                .expect("the session installs a link");
            assert!(!link.load(Ordering::Relaxed), "a new socket starts healthy");
            assert!(!v.realtime_reconnecting);
            v.stop_realtime();
            assert!(v.realtime_link.is_none(), "the session owns the link");
            assert!(!v.realtime_reconnecting);
        });
    });
}

#[gpui::test]
async fn a_stale_socket_flag_cannot_repaint_a_new_session(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();

    let stale = link(&mut visual, &view, true);
    tick(&mut visual, &view, Vec::new(), false);
    assert!(view.read_with(&*visual, |v, _| v.realtime_reconnecting));

    // Ending the session drops the link. The old task may still write its own
    // handle, but the UI reads only the current one.
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.stop_realtime();
            assert!(!v.realtime_reconnecting);
            assert!(v.realtime_link.is_none());
        });
    });
    stale.store(true, Ordering::Relaxed);
    tick(&mut visual, &view, Vec::new(), false);
    assert!(
        !view.read_with(&*visual, |v, _| v.realtime_reconnecting),
        "a stale task cannot repaint the next session"
    );
}
