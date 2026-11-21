//! Media cache, thumbnail and viewer tests. They live in a submodule so the
//! main GPUI test file stays inside the repository line limit.

use super::add_auth_window;
use super::*;
use std::time::Duration;

pub(super) fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba([200, 30, 30, 255]),
    ))
    .write_to(&mut out, image::ImageFormat::Png)
    .unwrap();
    out.into_inner()
}

pub(super) fn image_attachment() -> voxhold_api::Attachment {
    voxhold_api::Attachment {
        id: "a".repeat(32),
        filename: "photo.png".into(),
        mime_type: "image/png".into(),
        size_bytes: 1024,
        width: 400,
        height: 300,
        deleted: false,
    }
}

#[gpui::test]
async fn thumbnails_and_the_viewer_use_the_bounded_media_state(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = crate::test_support::TestServer::spawn(vec![
        crate::test_support::Reply::image(200, png_bytes(64, 48)),
        crate::test_support::Reply::image(200, png_bytes(120, 90)),
    ]);
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 7;
            v.viewport = (1280., 900.);
        });
    });
    let key = crate::media::ThumbKey::new(&image_attachment().id, 112., 64.);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.request_thumbnail(key.clone(), window, cx);
        });
    });
    let mut cached = false;
    for _ in 0..400 {
        visual.run_until_parked();
        cached = view.read_with(&*visual, |v, _| v.media.thumbs.contains(&key));
        if cached {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(cached, "the thumbnail lands in the bounded cache");
    let used = view.read_with(&*visual, |v, _| v.media.thumbs.used_bytes());
    assert!(used > 0 && used <= crate::media::THUMB_BUDGET);

    let file = image_attachment();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.open_viewer(5, &file, window, cx));
    });
    let mut opened = false;
    for _ in 0..400 {
        visual.run_until_parked();
        opened = view.read_with(&*visual, |v, _| {
            v.media
                .viewer
                .as_ref()
                .is_some_and(|viewer| viewer.image.is_some() && !viewer.loading)
        });
        if opened {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(opened, "the picture opens after the content answer");
    view.read_with(&*visual, |v, _| {
        let viewer = v.media.viewer.as_ref().unwrap();
        assert_eq!(viewer.render_box, (120, 90));
        assert!(viewer.source.is_some());
    });
    visual.update(|window, app| {
        view.update(app, |v, _| v.close_viewer(window));
    });
    assert!(view.read_with(&*visual, |v, _| v.media.viewer.is_none()));
}

#[gpui::test]
async fn a_closed_viewer_never_reopens_from_a_late_answer(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = crate::test_support::TestServer::spawn(vec![crate::test_support::Reply::image(
        200,
        png_bytes(40, 30),
    )]);
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 7;
        });
    });
    let file = image_attachment();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.open_viewer(5, &file, window, cx);
            v.close_viewer(window);
        });
    });
    // Let any late answer arrive; the request id guard keeps it closed.
    for _ in 0..40 {
        visual.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(view.read_with(&*visual, |v, _| v.media.viewer.is_none()));
}

#[gpui::test]
async fn a_deleted_attachment_releases_its_thumbnail_and_viewer(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let id = "b".repeat(32);
    let key = crate::media::ThumbKey::new(&id, 16., 16.);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 7;
            let frame = image::Frame::new(image::RgbaImage::new(16, 16));
            let render = std::sync::Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
            v.media.thumbs.put(key.clone(), render, 1024);
            v.media.viewer = Some(crate::media::Viewer::new(crate::media::ViewerKey {
                chat: v.chat_key(),
                access: v.access_generation,
                message_id: 5,
                attachment_id: id.clone(),
                open_id: 1,
            }));
            v.purge_attachments(&[id.clone()], window, cx);
            assert!(!v.media.thumbs.contains(&key));
            assert!(v.media.is_purged(&id));
            assert!(v.media.viewer.is_none());
            cx.notify();
        });
    });
}

