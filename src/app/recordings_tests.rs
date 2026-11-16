//! R06 regressions: the recordings API, snapshot races, access confirmation,
//! previews and clipped card actions.

use super::settings_tests::{make_app, wait_for};
use super::*;
use crate::test_support::{Gate, Reply, ReplyBody, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use std::time::{Duration, Instant};
use voxhold_api::{Recording, RECORDING_DELETED, RECORDING_READY, RECORDING_STARTED};

const ID: &str = "0123456789abcdef0123456789abcdef";

fn recording_json(id: &str, status: &str, updated: i64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "server_id": 1,
        "channel_id": 7,
        "owner_user_id": 1,
        "owner_username": "owner",
        "status": status,
        "mime_type": "video/mp4",
        "size_bytes": 1024,
        "duration_ms": 61_500,
        "created_at": 1_700_000_000,
        "updated_at": updated,
        "expires_at": 1_800_000_000,
        "preview_version": "etag-1234",
    })
}

fn list_json(rows: &[serde_json::Value]) -> String {
    serde_json::json!({
        "recordings": rows,
        "quota_bytes": (6i64 << 30),
        "max_file_bytes": (2i64 << 30),
        "retention_seconds": 86_400,
    })
    .to_string()
}

fn record(id: &str, status: &str, updated: i64) -> Recording {
    serde_json::from_value(recording_json(id, status, updated)).unwrap()
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::new(width, height));
    let mut out = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    out
}

fn client(base: &str) -> voxhold_api::Client {
    let mut client = voxhold_api::Client::new(base).unwrap();
    client.set_token("token".into());
    client
}

fn calls(server: &TestServer, suffix: &str) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| request.path.contains(suffix))
        .count()
}

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn wait_arrival(visual: &mut VisualTestContext, gate: &Gate) {
    for _ in 0..1000 {
        if gate.wait_arrived(Duration::ZERO) {
            return;
        }
        visual.update(|window, app| {
            window.draw(app).clear();
        });
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the gated request never arrived");
}

#[test]
fn recording_list_contract_and_limits() {
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &list_json(&[recording_json(ID, RECORDING_READY, 1)]),
    )]);
    let list = run(client(&server.base).list_recordings(1)).unwrap();
    assert_eq!(server.requests()[0].path, "/api/v1/servers/1/recordings");
    assert_eq!(list.quota_bytes, 6 << 30);
    assert_eq!(list.max_file_bytes, 2 << 30);
    assert_eq!(list.retention_seconds, 86_400);
    assert_eq!(list.recordings[0].id, ID);
    assert_eq!(
        list.recordings[0].preview_version.as_deref(),
        Some("etag-1234")
    );
    assert_eq!(
        list.recordings[0].filename(),
        "voxhold-20231114-221320-01234567.mp4"
    );
}

#[test]
fn recording_get_and_delete_use_the_record_route() {
    let ready = serde_json::to_string(&recording_json(ID, RECORDING_READY, 1)).unwrap();
    let deleted = serde_json::to_string(&recording_json(ID, RECORDING_DELETED, 2)).unwrap();
    let server = TestServer::spawn(vec![Reply::new(200, &ready), Reply::new(200, &deleted)]);
    let client = client(&server.base);
    let got = run(client.get_recording(ID)).unwrap();
    assert_eq!(got.id, ID);
    let removed = run(client.delete_recording(ID)).unwrap();
    assert_eq!(removed.status, RECORDING_DELETED);
    let requests = server.requests();
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, format!("/api/v1/recordings/{ID}"));
    assert_eq!(requests[1].method, "DELETE");
    assert_eq!(requests[1].path, format!("/api/v1/recordings/{ID}"));
}

#[test]
fn recording_ids_are_validated_before_any_request() {
    let server = TestServer::spawn(Vec::new());
    let client = client(&server.base);
    for id in [
        "",
        "short",
        &"z".repeat(32),
        &"A".repeat(32),
        "0123456789abcdef0123456789abcde/",
    ] {
        assert!(run(client.get_recording(id)).is_err());
        assert!(run(client.delete_recording(id)).is_err());
        assert!(run(client.recording_download_ticket(id)).is_err());
        assert!(run(client.recording_preview(id, "v")).is_err());
    }
    assert!(
        server.requests().is_empty(),
        "invalid ids reached the server"
    );
}

