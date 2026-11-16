//! R06 network and lifecycle races: overflow invalidation, list access errors,
//! late picker answers and DELETE results after the modal or server changed.

use super::recordings_tests::{
    at, calls, client, draw, list_json, recording_json, recording_without_preview, wait_arrival, ID,
};
use super::settings_tests::{make_app, wait_for};
use super::tests::message_json;
use super::*;
use crate::test_support::{Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use std::time::Duration;
use voxhold_api::{RECORDING_DELETED, RECORDING_READY, RECORDING_STARTED};

#[gpui::test]
async fn recording_overflow_unconfirms_the_snapshot_in_a_direct_chat(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let mut ready = recording_json(&"1".repeat(32), RECORDING_READY, 1);
    let mut live = recording_json(&"2".repeat(32), RECORDING_STARTED, 2);
    ready["preview_version"] = serde_json::Value::Null;
    live["preview_version"] = serde_json::Value::Null;
    let both = list_json(&[ready, live]);
    let server = TestServer::spawn(vec![
        Reply::new(200, &both),
        Reply::new(503, "{\"error\":\"unavailable\"}").with_delay(Duration::from_secs(1)),
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.confirmed_epoch == Some(v.recordings.access_epoch)
    });
    let previews_before = calls(&server, "/preview");

    // The queue overflowed, so `channels.changed` may be gone. A personal
    // conversation must still unconfirm the server snapshot as soon as the
    // batch is processed, and the next list is held open.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.conversation = Some(voxhold_api::Conversation {
                id: 99,
                direct: true,
                ..Default::default()
            });
            v.access_pending = true;
            v.events_overflow
                .store(true, std::sync::atomic::Ordering::SeqCst);
            v.process_realtime_batch(window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings.sorted(1_700_000_100).is_empty());
        assert!(v.recordings.live(7, 1_700_000_100).is_none());
    });

    // While the new GET is held no card, live marker or preview may appear.
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.conversation = None;
            v.access_pending = false;
        });
        window.draw(app).clear();
    });
    assert_eq!(
        calls(&server, "/preview"),
        previews_before,
        "an unconfirmed snapshot started a preview"
    );

    // A 503 does not re-confirm what the access change already invalidated.
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && !v.recordings.error.is_empty()
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings.sorted(1_700_000_100).is_empty());
        assert!(v.recordings.live(7, 1_700_000_100).is_none());
    });
}

#[gpui::test]
async fn recording_access_error_after_a_ws_event_does_not_retry(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[
        recording_without_preview(&"1".repeat(32), RECORDING_READY, 1),
        recording_without_preview(&"2".repeat(32), RECORDING_STARTED, 2),
    ]);
    let server = TestServer::spawn(vec![
        Reply::new(200, &ready),
        Reply::new(404, "{\"error\":\"not found\"}").with_delay(Duration::from_millis(300)),
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.confirmed_epoch == Some(v.recordings.access_epoch)
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.refresh_recordings(0, window, cx));
    });
    visual.update(|_, app| {
        view.update(app, |v, _| {
            let event = recording_without_preview(&"3".repeat(32), RECORDING_READY, 3);
            assert!(
                v.apply_realtime_event("recording.updated", &event)
                    .recording_event
            );
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && !v.recordings.error.is_empty()
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.recordings.confirmed_epoch, None,
            "error={} active={:?}",
            v.recordings.error, v.recordings.active
        );
        assert!(v.recordings.sorted(1_700_000_100).is_empty());
        assert!(v.recordings.live(7, 1_700_000_100).is_none());
    });
    assert_eq!(
        calls(&server, "/recordings"),
        2,
        "a 404 after a WS event started a retry"
    );
}