#[gpui::test]
async fn a_deleted_attachment_cannot_be_reopened_before_history(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = crate::test_support::TestServer::spawn(vec![]);
    let id = "c".repeat(32);
    let stale = voxhold_api::Attachment {
        id: id.clone(),
        filename: "photo.png".into(),
        mime_type: "image/png".into(),
        size_bytes: 64,
        width: 10,
        height: 10,
        deleted: false,
    };
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 7;
            v.client = Some(Client::new(&server.base).unwrap());
            let mut dto = message_json(1, 1, "файл");
            dto["attachments"] = serde_json::json!([{
                "id": id,
                "filename": "photo.png",
                "mime_type": "image/png",
                "size_bytes": 64,
                "width": 10,
                "height": 10,
                "deleted": false
            }]);
            v.merge_message(true, serde_json::from_value::<Message>(dto).unwrap());
            let effect = v.apply_realtime_event(
                "attachments.changed",
                &serde_json::json!({"channel_id": 7, "attachment_ids": [id]}),
            );
            assert_eq!(effect.attachments.len(), 1);
            assert!(v.messages[0].attachments[0].deleted);
            // A click with the stale row data must not start content GET.
            v.open_viewer(1, &stale, window, cx);
            assert!(v.media.viewer.is_none());
            assert!(server.requests().is_empty());
            // An older page still claims deleted = false; the tombstone wins.
            let mut old = message_json(1, 2, "файл");
            old["attachments"] = serde_json::json!([{
                "id": id,
                "filename": "photo.png",
                "mime_type": "image/png",
                "size_bytes": 64,
                "width": 10,
                "height": 10,
                "deleted": false
            }]);
            v.apply_history_snapshot(
                voxhold_api::MessagePage {
                    messages: vec![serde_json::from_value::<Message>(old).unwrap()],
                    pagination: voxhold_api::Pagination::default(),
                },
                Default::default(),
            );
            assert!(v.messages[0].attachments[0].deleted);
            assert!(v.media.viewer.is_none());
            cx.notify();
        });
    });
    assert!(server.requests().is_empty());
}

#[gpui::test]
async fn rolling_the_purged_set_does_not_resurrect_a_deleted_attachment(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            let id = "e".repeat(32);
            let row = |revision: i64, deleted: bool| {
                let mut dto = message_json(1, revision, "файл");
                dto["attachments"] = serde_json::json!([{
                    "id": id.clone(),
                    "filename": "photo.png",
                    "mime_type": "image/png",
                    "size_bytes": 64,
                    "width": 10,
                    "height": 10,
                    "deleted": deleted
                }]);
                serde_json::from_value::<Message>(dto).unwrap()
            };
            v.merge_message(true, row(1, false));
            let mut ids = vec![id.clone()];
            ids.extend((1..=crate::media::PURGED_MARKERS).map(|index| format!("{index:032x}")));
            let effect = v.apply_realtime_event(
                "attachments.changed",
                &serde_json::json!({"channel_id": 7, "attachment_ids": ids}),
            );
            assert_eq!(effect.attachments.len(), crate::media::PURGED_MARKERS + 1);
            assert!(v.messages[0].attachments[0].deleted);
            assert!(
                !v.media.is_purged(&id),
                "the oldest marker rolled out of the bounded set"
            );
            assert!(v.media.purged.len() <= crate::media::PURGED_MARKERS);

            // The late page still carries `deleted: false`; the row's own
            // tombstone must win even after its id left `purged`.
            v.apply_history_snapshot(
                voxhold_api::MessagePage {
                    messages: vec![row(2, false)],
                    pagination: voxhold_api::Pagination::default(),
                },
                Default::default(),
            );
            assert!(
                v.messages[0].attachments[0].deleted,
                "a late row cannot resurrect an attachment this row already dropped"
            );
        });
    });
}