#[test]
fn recording_preview_requires_jpeg_and_bounds() {
    let valid = jpeg(320, 180);
    let jpeg_reply = || Reply {
        status: 200,
        body: ReplyBody::Bytes(valid.clone()),
        content_type: "image/jpeg",
        gate: None,
        delay: None,
    };
    let server = TestServer::spawn(vec![jpeg_reply()]);
    let image = run(client(&server.base).recording_preview(ID, "etag-1234")).unwrap();
    assert_eq!((image.width(), image.height()), (320, 180));

    let server = TestServer::spawn(vec![Reply::new(200, "<html>")]);
    assert!(run(client(&server.base).recording_preview(ID, "etag-1234")).is_err());

    let server = TestServer::spawn(vec![Reply {
        status: 200,
        body: ReplyBody::Bytes(vec![0; (128 << 10) + 1]),
        content_type: "image/jpeg",
        gate: None,
        delay: None,
    }]);
    assert!(run(client(&server.base).recording_preview(ID, "etag-1234")).is_err());
}

#[test]
fn recording_ticket_stays_on_the_recording_route() {
    for (url, valid) in [
        ("/api/v1/recording-downloads/YWJj.ZGVm", true),
        ("https://evil.example/api/v1/recording-downloads/a.b", false),
        ("//evil.example/api/v1/recording-downloads/a.b", false),
        ("/api/v1/recording-downloads/../../admin", false),
        ("/api/v1/recording-downloads/a/b", false),
        ("/api/v1/recording-downloads/a.b?x=1", false),
        ("", false),
    ] {
        let server = TestServer::spawn(vec![Reply::new(
            200,
            &serde_json::json!({"url": url}).to_string(),
        )]);
        let ticket = run(client(&server.base).recording_download_ticket(ID));
        if valid {
            assert_eq!(ticket.unwrap(), url);
        } else {
            assert!(ticket.is_err(), "{url}");
        }
    }
}

#[test]
fn recording_download_checks_size_and_rejects_redirects() {
    const TICKET: &str = "/api/v1/recording-downloads/YWJj.ZGVm";
    for (status, body, expect, ok) in [
        (200, "1234", 4, true),
        (200, "1234", 0, true),
        (200, "1234", 8, false),
        (302, "", 0, false),
        (403, "", 0, false),
    ] {
        let server = TestServer::spawn(vec![Reply::new(status, body)]);
        let mut out = Vec::new();
        let result =
            run(
                client(&server.base).download_recording_to(TICKET, expect, &mut |chunk| {
                    out.extend_from_slice(chunk);
                    Ok(())
                }),
            );
        assert_eq!(result.is_ok(), ok, "status {status} body {body}");
        if ok {
            assert_eq!(out, body.as_bytes());
        }
    }
    let server = TestServer::spawn(Vec::new());
    for ticket in [
        "https://evil.example/x",
        "/api/v1/attachments/x/content",
        "/api/v1/recording-downloads/a",
    ] {
        assert!(
            run(client(&server.base).download_recording_to(ticket, 0, &mut |_| Ok(()))).is_err()
        );
    }
    assert!(server.requests().is_empty());
}

#[gpui::test]
async fn recordings_retry_keeps_newer_event(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let stale = list_json(&[recording_json(ID, RECORDING_STARTED, 1)]);
    let server = TestServer::spawn(vec![Reply::new(200, &stale)]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            // Simulate the list being in flight when a newer WS event lands.
            let request = v.recordings.begin_refresh(1, v.recording_access_epoch, 0);
            v.recordings
                .apply_event(record(ID, RECORDING_READY, 9), 1_700_000_100);
            v.finish_recording_list(
                Ok(voxhold_api::RecordingList {
                    recordings: vec![record(ID, RECORDING_STARTED, 1)],
                    ..voxhold_api::RecordingList::default()
                }),
                request,
                window,
                cx,
            );
        });
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && calls(&server, "/recordings") >= 1
    });
    view.read_with(&*visual, |v, _| {
        let rows = v.recordings.sorted(1_700_000_100);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, RECORDING_READY);
        assert_eq!(rows[0].updated_at, 9, "the retry rolled the row back");
        assert!(v.recordings.live(7, 1_700_000_100).is_none());
    });
}

