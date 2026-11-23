//! Viewer lifecycle tests: closing releases the GPU surface through
//! `Window::drop_image` and cancels a running rescale pass, so a reopened
//! viewer never inherits the previous pass.

use super::media::{image_attachment, png_bytes};
use super::*;
use std::time::Duration;

#[gpui::test]
async fn closing_the_viewer_releases_its_surface_and_decoded_source(cx: &mut TestAppContext) {
    use crate::app::media_ops::image_probe;

    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let mut source = None;
    let mut surface = None;
    image_probe::reset();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 7;
            let decoded = std::sync::Arc::new(image::DynamicImage::ImageRgba8(
                image::RgbaImage::new(64, 48),
            ));
            let frame = image::Frame::new(image::RgbaImage::new(64, 48));
            let render = std::sync::Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
            source = Some(std::sync::Arc::downgrade(&decoded));
            surface = Some(std::sync::Arc::downgrade(&render));
            v.media.viewer = Some(crate::media::Viewer::new(crate::media::ViewerKey {
                chat: v.chat_key(),
                access: v.access_generation,
                message_id: 5,
                attachment_id: "d".repeat(32),
                open_id: 1,
            }));
            let viewer = v.media.viewer.as_mut().unwrap();
            viewer.loading = false;
            viewer.source = Some(decoded);
            viewer.image = Some(render);
            viewer.render_box = (64, 48);
            viewer.target_box = (64, 48);
            v.close_viewer(window);
            assert!(v.media.viewer.is_none());
            cx.notify();
        });
    });
    assert!(
        source.expect("source").upgrade().is_none(),
        "the decoded original is released on close"
    );
    assert!(
        surface.expect("surface").upgrade().is_none(),
        "the GPU surface is released on close"
    );
    assert_eq!(
        image_probe::drops(),
        1,
        "close must call Window::drop_image, not only drop the Arc"
    );
}

#[gpui::test]
async fn closing_the_viewer_stops_a_running_scale_pass(cx: &mut TestAppContext) {
    use crate::app::media_ops::{image_probe, scale_probe};

    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    // The frame loop re-asserts the real window viewport, so the final
    // geometry after the reopen is the fit for this size.
    let real = visual.update(|window, _| {
        let size = window.viewport_size();
        let width: f32 = size.width.into();
        let height: f32 = size.height.into();
        (width, height)
    });
    let expected_final = crate::media::fit_size((320, 240), crate::media::viewer_area(real));
    let server = crate::test_support::TestServer::spawn(vec![
        crate::test_support::Reply::image(200, png_bytes(1600, 1200)),
        crate::test_support::Reply::image(200, png_bytes(320, 240)),
    ]);
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 7;
            v.viewport = real;
        });
    });
    let first = image_attachment();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.open_viewer(5, &first, window, cx));
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
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(opened, "the first picture opens");
    let old_source = view.read_with(&*visual, |v, _| {
        std::sync::Arc::downgrade(v.media.viewer.as_ref().unwrap().source.as_ref().unwrap())
    });

    // Start a rescale and close in the same turn of the event loop, before the
    // dispatcher can run the background pass. Dropping the stored handle has
    // to cancel it right there.
    image_probe::reset();
    scale_probe::reset();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.viewport = (700., 550.);
            v.ensure_viewer_scale(window, cx);
            assert!(v.media.viewer.as_ref().unwrap().rescaling);
            assert_eq!(scale_probe::passes(), 1, "the pass started");
            v.close_viewer(window);
            assert!(v.media.viewer.is_none(), "close drops the scale handle");
        });
    });
    assert_eq!(image_probe::drops(), 1, "the closed surface is released");
    visual.run_until_parked();
    assert!(
        old_source.upgrade().is_none(),
        "the cancelled pass no longer holds the decoded original"
    );
    assert_eq!(
        scale_probe::installed(),
        0,
        "the cancelled pass never reaches the UI half"
    );

    // A second attachment gets a fresh viewer state; the cancelled pass must
    // not leave `rescaling` set or start another pass in it.
    let second = voxhold_api::Attachment {
        id: "b".repeat(32),
        ..image_attachment()
    };
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.viewport = real;
            v.open_viewer(6, &second, window, cx);
        });
    });
    let mut reopened = false;
    for _ in 0..400 {
        visual.run_until_parked();
        reopened = view.read_with(&*visual, |v, _| {
            v.media
                .viewer
                .as_ref()
                .is_some_and(|viewer| viewer.image.is_some() && !viewer.loading)
        });
        if reopened {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(reopened, "the second picture opens");
    view.read_with(&*visual, |v, _| {
        let viewer = v.media.viewer.as_ref().unwrap();
        assert_eq!(viewer.key.message_id, 6);
        assert!(!viewer.rescaling, "a new viewer starts without a pass");
        assert_eq!(scale_probe::passes(), 1, "no stale follow-up started");
        assert_eq!(scale_probe::installed(), 0);
    });

    // The new viewer can still rescale normally. A follow-up for the real
    // window size may be needed after the frame loop re-asserts its viewport.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.viewport = (200., 200.);
            v.ensure_viewer_scale(window, cx);
            assert!(v.media.viewer.as_ref().unwrap().rescaling);
            assert_eq!(scale_probe::passes(), 2);
        });
    });
    let mut settled = false;
    for _ in 0..400 {
        visual.run_until_parked();
        settled = view.read_with(&*visual, |v, _| {
            v.media
                .viewer
                .as_ref()
                .is_some_and(|viewer| !viewer.rescaling && viewer.render_box == expected_final)
        });
        if settled {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(settled, "the new viewer lands on the real window geometry");
    view.read_with(&*visual, |v, _| {
        let viewer = v.media.viewer.as_ref().unwrap();
        assert_eq!(viewer.key.message_id, 6);
        assert_eq!(viewer.render_box, expected_final);
        // Every pass started after the reopen reached the UI; the only pass
        // that did not was the one cancelled by the close.
        assert_eq!(
            scale_probe::passes() - 1,
            scale_probe::installed(),
            "every pass started after the reopen installed a frame"
        );
    });
}