#[gpui::test]
async fn a_late_context_answer_cannot_resurrect_an_evicted_attachment(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.generation = 3;
            v.access_generation = 4;
            v.before = Some(10);
            assert!(
                v.messages.is_empty(),
                "the /context target is not in the loaded feed"
            );
            // A /context, a refresh and an older-page request are in flight.
            v.context_request = Some(7);
            v.refresh_request = Some(8);
            v.older_request = 9;
            v.loading_older = true;
            assert!(v.accepts_context_reply(7, 3, 4, 1, 7));
            assert!(v.accepts_older_reply(9, 10, 1, 7, 3, 4));

            let id = "f".repeat(32);
            let mut ids = vec![id.clone()];
            ids.extend((1..=crate::media::PURGED_MARKERS).map(|index| format!("{index:032x}")));
            let effect = v.apply_realtime_event(
                "attachments.changed",
                &serde_json::json!({"channel_id": 7, "attachment_ids": ids}),
            );
            assert_eq!(effect.attachments.len(), crate::media::PURGED_MARKERS + 1);
            assert!(
                !v.media.is_purged(&id),
                "the first attachment marker rolled out of the bounded set"
            );

            assert!(v.context_request.is_none());
            assert!(v.refresh_request.is_none());
            assert_eq!(v.older_request, 0);
            assert!(!v.loading_older);
            assert!(
                !v.accepts_context_reply(7, 3, 4, 1, 7),
                "the /context answer requested before the deletion is stale"
            );
            assert!(
                !v.accepts_older_reply(9, 10, 1, 7, 3, 4),
                "the history answer requested before the deletion is stale"
            );

            // Run the callback's guard exactly as the background task would.
            let mut row = message_json(50, 1, "старое сообщение");
            row["attachments"] = serde_json::json!([{
                "id": id,
                "filename": "old.png",
                "mime_type": "image/png",
                "size_bytes": 64,
                "width": 10,
                "height": 10,
                "deleted": false
            }]);
            let context: voxhold_api::MessageContext = serde_json::from_value(serde_json::json!({
                "messages": [row],
                "target_message_id": 50,
                "target_index": 0
            }))
            .unwrap();
            if v.accepts_context_reply(7, 3, 4, 1, 7) {
                v.apply_context_snapshot(context);
            }
            assert!(
                v.messages.is_empty(),
                "the late context row must not return the evicted attachment"
            );

            // Requests started after the deletion are not affected.
            v.context_request = Some(10);
            assert!(v.accepts_context_reply(10, 3, 4, 1, 7));
        });
    });
}

#[gpui::test]
async fn an_evicted_marker_restarts_a_cancelled_initial_history_load(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let row = message_json(1, 1, "свежая строка");
    let page = serde_json::json!({
        "messages": [row],
        "pagination": {"next_before_id": null, "has_more": false}
    })
    .to_string();
    let server =
        crate::test_support::TestServer::spawn(vec![crate::test_support::Reply::new(200, &page)]);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_workspace(v);
            v.client = Some(Client::new(&server.base).unwrap());
            v.generation = 3;
            v.access_generation = 4;
            assert!(v.messages.is_empty());
            // The first history load is already in flight.
            v.request_seq = 8;
            v.refresh_request = Some(8);

            let id = "a".repeat(32);
            let mut ids = vec![id];
            ids.extend((1..=crate::media::PURGED_MARKERS).map(|index| format!("{index:032x}")));
            let effect = v.apply_realtime_event(
                "attachments.changed",
                &serde_json::json!({"channel_id": 7, "attachment_ids": ids}),
            );
            assert!(effect.restart_history);
            assert!(v.refresh_request.is_none());

            // This is the event pump's post-batch action.
            if effect.restart_history {
                v.refresh_history(window, cx);
            }
            assert!(v.refresh_request.is_some(), "a fresh refresh started");
        });
    });

    let mut loaded = false;
    for _ in 0..200 {
        visual.run_until_parked();
        loaded = view.read_with(&*visual, |v, _| !v.messages.is_empty());
        if loaded {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(loaded, "the replacement history page is applied");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.messages.len(), 1);
        assert_eq!(v.messages[0].content, "свежая строка");
        assert!(v.refresh_request.is_none());
    });
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        1,
        "exactly one fresh history request: {requests:?}"
    );
    assert!(requests[0].path.contains("/messages"));
}

fn preview_json(url: &str, title: &str) -> String {
    format!(
        "{{\"url\":\"{url}\",\"title\":\"{title}\",\"description\":\"Описание\",\"site_name\":\"Example\",\"image_path\":\"\"}}"
    )
}

