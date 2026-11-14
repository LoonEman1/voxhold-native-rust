//! Media cache, thumbnail and viewer tests. They live in a submodule so the
//! main GPUI test file stays inside the repository line limit.

use super::add_auth_window;
use super::*;

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
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

fn image_attachment() -> voxhold_api::Attachment {
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
