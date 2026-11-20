//! CF-06: a loaded history window that ends before the live tail must not close
//! the gap with one fresh realtime row. `history_has_newer` means "there is an
//! unloaded range between this window and the tail"; numeric id differences
//! cannot prove continuity, so a created row behind the right edge is not
//! inserted and the accumulated read receipt stays unsent. "Рљ РїРѕСЃР»РµРґРЅРёРј"
//! replaces the window only after a successful latest page and replays the rows
//! that arrived while that page loaded, without resurrecting tombstones.

use super::super::*;
use super::message_json;
use super::w08a_p2::{channel_row, direct_client, make_app, wait_for};
use crate::test_support::{Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SERVER: i64 = 1;
const CHANNEL: i64 = 7;
const DM: i64 = 7;

fn dto(id: i64, revision: i64, content: &str) -> voxhold_api::Message {
    let mut value = message_json(id, revision, content);
    value["channel_id"] = json!(CHANNEL);
    serde_json::from_value(value).expect("message dto")
}

fn created(id: i64, content: &str) -> serde_json::Value {
    message_json(id, 1, content)
}

fn dm_row(id: i64, content: &str) -> serde_json::Value {
    json!({
        "id": id,
        "channel_id": 0,
        "conversation_id": DM,
        "content": content,
        "author": {"user_id": 1, "username": "neva"},
        "created_at": 1,
        "revision": 1,
        "comment_count": 0,
        "client_message_id": "",
        "reactions": [],
        "reactions_revision": 0,
        "attachments": [],
        "poll": null
    })
}

/// A conversation page uses the top-level `has_more`; the client derives the
/// cursor from the first row.
fn dm_page(ids: &[i64], has_more: bool) -> String {
    json!({
        "messages": ids
            .iter()
            .map(|id| dm_row(*id, "dm row"))
            .collect::<Vec<_>>(),
        "has_more": has_more,
    })
    .to_string()
}

fn dm_conversation(last_message_id: i64) -> voxhold_api::Conversation {
    serde_json::from_value(json!({
        "id": DM,
        "name": "neva",
        "direct": true,
        "members": [
            {"user_id": 1, "username": "me"},
            {"user_id": 2, "username": "neva"}
        ],
        "unread_count": 0,
        "saved": false,
        "muted": false,
        "last_message_id": last_message_id
    }))
    .expect("conversation")
}

fn event(kind: &str, data: serde_json::Value) -> voxhold_api::RealtimeEvent {
    voxhold_api::RealtimeEvent {
        kind: kind.into(),
        data,
        ..Default::default()
    }
}

/// Deliver one batch through the real pump, exactly like `process_realtime_batch`
/// does for a queued socket event.
fn deliver(
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

/// Pump the executor for a bounded time so a background HTTP answer reaches its
/// callback even when the test asserts on the absence of a request.
fn settle(visual: &mut VisualTestContext) {
    for _ in 0..100 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// An old anchored window around ids 1..=10 whose right edge is separated from
/// the tail by an unloaded range. The window is what `/context` or a quote jump
/// leaves behind.
fn setup_old_window(visual: &mut VisualTestContext, view: &Entity<Voxhold>) {
    visual.update(|window, app| {
        window.activate_window();
        view.update(app, |v, _| {
            v.server = SERVER;
            v.channel = CHANNEL;
            v.personal_mode = false;
            v.conversation = None;
            v.access_pending = false;
            v.access_reload_in_flight = false;
            v.self_user_id = 1;
            v.channels = vec![channel_row(CHANNEL, "test")];
            v.messages = (1..=10).map(|id| dto(id, 1, "loaded")).collect();
            v.before = Some(1);
            v.has_more = true;
            v.history_has_newer = true;
            v.read_sent = 0;
            v.read_pending = false;
            v.read_retry_at = None;
        });
    });
}

fn latest_page(ids: &[i64]) -> String {
    json!({
        "messages": ids
            .iter()
            .map(|id| {
                let mut row = message_json(*id, 1, "tail");
                row["channel_id"] = json!(CHANNEL);
                row
            })
            .collect::<Vec<_>>(),
        "pagination": {"next_before_id": null, "has_more": false},
    })
    .to_string()
}

fn read_requests(server: &TestServer) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .filter(|record| record.path.ends_with("/read") || record.path.contains("/notifications"))
        .map(|record| format!("{} {}", record.method, record.path))
        .collect()
}

/// (a) A created row behind the gap stays out of the old window, while edits,
/// reactions and deletions of loaded rows keep working. The accumulated read
/// receipt is not sent while the gap is open.
#[gpui::test]
async fn a_fresh_row_behind_the_gap_stays_out_and_blocks_reads(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    deliver(
        &mut visual,
        &view,
        vec![
            event("message.created", created(1000, "tail row")),
            event("message.updated", created(5, "edited")),
            event(
                "message.reactions_changed",
                json!({"channel_id":7,"message_id":5,"revision":3,
                        "reactions":[{"emoji":"+1","count":1,"me":true}]}),
            ),
            event("message.deleted", json!({"channel_id":7,"message_id":6})),
        ],
    );

    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![1, 2, 3, 4, 5, 7, 8, 9, 10],
            "the old window keeps its range, loses the tombstone and never gains 1000"
        );
        assert!(v.history_has_newer, "the gap is still unloaded");
        let edited = v.messages.iter().find(|row| row.id == 5).expect("row 5");
        assert_eq!(edited.content, "edited", "an edit of a loaded row applies");
        assert_eq!(edited.reactions_revision, 3);
        assert!(
            edited.reactions.iter().any(|reaction| reaction.me),
            "a reaction on a loaded row applies"
        );
        assert!(v.deleted_ids.contains(&6));
        assert!(!v.pending_edits.contains_key(&1000));
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| v.mark_visible_read(10, window, cx));
    });
    settle(&mut visual);
    assert!(
        read_requests(&server).is_empty(),
        "no read receipt may announce the old window while the gap is open: {:?}",
        read_requests(&server)
    );
}