#[gpui::test]
async fn recordings_hide_metadata_while_access_is_unconfirmed(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let both = list_json(&[
        recording_json(&"1".repeat(32), RECORDING_READY, 1),
        recording_json(&"2".repeat(32), RECORDING_STARTED, 2),
    ]);
    let server = TestServer::spawn(vec![
        Reply::new(200, &both),
        Reply::new(503, "{\"error\":\"unavailable\"}"),
        Reply::new(503, "{\"error\":\"unavailable\"}"),
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.refresh_recordings(0, window, cx));
    });
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.recordings.sorted(1_700_000_100).len(), 2);
        assert!(v.recordings.live(7, 1_700_000_100).is_some());
    });

    // A reconnect can hide a revocation; the list fails, so unverified
    // metadata and both live markers must disappear.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.invalidate_recordings(window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && !v.recordings.error.is_empty()
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings.sorted(1_700_000_100).is_empty());
        assert!(v.recordings.live(7, 1_700_000_100).is_none());
    });

    // Same after channels.changed.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.invalidate_recordings(window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && calls(&server, "/recordings") >= 3
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings.sorted(1_700_000_100).is_empty());
        assert!(v.recordings.live(7, 1_700_000_100).is_none());
    });
}

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
async fn recording_preview_waits_for_access_confirmation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[recording_json(ID, RECORDING_READY, 1)]);
    let server = TestServer::spawn(vec![
        Reply::new(503, "{\"error\":\"unavailable\"}"),
        Reply::new(200, &ready),
        Reply {
            status: 200,
            body: ReplyBody::Bytes(jpeg(320, 180)),
            content_type: "image/jpeg",
            gate: None,
            delay: None,
        },
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    for _ in 0..5 {
        visual.update(|window, app| {
            window.draw(app).clear();
        });
    }
    assert_eq!(
        calls(&server, "/preview"),
        0,
        "a failed list started previews"
    );

    visual.update(|window, app| {
        view.update(app, |v, cx| v.refresh_recordings(0, window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        !v.recordings.loading && v.recordings.confirmed_epoch == Some(v.recordings.access_epoch)
    });
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    let key = format!("{ID}@etag-1234");
    wait_for(&mut visual, &view, |v| {
        v.recordings_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.previews.contains(&key))
    });
    assert_eq!(calls(&server, "/preview"), 1);
}

#[gpui::test]
async fn recording_preview_stops_when_dialog_closes(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[recording_json(ID, RECORDING_READY, 1)]);
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &ready),
        Reply {
            status: 200,
            body: ReplyBody::Bytes(jpeg(320, 180)),
            content_type: "image/jpeg",
            gate: Some(gate.clone()),
            delay: None,
        },
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    gate.arm();
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    wait_arrival(&mut visual, &gate);
    visual.update(|window, app| {
        view.update(app, |v, cx| v.close_recordings(true, window, cx));
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings_dialog.is_none());
        assert!(
            v.recordings_preview_tasks.is_empty(),
            "closing did not cancel the preview task"
        );
    });
    gate.release();
    visual.run_until_parked();
    std::thread::sleep(Duration::from_millis(50));
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert!(
            v.recordings_dialog.is_none(),
            "a late answer reopened the dialog"
        );
    });
    assert_eq!(calls(&server, "/preview"), 1);
}