#[gpui::test]
async fn recording_tombstone_survives_a_404_and_an_old_ready(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let deleted_id = "1".repeat(32);
    let kept_id = "2".repeat(32);
    let mut deleted = recording_json(&deleted_id, RECORDING_READY, 1);
    let mut kept = recording_json(&kept_id, RECORDING_READY, 2);
    deleted["preview_version"] = serde_json::Value::Null;
    kept["preview_version"] = serde_json::Value::Null;
    let both = list_json(&[deleted.clone(), kept.clone()]);
    let tombstone = serde_json::to_string(&serde_json::json!({
        "id": deleted_id,
        "server_id": 1,
        "channel_id": 7,
        "owner_user_id": 1,
        "owner_username": "owner",
        "status": RECORDING_DELETED,
        "updated_at": 3,
        "expires_at": 1_800_000_000,
    }))
    .unwrap();
    let server = TestServer::spawn(vec![
        Reply::new(200, &both),
        Reply::new(200, &tombstone),
        Reply::new(404, "{\"error\":\"not found\"}"),
        Reply::new(200, &both),
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.records.len() == 2
    });
    let row = view.read_with(&*visual, |v, _| {
        v.recordings
            .records
            .iter()
            .find(|row| row.id == deleted_id)
            .cloned()
            .unwrap()
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.delete_recording(row, window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        v.recordings.tombstones.contains(&deleted_id)
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| v.refresh_recordings(0, window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && !v.recordings.error.is_empty()
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.recordings.confirmed_epoch, None);
        assert!(v.recordings.sorted(1_700_000_100).is_empty());
        assert!(v.recordings.tombstones.contains(&deleted_id));
    });

    // An old ready event unconfirms again but must not drop the tombstone; the
    // successful list still hides the deleted row instead of resurrecting it.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            tx.send(voxhold_api::RealtimeEvent {
                kind: "ready".into(),
                ..Default::default()
            })
            .unwrap();
            v.events = Some(std::sync::Arc::new(std::sync::Mutex::new(rx)));
            v.events_overflow = Default::default();
            v.process_realtime_batch(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.confirmed_epoch == Some(v.recordings.access_epoch)
    });
    view.read_with(&*visual, |v, _| {
        let ids: Vec<String> = v
            .recordings
            .sorted(1_700_000_100)
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, vec![kept_id.clone()]);
        assert!(v.recordings.tombstones.contains(&deleted_id));
    });
}

#[gpui::test]
async fn late_recording_pick_cannot_clear_another_attempt(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[recording_without_preview(ID, RECORDING_READY, 1)]);
    let server = TestServer::spawn(vec![Reply::new(200, &ready)]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.records.len() == 1
    });
    let path = std::env::temp_dir().join("voxhold-late-recording-pick.mp4");
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let first = v.next_recording_op(ID).unwrap();
            let second = v.next_recording_op(ID).unwrap();
            if let Some(dialog) = v.recordings_dialog.as_mut() {
                dialog.busy = ID.into();
                dialog.busy_attempt = second.attempt;
                dialog.confirm = ID.into();
                dialog.error = "operation B".into();
            }
            // Both answers belong to A; neither may clear B's busy state, its
            // confirmation, its error, or request a ticket.
            v.resolve_recording_pick(first.clone(), None, cx);
            v.resolve_recording_pick(first, Some(path), cx);
            let dialog = v.recordings_dialog.as_ref().unwrap();
            assert_eq!(dialog.busy, ID);
            assert_eq!(dialog.busy_attempt, second.attempt);
            assert_eq!(dialog.confirm, ID);
            assert_eq!(dialog.error, "operation B");
            let _ = window;
        });
    });
    assert_eq!(calls(&server, "/download"), 0);
}

#[gpui::test]
async fn recording_delete_requires_a_confirmed_snapshot(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[recording_without_preview(ID, RECORDING_READY, 1)]);
    let server = TestServer::spawn(vec![Reply::new(200, &ready)]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.records.len() == 1
    });
    let row = view.read_with(&*visual, |v, _| v.recordings.records[0].clone());
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            // A row from an old frame is still in `records`, but the snapshot
            // is no longer confirmed for the current access generation.
            v.recording_access_epoch = v.recording_access_epoch.wrapping_add(1);
            v.recordings.set_access_epoch(v.recording_access_epoch);
            v.delete_recording(row, window, cx);
            assert!(v
                .recordings_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.busy.is_empty()));
        });
    });
    visual.run_until_parked();
    assert!(
        !server
            .requests()
            .iter()
            .any(|request| request.method == "DELETE"),
        "an unconfirmed row reached DELETE"
    );
}