#[gpui::test]
async fn an_expired_off_screen_preview_does_not_loop_the_redraw(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_workspace(v);
            v.media.put_preview(
                "https://offscreen.example.com/",
                crate::media::PreviewEntry {
                    retry_at: Some(std::time::Instant::now() - Duration::from_secs(1)),
                    failed: true,
                    ..Default::default()
                },
            );
            cx.notify();
        });
        window.draw(app).clear();
    });
    visual.run_until_parked();
    // The entry is still cached, but no link card is visible: the render must
    // not schedule a timer at zero delay for it.
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    view.read_with(&*visual, |v, _| {
        assert!(v
            .media
            .preview_failures
            .contains_key("https://offscreen.example.com/"));
        assert!(
            v.preview_wake_at.is_none(),
            "an off-screen expired gate must not wake every frame"
        );
    });
}

#[gpui::test]
async fn a_transient_preview_failure_retries_from_the_timer(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let url = "https://example.com/article";
    let server = crate::test_support::TestServer::spawn(vec![
        crate::test_support::Reply::new(500, "{\"error\":\"temporarily unavailable\"}"),
        crate::test_support::Reply::new(200, &preview_json(url, "Заголовок")),
    ]);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_workspace(v);
            v.client = Some(Client::new(&server.base).unwrap());
            v.apply_realtime_event(
                "message.created",
                &message_json(1, 1, "смотри https://example.com/article"),
            );
            cx.notify();
        });
        window.draw(app).clear();
    });
    let mut failed = false;
    for _ in 0..300 {
        visual.run_until_parked();
        failed = view.read_with(&*visual, |v, _| {
            v.media
                .preview_failures
                .contains_key("https://example.com/article")
        });
        if failed {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(failed, "the first answer failed and was gated");
    assert!(
        view.read_with(&*visual, |v, _| v.preview_wake_at.is_some()),
        "a retry wakeup is scheduled"
    );

    // Let the real 5 s gate expire, then fire the timer and the next frame.
    std::thread::sleep(Duration::from_secs(5) + Duration::from_millis(100));
    visual.executor().advance_clock(Duration::from_secs(6));
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    let mut succeeded = false;
    for _ in 0..300 {
        visual.run_until_parked();
        succeeded = view.read_with(&*visual, |v, _| {
            v.media
                .preview_entry("https://example.com/article")
                .is_some()
        });
        if succeeded {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(succeeded, "the timer retried the preview without input");
    let requests: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|request| request.path.contains("link-preview"))
        .collect();
    assert_eq!(requests.len(), 2, "one failure and one retry");
    view.read_with(&*visual, |v, _| {
        let entry = v.media.preview_entry(url).expect("preview");
        assert_eq!(entry.meta.title, "Заголовок");
    });
}

#[gpui::test]
async fn a_permanent_preview_failure_is_not_requested_again(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = crate::test_support::TestServer::spawn(vec![crate::test_support::Reply::new(
        400,
        "{\"error\":\"public HTTPS URL required\"}",
    )]);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_workspace(v);
            v.client = Some(Client::new(&server.base).unwrap());
            v.apply_realtime_event(
                "message.created",
                &message_json(1, 1, "смотри https://example.com/article"),
            );
            cx.notify();
        });
        window.draw(app).clear();
    });
    let mut failed = false;
    for _ in 0..300 {
        visual.run_until_parked();
        failed = view.read_with(&*visual, |v, _| {
            v.media
                .previews
                .get("https://example.com/article")
                .is_some_and(|entry| entry.failed)
        });
        if failed {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(failed);
    assert!(view.read_with(&*visual, |v, _| v.preview_wake_at.is_none()));
    visual.executor().advance_clock(Duration::from_secs(60));
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    visual.run_until_parked();
    let previews = server
        .requests()
        .into_iter()
        .filter(|request| request.path.contains("link-preview"))
        .count();
    assert_eq!(previews, 1, "400 is permanent");
}

#[gpui::test]
async fn access_change_releases_cached_media(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new("http://127.0.0.1:1").unwrap());
            v.server = 1;
            v.channel = 7;
            let frame = image::Frame::new(image::RgbaImage::new(16, 16));
            let render = std::sync::Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
            v.media
                .thumbs
                .put(crate::media::ThumbKey::new("a", 16., 16.), render, 1024);
            v.media.put_preview(
                "https://example.com/",
                crate::media::PreviewEntry::default(),
            );
            assert!(v.media.thumbs.used_bytes() > 0);
            v.invalidate_access(window, cx);
            assert_eq!(v.media.thumbs.used_bytes(), 0);
            assert!(v.media.previews.is_empty());
            assert!(v.media.viewer.is_none());
        });
    });
}