#[gpui::test]
async fn ready_with_empty_preview_version_keeps_geometry_and_requests_zero(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let mut row = recording_json(ID, RECORDING_READY, 1);
    row["preview_version"] = serde_json::Value::Null;
    let ready = list_json(&[row]);
    let server = TestServer::spawn(vec![Reply::new(200, &ready)]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    view.read_with(&*visual, |v, _| {
        let dialog = v.recordings_dialog.as_ref().unwrap();
        assert_eq!(dialog.rows.len(), 1);
        assert!(dialog.rows[0].preview.w >= 132.);
        assert!(dialog.rows[0].ready);
        assert!(dialog.rows[0].download.w > 0.);
        assert!(dialog.previews.is_empty());
    });
    assert_eq!(calls(&server, "/preview"), 0);
}

#[gpui::test]
async fn recording_delete_then_ws_then_404_clears_the_row(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let ready = list_json(&[{
        let mut row = recording_json(ID, RECORDING_READY, 1);
        row["preview_version"] = serde_json::Value::Null;
        row
    }]);
    let tombstone = serde_json::to_string(&serde_json::json!({
        "id": ID,
        "server_id": 1,
        "channel_id": 7,
        "owner_user_id": 1,
        "owner_username": "owner",
        "status": RECORDING_DELETED,
        "updated_at": 2,
        "expires_at": 1_800_000_000,
    }))
    .unwrap();
    let server = TestServer::spawn(vec![
        Reply::new(200, &ready),
        Reply::new(200, &tombstone),
        Reply::new(404, "{\"error\":\"not found\"}"),
    ]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    let row = view.read_with(&*visual, |v, _| {
        v.recordings
            .sorted(1_700_000_100)
            .into_iter()
            .next()
            .unwrap()
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.delete_recording(row, window, cx));
    });
    wait_for(&mut visual, &view, |v| v.recordings.records.is_empty());

    // The same tombstone arriving over the shared WS must not resurrect it,
    // and a late GET 404 leaves the snapshot empty.
    visual.update(|_, app| {
        view.update(app, |v, _| {
            let event = recording_json(ID, RECORDING_DELETED, 3);
            assert!(
                v.apply_realtime_event("recording.updated", &event)
                    .recording_event
            );
            assert!(v.recordings.tombstones.contains(ID));
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.refresh_recordings(0, window, cx));
    });
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    view.read_with(&*visual, |v, _| {
        assert!(v.recordings.sorted(1_700_000_100).is_empty());
        assert!(v.recordings.records.is_empty());
    });
}

/// The plan's large-file scenario: a 24 MiB recording that takes longer than
/// the shared eight-second JSON timeout, saved through the same atomic `.part`
/// path as attachments.
#[test]
fn recording_24_mib_download_outlives_json_timeout_and_saves_atomically() {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-recording-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let size = 24 << 20;
    let source = dir.join("source.mp4");
    let mut bytes = vec![0u8; size];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    std::fs::write(&source, &bytes).unwrap();
    let server = TestServer::spawn(vec![
        Reply::file(200, source.clone(), "video/mp4").with_delay(Duration::from_secs(9))
    ]);
    let target = dir.join("saved.mp4");
    let started = Instant::now();
    let written = run(async {
        let client = client(&server.base);
        let mut save = crate::file_io::PartFile::create(&target).unwrap();
        let downloaded = client
            .download_recording_to(
                "/api/v1/recording-downloads/YWJj.ZGVm",
                size as i64,
                &mut |chunk| save.write(chunk),
            )
            .await
            .unwrap();
        save.finish(Some(size as u64)).unwrap();
        downloaded
    });
    assert_eq!(written, size as u64);
    assert!(
        started.elapsed() > Duration::from_secs(8),
        "the transfer must outlive the JSON timeout"
    );
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    let parts = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
        .count();
    assert_eq!(parts, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn recordings_render_at_reference_frames(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let both = list_json(&[
        recording_json(&"1".repeat(32), RECORDING_READY, 1),
        recording_json(&"2".repeat(32), RECORDING_STARTED, 2),
    ]);
    let server = TestServer::spawn(vec![Reply::new(200, &both)]);
    let (window, view) = make_app(cx, client(&server.base));
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.toggle_recordings("header", window, cx));
    });
    wait_for(&mut visual, &view, |v| !v.recordings.loading);
    for (width, height) in [(1440., 900.), (900., 720.), (390., 760.)] {
        visual.simulate_resize(gpui::size(gpui::px(width), gpui::px(height)));
        visual.update(|window, app| {
            window.draw(app).clear();
        });
        visual.run_until_parked();
        view.read_with(&*visual, |v, _| {
            assert!(
                (v.viewport.0 - width).abs() < 1. && (v.viewport.1 - height).abs() < 1.,
                "viewport {:?} at {width}x{height}",
                v.viewport
            );
            let dialog = v.recordings_dialog.as_ref().unwrap();
            assert!(dialog.bounds.w <= 570.);
            assert!(dialog.bounds.w <= width - 8.);
            assert!(dialog.bounds.x >= 0.);
            assert!(dialog.bounds.bottom() <= height);
            assert!(dialog.list.h >= 60.);
            assert_eq!(dialog.rows.len(), 2);
            if width == 390. {
                assert!(
                    crate::recordings_dialog::stacked(
                        dialog.bounds.w - 2. * crate::recordings::CARD_PADDING
                    ),
                    "the narrow frame stacks the cards"
                );
            }
        });
    }
}

#[test]
fn recordings_layout_covers_reference_frames() {
    let ready = record(ID, RECORDING_READY, 1);
    let inner = |width: f32| width - 50.;
    for (width, height) in [(1440., 900.), (900., 720.), (390., 760.)] {
        let heights: Vec<f32> = (0..6)
            .map(|_| {
                crate::recordings_dialog::row_height(&ready, inner(570.), 1_700_000_100, false)
            })
            .collect();
        let geometry = crate::recordings_dialog::dialog_geometry(width, height, 2, &heights);
        assert!(geometry.bounds.w <= 570.);
        assert!(geometry.bounds.x >= 0.);
        assert!(geometry.bounds.bottom() <= height);
        assert!(geometry.list.h >= 60.);
        assert!(geometry.max_scroll >= 0.);
    }
}