/// (b) "Рљ РїРѕСЃР»РµРґРЅРёРј" loads the latest page while a fresh row arrives. After the
/// answer the window is the fresh page plus that row, each exactly once, and the
/// gap is closed.
///
/// The refresh runs on the GPUI background executor with a blocking `block_on`,
/// so holding `Gate` across `run_until_parked` would block the test thread until
/// the client timeout. The fixture instead takes the reset request slot, feeds
/// the realtime batch, and only then lets the (delayed) answer apply; the row
/// still arrives strictly after the latest request started and before its
/// answer.
#[gpui::test]
async fn latest_page_replays_events_that_arrived_while_it_loaded(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(200, &latest_page(&[1000, 1001]))
        .with_path("/api/v1/servers/1/channels/7/messages")
        .with_delay(Duration::from_millis(200))]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    let request = visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.reset_for_latest();
            v.refresh_history(window, cx);
            v.refresh_request
        })
    });
    assert!(request.is_some(), "the latest page request owns the slot");
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.messages.last().map(|row| row.id),
            Some(10),
            "the old window stays visible until the answer"
        );
        assert!(v.history_has_newer);
    });

    deliver(
        &mut visual,
        &view,
        vec![event("message.created", created(1002, "raced row"))],
    );
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.messages.last().map(|row| row.id),
            Some(10),
            "a fresh row must not be appended to the old window"
        );
    });

    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && v.messages.iter().any(|row| row.id == 1002)
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![1000, 1001, 1002]);
        assert_eq!(
            ids.iter().filter(|id| **id == 1002).count(),
            1,
            "the raced row is replayed once"
        );
        assert!(
            !v.history_has_newer,
            "the confirmed latest page closes the gap"
        );
    });
}

/// (c) A row deleted while the latest page loads does not come back from that
/// page; the answer still replaces the old window.
#[gpui::test]
async fn a_row_deleted_during_the_latest_load_stays_deleted(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(200, &latest_page(&[1000, 1001]))
        .with_path("/api/v1/servers/1/channels/7/messages")
        .with_delay(Duration::from_millis(200))]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.reset_for_latest();
            v.refresh_history(window, cx);
        });
    });
    deliver(
        &mut visual,
        &view,
        vec![event(
            "message.deleted",
            json!({"channel_id":7,"message_id":1000}),
        )],
    );
    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && v.messages.iter().any(|row| row.id == 1001)
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![1001], "the tombstone survives the latest page");
        assert!(v.deleted_ids.contains(&1000));
        assert!(!v.history_has_newer);
    });
}

/// (d) A failed latest answer leaves the old window, its gap and the read
/// guard untouched.
#[gpui::test]
async fn a_failed_latest_page_keeps_the_gap_and_the_read_guard(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server =
        TestServer::spawn(vec![Reply::new(500, r#"{"error":"failed"}"#)
            .with_path("/api/v1/servers/1/channels/7/messages")]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.reset_for_latest();
            v.refresh_history(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && !v.error.is_empty()
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            (1..=10).collect::<Vec<_>>(),
            "the old window survives a failed latest page"
        );
        assert!(v.history_has_newer, "the gap is still open");
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| v.mark_visible_read(10, window, cx));
    });
    settle(&mut visual);
    assert!(
        read_requests(&server).is_empty(),
        "the failed reset keeps the read guard: {:?}",
        read_requests(&server)
    );
}