#[test]
fn thumbnail_frame_scales_the_borrowed_original_without_a_full_size_copy() {
    use crate::app::media_ops::{scale_probe, thumbnail_frame};
    use image::GenericImageView;

    // 4000×3000 is large enough that a source-sized RGBA copy (48 MB,
    // 12 M pixels) is unmistakably bigger than the 120×90 target, so the
    // probe's `oversized` flag catches one if the pass ever allocates it.
    //
    // Limitation: the probe only sees the buffers this module reports. A copy
    // made inside `image` without passing through `note_scale_buffer` is not
    // observable without a global allocator. The structural guarantee that
    // remains is the signature: `thumbnail_frame` takes `&DynamicImage` and
    // the viewer path never clones the original (it is shared as an `Arc`).
    let source = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        4000,
        3000,
        image::Rgba([10, 200, 30, 255]),
    ));
    scale_probe::reset();
    let (frame, bytes) = thumbnail_frame(&source, 120, 90);
    assert_eq!(frame.buffer().dimensions(), (120, 90));
    assert_eq!(bytes, 120 * 90 * 4);
    // The source is read in place at its real size...
    assert_eq!(scale_probe::source_pixels(), 4000 * 3000);
    // ...while every allocated buffer stays at the target.
    assert_eq!(
        scale_probe::peak_pixels(),
        120 * 90,
        "the pass only materialises target-sized buffers"
    );
    assert!(
        !scale_probe::oversized(),
        "an allocated buffer larger than the target would be a source-sized copy"
    );
    // The shared original is untouched and still usable after the pass.
    assert_eq!(source.dimensions(), (4000, 3000));
    assert_eq!(source.get_pixel(0, 0).0, [10, 200, 30, 255]);
    // The frame is BGRA: green stays in the middle, red and blue swap.
    assert_eq!(frame.buffer().get_pixel(60, 45).0, [30, 200, 10, 255]);
}

#[test]
fn scaled_frames_keep_proportions_and_channels_on_odd_and_tiny_sources() {
    use crate::app::media_ops::{scale_probe, thumbnail_frame};

    let wide = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        40,
        10,
        image::Rgb([250, 5, 120]),
    ));
    scale_probe::reset();
    let (frame, bytes) = thumbnail_frame(&wide, 8, 2);
    assert_eq!(frame.buffer().dimensions(), (8, 2));
    assert_eq!(bytes, 8 * 2 * 4);
    assert_eq!(frame.buffer().get_pixel(4, 1).0, [120, 5, 250, 255]);
    assert_eq!(scale_probe::peak_pixels(), 8 * 2);
    assert!(!scale_probe::oversized());

    // A two-pixel source still yields at least a 1×1 surface, and a zero box
    // is clamped instead of producing an empty texture.
    let tiny = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        3,
        image::Rgba([1, 2, 3, 200]),
    ));
    let (frame, bytes) = thumbnail_frame(&tiny, 0, 0);
    assert_eq!(frame.buffer().dimensions(), (1, 1));
    assert_eq!(bytes, 4);
    assert_eq!(frame.buffer().get_pixel(0, 0).0, [3, 2, 1, 200]);
}