#[gpui::test]
async fn recording_delete_closed_dialog_still_applies_the_tombstone(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let deleted_id = "1".repeat(32);
    let kept_id = "2".repeat(32);
    let both = list_json(&[
        recording_without_preview(&deleted_id, RECORDING_READY, 1),
        recording_without_preview(&kept_id, RECORDING_READY, 2),
    ]);
    let remaining = list_json(&[recording_without_preview(&kept_id, RECORDING_READY, 2)]);
    let tombstone = serde_json::to_string(&serde_json::json!({
        "id": deleted_id,
        "server_id": 1,
        "channel_id": 7,
        "owner_user_id": 1,
        "owner_username": "owner",
        "status": RECORDING_DELETED,
        "updated_at": 3,
        "expires_at": 1_800_000_000,
    }))
    .unwrap();
    let server = TestServer::spawn(vec![
        Reply::new(200, &both),
        Reply::new(200, &tombstone).with_delay(Duration::from_millis(300)),
        Reply::new(200, &remaining),
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.records.len() == 2
    });
    let row = view.read_with(&*visual, |v, _| {
        v.recordings
            .records
            .iter()
            .find(|row| row.id == deleted_id)
            .cloned()
            .unwrap()
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.delete_recording(row, window, cx);
            assert_eq!(
                v.recordings_dialog
                    .as_ref()
                    .map(|dialog| dialog.busy.as_str()),
                Some(deleted_id.as_str()),
                "delete did not start"
            );
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.close_recordings(true, window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        v.recordings.tombstones.contains(&deleted_id)
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.recordings.records.iter().any(|row| row.id == deleted_id));
    });

    // Reopening starts from a fresh attempt, not the closed one's busy row.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading
            && v.recordings_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.rows.len() == 1)
    });
    view.read_with(&*visual, |v, _| {
        let dialog = v.recordings_dialog.as_ref().unwrap();
        assert!(dialog.busy.is_empty());
        assert_eq!(dialog.busy_attempt, 0);
        assert!(dialog.confirm.is_empty());
        assert_eq!(dialog.rows[0].record.id, kept_id);
    });
}

#[gpui::test]
async fn recording_delete_from_an_old_server_is_discarded(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[recording_without_preview(ID, RECORDING_READY, 1)]);
    let tombstone = serde_json::to_string(&serde_json::json!({
        "id": ID,
        "server_id": 1,
        "channel_id": 7,
        "owner_user_id": 1,
        "owner_username": "owner",
        "status": RECORDING_DELETED,
        "updated_at": 3,
        "expires_at": 1_800_000_000,
    }))
    .unwrap();
    let server = TestServer::spawn(vec![
        Reply::new(200, &ready),
        Reply::new(200, &tombstone).with_delay(Duration::from_millis(300)),
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.records.len() == 1
    });
    let row = view.read_with(&*visual, |v, _| v.recordings.records[0].clone());
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.delete_recording(row, window, cx);
            assert!(
                v.recordings_dialog
                    .as_ref()
                    .is_some_and(|dialog| !dialog.busy.is_empty()),
                "delete did not start"
            );
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.reset_recordings(window, cx);
            v.server = 2;
            v.recordings.server_id = 2;
        });
    });
    visual.run_until_parked();
    std::thread::sleep(Duration::from_millis(450));
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert!(!v.recordings.tombstones.contains(ID));
        assert!(v.recordings.records.is_empty());
    });
}