/// (e) The bounded replay buffer drops its rows on overflow and asks for the
/// normal refresh repeat; the repeat reads the latest page again instead of
/// growing the buffer without limit.
#[gpui::test]
async fn an_overflowed_latest_buffer_repeats_the_refresh(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &latest_page(&[1000, 1001]))
            .with_path("/api/v1/servers/1/channels/7/messages"),
        Reply::new(200, &latest_page(&[1000, 1001]))
            .with_path("/api/v1/servers/1/channels/7/messages"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.reset_for_latest();
            v.refresh_history(window, cx);
        });
    });

    // 1001 rows is more than the bounded replay buffer accepts while the page
    // is in flight. None of them may enter the old window.
    visual.update(|_, app| {
        view.update(app, |v, _| {
            for id in 2000..=3000 {
                v.merge_message(true, dto(id, 1, "raced"));
            }
            assert_eq!(
                v.messages.last().map(|row| row.id),
                Some(10),
                "overflowed events still must not touch the old window"
            );
        });
    });

    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && !v.history_has_newer
    });
    settle(&mut visual);
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![1000, 1001],
            "the latest page, not the overflowed buffer, owns the window"
        );
    });
    let page_requests = server
        .requests()
        .into_iter()
        .filter(|record| record.path.contains("/channels/7/messages"))
        .count();
    assert_eq!(
        page_requests, 2,
        "the overflow uses the normal refresh repeat"
    );
}

/// An ordinary (not reset) refresh that replaces the fragment with the latest
/// page must replay the rows it held for that page. Otherwise the gap reappears
/// as a hole that later rows cannot fill.
#[gpui::test]
async fn ordinary_refresh_closes_the_gap_without_losing_raced_rows(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(200, &latest_page(&[1000, 1001]))
        .with_path("/api/v1/servers/1/channels/7/messages")
        .with_delay(Duration::from_millis(200))]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    // No "Рљ РїРѕСЃР»РµРґРЅРёРј": this is the reconnect/resync path.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.refresh_history(window, cx));
    });
    deliver(
        &mut visual,
        &view,
        vec![event("message.created", created(1002, "raced row"))],
    );
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.messages.last().map(|row| row.id),
            Some(10),
            "the old fragment must not absorb the fresh row"
        );
        assert!(v.history_has_newer);
        assert!(!v.history_reset_pending, "no reset owns this refresh");
    });

    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && !v.history_has_newer
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![1000, 1001, 1002],
            "the ordinary refresh replays the row it held"
        );
    });

    deliver(
        &mut visual,
        &view,
        vec![event("message.created", created(1003, "next row"))],
    );
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![1000, 1001, 1002, 1003],
            "after the confirmed page the next row is appended, not lost"
        );
        assert!(!v.history_has_newer);
    });
}

/// A buffered row deleted while "Рљ РїРѕСЃР»РµРґРЅРёРј" loads is a tombstone, not a page
/// row: neither the replay set nor the page may bring it back.
#[gpui::test]
async fn a_buffered_row_deleted_during_the_latest_load_stays_deleted(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(200, &latest_page(&[1000, 1001]))
        .with_path("/api/v1/servers/1/channels/7/messages")
        .with_delay(Duration::from_millis(200))]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.reset_for_latest();
            v.refresh_history(window, cx);
        });
    });
    deliver(
        &mut visual,
        &view,
        vec![
            event("message.created", created(1002, "raced row")),
            event("message.deleted", json!({"channel_id":7,"message_id":1002})),
        ],
    );
    view.read_with(&*visual, |v, _| {
        assert!(v.deleted_ids.contains(&1002));
        assert_eq!(v.messages.last().map(|row| row.id), Some(10));
    });

    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && !v.history_has_newer
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![1000, 1001], "the tombstone wins over the buffer");
        assert!(v.deleted_ids.contains(&1002));
    });
}

/// The confirmed latest page clears the gap, so the accumulated read receipt is
/// allowed again.
#[gpui::test]
async fn a_confirmed_latest_page_unblocks_the_read_receipt(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &latest_page(&[1000, 1001]))
            .with_path("/api/v1/servers/1/channels/7/messages"),
        Reply::new(200, "{}"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.reset_for_latest();
            v.refresh_history(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && !v.history_has_newer
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| v.mark_visible_read(1001, window, cx));
    });
    settle(&mut visual);
    let reads = read_requests(&server);
    assert_eq!(
        reads.len(),
        1,
        "the closed gap allows one read receipt: {reads:?}"
    );
    assert_eq!(reads[0], "PUT /api/v1/servers/1/channels/7/read");
}