#[test]
fn sixteen_bit_sources_keep_size_channels_and_bgra_order() {
    use crate::app::media_ops::{scale_probe, thumbnail_frame};

    // The resample keeps the 16-bit channel layout and only the final,
    // target-sized buffer is converted to 8-bit RGBA. A 16-bit surface has two
    // bytes per channel, so its intermediate buffer is larger in bytes than
    // the 8-bit result while still having exactly the target pixel count.
    // Values are u16::MAX/0, which map to 255/0 without rounding, so the BGRA
    // assertion is exact; a mid-range value could differ by one between the
    // old and new paths and is deliberately avoided here.
    let rgb = image::DynamicImage::ImageRgb16(image::ImageBuffer::from_pixel(
        40,
        10,
        image::Rgb([u16::MAX, 0, 0]),
    ));
    scale_probe::reset();
    let (frame, bytes) = thumbnail_frame(&rgb, 8, 2);
    assert_eq!(frame.buffer().dimensions(), (8, 2));
    assert_eq!(bytes, 8 * 2 * 4);
    assert_eq!(frame.buffer().get_pixel(4, 1).0, [0, 0, 255, 255]);
    assert_eq!(scale_probe::source_pixels(), 40 * 10);
    assert_eq!(scale_probe::peak_pixels(), 8 * 2);
    assert!(!scale_probe::oversized());

    let rgba = image::DynamicImage::ImageRgba16(image::ImageBuffer::from_pixel(
        40,
        10,
        image::Rgba([u16::MAX, 0, 0, u16::MAX]),
    ));
    scale_probe::reset();
    let (frame, bytes) = thumbnail_frame(&rgba, 8, 2);
    assert_eq!(frame.buffer().dimensions(), (8, 2));
    assert_eq!(bytes, 8 * 2 * 4);
    assert_eq!(frame.buffer().get_pixel(0, 0).0, [0, 0, 255, 255]);
    assert!(!scale_probe::oversized());
}

#[gpui::test]
async fn dragging_the_window_keeps_one_rescale_pass_and_lands_on_the_last_box(
    cx: &mut TestAppContext,
) {
    use crate::app::media_ops::scale_probe;

    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = crate::test_support::TestServer::spawn(vec![crate::test_support::Reply::image(
        200,
        png_bytes(1600, 1200),
    )]);
    // The frame loop re-asserts the real window viewport, so the drag ends by
    // releasing the window at its own size.
    let real = visual.update(|window, _| {
        let size = window.viewport_size();
        let width: f32 = size.width.into();
        let height: f32 = size.height.into();
        (width, height)
    });
    let initial = crate::media::fit_size((1600, 1200), crate::media::viewer_area(real));
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 7;
            v.viewport = real;
        });
    });
    let file = image_attachment();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.open_viewer(5, &file, window, cx));
    });
    let mut opened = false;
    for _ in 0..400 {
        visual.run_until_parked();
        opened = view.read_with(&*visual, |v, _| {
            v.media
                .viewer
                .as_ref()
                .is_some_and(|viewer| viewer.image.is_some() && !viewer.loading)
        });
        if opened {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(opened, "the picture opens before the drag");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.media.viewer.as_ref().unwrap().render_box, initial);
    });

    scale_probe::reset();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            for viewport in [(1000., 700.), (900., 650.), (800., 600.), (700., 550.)] {
                v.viewport = viewport;
                v.ensure_viewer_scale(window, cx);
            }
            let viewer = v.media.viewer.as_ref().unwrap();
            assert!(viewer.rescaling, "the first pass is still running");
            assert_eq!(
                scale_probe::passes(),
                1,
                "a drag queues no extra pass behind the running one"
            );
            assert_eq!(viewer.target_box, (560, 420), "the newest box is kept");
            // Releasing the window at its own size is one more request, still
            // without a second background pass.
            v.viewport = real;
            v.ensure_viewer_scale(window, cx);
            assert_eq!(scale_probe::passes(), 1);
            assert_eq!(v.media.viewer.as_ref().unwrap().target_box, initial);
        });
    });

    let mut settled = false;
    for _ in 0..400 {
        visual.run_until_parked();
        settled = view.read_with(&*visual, |v, _| {
            v.media
                .viewer
                .as_ref()
                .is_some_and(|viewer| !viewer.rescaling && viewer.render_box == initial)
        });
        if settled {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(settled, "the viewer lands on the last requested geometry");
    view.read_with(&*visual, |v, _| {
        let viewer = v.media.viewer.as_ref().unwrap();
        assert_eq!(viewer.render_box, initial);
        assert!(viewer.image.is_some());
        assert_eq!(
            scale_probe::passes(),
            2,
            "exactly one follow-up pass for the last geometry"
        );
    });
    visual.update(|window, app| {
        view.update(app, |v, _| v.close_viewer(window));
    });
}
