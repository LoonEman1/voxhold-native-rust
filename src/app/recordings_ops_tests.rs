//! R06 network and lifecycle races: overflow invalidation, list access errors,
//! late picker answers and DELETE results after the modal or server changed.

use super::recordings_tests::{
    calls, client, list_json, recording_json, recording_without_preview, ID,
};
use super::settings_tests::{make_app, wait_for};
use super::*;
use crate::test_support::{Gate, Reply, TestServer};
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
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &both),
        Reply::new(503, "{\"error\":\"unavailable\"}").with_gate(gate.clone()),
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
    gate.arm();
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
    gate.release();

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
    wait_for(&mut visual, &view, |v| v.recordings.tombstones.contains(&deleted_id));

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
    let row = view.read_with(&*visual, |v, _| {
        v.recordings.records[0].clone()
    });
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