/// A conversation page that already ends at `last_message_id` reaches the live
/// edge; a numeric id difference alone is not a gap.
#[gpui::test]
async fn a_conversation_jump_at_the_tail_does_not_invent_a_gap(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &dm_page(&[10, 15, 20], false))
            .with_path("/api/v1/conversations/7/messages?before="),
        Reply::new(200, &dm_page(&[], false)).with_path("/api/v1/conversations/7/messages"),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#)
            .with_path("/api/v1/conversations/7/notifications"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let row = dm_conversation(20);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.open_conversation_message(DM, 15, window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && v.messages.iter().any(|row| row.id == 15)
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![10, 15, 20]);
        assert!(
            !v.history_has_newer,
            "the page ends at last_message_id, so no gap is open"
        );
        assert_eq!(
            v.highlighted_message.map(|(id, _)| id),
            Some(15),
            "the target is highlighted"
        );
        assert!(v.error.is_empty());
    });
}

/// The same jump with a newer tail keeps `history_has_newer` set.
#[gpui::test]
async fn a_conversation_jump_before_the_tail_keeps_the_gap(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &dm_page(&[10, 15, 20], false))
            .with_path("/api/v1/conversations/7/messages?before="),
        Reply::new(200, &dm_page(&[], false)).with_path("/api/v1/conversations/7/messages"),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#)
            .with_path("/api/v1/conversations/7/notifications"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let row = dm_conversation(99);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.open_conversation_message(DM, 15, window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none() && v.messages.iter().any(|row| row.id == 15)
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![10, 15, 20]);
        assert!(
            v.history_has_newer,
            "last_message_id is newer than the loaded tail"
        );
        assert_eq!(v.highlighted_message.map(|(id, _)| id), Some(15));
    });
}

/// "Рљ РїРѕСЃР»РµРґРЅРёРј" clicked while a conversation jump holds the request slot is
/// repeated after the jump, so the click is not lost.
#[gpui::test]
async fn latest_clicked_during_a_conversation_jump_is_not_lost(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &dm_page(&[10, 15, 20], false))
            .with_path("/api/v1/conversations/7/messages?before=")
            .with_delay(Duration::from_millis(200)),
        Reply::new(200, &dm_page(&[], false)).with_path("/api/v1/conversations/7/messages"),
        Reply::new(200, &dm_page(&[90, 91], false)).with_path("/api/v1/conversations/7/messages"),
        Reply::new(200, r#"{"conversation_id":7,"muted":false}"#)
            .with_path("/api/v1/conversations/7/notifications"),
    ]);
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let row = dm_conversation(99);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal_mode = true;
            v.conversations = vec![row.clone()];
            v.open_conversation_message(DM, 15, window, cx);
            v.reset_for_latest();
            v.refresh_history(window, cx);
            assert!(
                v.refresh_again,
                "the running jump owns the slot and defers the reset"
            );
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.refresh_request.is_none()
            && v.messages.first().map(|row| row.id) == Some(90)
            && !v.history_has_newer
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec![90, 91],
            "the deferred reset replaced the jump window"
        );
    });
}

/// An ordinary refresh without a gap keeps the `refresh_new_ids` contract: rows
/// inserted while the page was in flight survive when they are newer than it.
#[gpui::test]
async fn an_ordinary_refresh_without_a_gap_keeps_new_ids(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, direct_client(&server));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    setup_old_window(&mut visual, &view);

    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.history_has_newer = false;
            v.refresh_request = Some(1);
            v.merge_message(true, dto(20, 1, "during the page"));
            assert!(v.refresh_new_ids.contains(&20));
            let new_ids = std::mem::take(&mut v.refresh_new_ids);
            let page = voxhold_api::MessagePage {
                messages: vec![dto(5, 1, "page"), dto(10, 1, "page")],
                pagination: voxhold_api::Pagination {
                    next_before_id: Some(5),
                    has_more: true,
                },
            };
            v.refresh_request = None;
            v.apply_history_snapshot(page, new_ids);
            let ids: Vec<i64> = v.messages.iter().map(|row| row.id).collect();
            assert_eq!(ids, vec![5, 10, 20], "a newer in-flight row survives");
        });
    });
}