#[gpui::test]
async fn recording_backdrop_blocks_right_click_and_wheel_over_history(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[recording_without_preview(ID, RECORDING_READY, 1)]);
    let server = TestServer::spawn(vec![Reply::new(200, &ready)]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(900.)));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channel = 7;
            v.messages = (0..40)
                .map(|index| {
                    serde_json::from_value(message_json(
                        100 + index,
                        1,
                        &format!("history row {index}"),
                    ))
                    .unwrap()
                })
                .collect();
            v.toggle_recordings("header", window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    draw(&mut visual);
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.history_list.scroll_to(gpui::ListOffset {
                item_ix: 5,
                offset_in_item: gpui::px(0.),
            });
        });
        window.draw(app).clear();
    });
    visual.run_until_parked();
    let (point, before) = view.read_with(&*visual, |v, _| {
        let dialog = v.recordings_dialog.as_ref().unwrap();
        let row = v.history_list.bounds_for_item(5).expect("history row");
        let x = if f32::from(row.left()) < dialog.bounds.x {
            f32::from(row.left()) + 10.
        } else {
            f32::from(row.right()) - 10.
        };
        let y = f32::from(row.center().y);
        assert!(
            !dialog.bounds.contains((x, y)),
            "the history point must be outside the modal card"
        );
        let top = v.history_list.logical_scroll_top();
        (at(x, y), (top.item_ix, f32::from(top.offset_in_item)))
    });
    visual.simulate_mouse_down(point, MouseButton::Right, Default::default());
    visual.simulate_mouse_up(point, MouseButton::Right, Default::default());
    view.read_with(&*visual, |v, _| {
        assert!(v.message_menu.is_none(), "the background menu opened");
        assert!(
            v.recordings_dialog.is_some(),
            "the modal closed on right click"
        );
    });
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: point,
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-120.))),
        ..Default::default()
    });
    view.read_with(&*visual, |v, _| {
        let top = v.history_list.logical_scroll_top();
        assert_eq!(
            (top.item_ix, f32::from(top.offset_in_item)),
            before,
            "the background history scrolled through the modal backdrop"
        );
    });
}

#[cfg(feature = "voxhold-media")]
#[gpui::test]
async fn recording_voice_close_falls_back_when_the_call_panel_is_hidden(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.voice_channel = 7;
            v.channel = 7;
            v.access_pending = false;
            assert!(v.recordings_voice_panel_visible());
            v.access_pending = true;
            assert!(
                !v.recordings_voice_panel_visible(),
                "a hidden call panel must not receive focus"
            );
            v.recordings_dialog = Some(crate::recordings_dialog::DialogState::new(
                1,
                1,
                v.recording_access_epoch,
            ));
            v.recordings_voice_entry = true;
            v.close_recordings(true, window, cx);
            assert!(v.recordings_entry_focus.is_focused(window));
        });
    });
}

#[gpui::test]
async fn recording_backdrop_requires_one_completed_outside_click(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[recording_without_preview(ID, RECORDING_READY, 1)]);
    let server = TestServer::spawn(vec![Reply::new(200, &ready)]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(900.)));
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    draw(&mut visual);
    let (outside, inside) = view.read_with(&*visual, |v, _| {
        let dialog = v.recordings_dialog.as_ref().unwrap();
        let outside = (dialog.bounds.x - 20., dialog.bounds.y + 30.);
        let inside = (dialog.bounds.x + 12., dialog.bounds.y + 12.);
        assert!(!dialog.bounds.contains(outside));
        assert!(dialog.bounds.contains(inside));
        (outside, inside)
    });

    // Press outside, release inside: the card consumes the release and must
    // also drop the backdrop press, or the next gesture closes the modal.
    visual.simulate_mouse_down(
        at(outside.0, outside.1),
        MouseButton::Left,
        Default::default(),
    );
    view.read_with(&*visual, |v, _| {
        assert!(v
            .recordings_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.backdrop_pressed));
    });
    visual.simulate_mouse_up(
        at(inside.0, inside.1),
        MouseButton::Left,
        Default::default(),
    );
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings_dialog.is_some());
        assert!(v
            .recordings_dialog
            .as_ref()
            .is_some_and(|dialog| !dialog.backdrop_pressed));
    });

    // A second gesture starting inside and ending outside must not complete the
    // first press either.
    visual.simulate_mouse_down(
        at(inside.0, inside.1),
        MouseButton::Left,
        Default::default(),
    );
    visual.simulate_mouse_up(
        at(outside.0, outside.1),
        MouseButton::Left,
        Default::default(),
    );
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings_dialog.is_some());
    });

    // One click fully outside still closes the dialog.
    visual.simulate_click(at(outside.0, outside.1), gpui::Modifiers::default());
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings_dialog.is_none());
    });
}
