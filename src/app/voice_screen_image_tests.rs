//! Texture-release regressions for the screen preview, picker cache and
//! incoming player. The tests count calls that reached `Window::drop_image`
//! instead of inspecting a real DirectX atlas: no GPU is involved.

use super::voice_screen_tests::{draw, monitor};
use gpui::{RenderImage, TestAppContext};
use std::sync::Arc;
use voxhold_media::publisher::Sharing;
use voxhold_media::screen_capture::SourceKind;

/// One small GPUI picture with a real image id, so the slot can be checked
/// without a capture stack.
fn plate(width: u32, height: u32, color: [u8; 4]) -> Arc<RenderImage> {
    let buffer = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
    Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
        buffer
    )]))
}

/// A new preview frame replaces the slot first and releases the old picture
/// afterwards. The old id must be gone from the slot when the count moves.
#[gpui::test]
async fn a_replaced_screen_preview_frame_is_released(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let first = plate(2, 2, [0x00, 0x00, 0xff, 0xff]);
    let first_id = first.id.0;
    let second = plate(2, 2, [0xff, 0x00, 0x00, 0xff]);
    let second_id = second.id.0;
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_preview(window, first);
            assert_eq!(
                panel.test_screen_preview_image_id(),
                Some(first_id),
                "the first frame is installed"
            );
            assert_eq!(
                panel.test_screen_images_released(),
                0,
                "installing the first frame replaces nothing"
            );

            panel.test_set_screen_preview(window, second);
            assert_eq!(
                panel.test_screen_preview_image_id(),
                Some(second_id),
                "the slot points at the new frame before the old one is released"
            );
            assert_eq!(
                panel.test_screen_release_slot_image_id(),
                Some(second_id as u64),
                "the slot already held the replacement when the old frame was dropped"
            );
            assert_eq!(
                panel.test_screen_images_released(),
                1,
                "the replaced preview frame must be released from the atlas"
            );
        });
    });
}

/// Inserting a new thumbnail for a card that already has one replaces the
/// cached picture and releases the previous texture.
#[gpui::test]
async fn a_replaced_thumbnail_is_released(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![source.clone()],
                None,
            );
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            assert_eq!(panel.test_screen_images_released(), 0);

            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            assert_eq!(
                panel.test_screen_preview_counts(),
                (1, 8 * 8 * 4),
                "the same card keeps one picture and one byte budget"
            );
            assert_eq!(
                panel.test_screen_images_released(),
                1,
                "the replaced thumbnail must reach Window::drop_image"
            );
        });
    });
}

/// Stop clears a non-empty picker cache and releases every cached thumbnail.
#[gpui::test]
async fn stop_releases_the_picker_thumbnail_cache(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    let other = monitor(5, "Экран B", 2560, 1440);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![source.clone(), other.clone()],
                None,
            );
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            assert!(panel.test_inject_screen_preview(window, &other, 8, 8));
            assert_eq!(panel.test_screen_preview_counts(), (2, 2 * 8 * 8 * 4));
            assert_eq!(panel.test_screen_images_released(), 0);

            panel.test_stop_screen_share(window, cx);
            assert_eq!(
                panel.test_screen_preview_counts(),
                (0, 0),
                "Stop empties the cache"
            );
            assert_eq!(
                panel.test_screen_images_released(),
                2,
                "both cached pictures reached Window::drop_image"
            );
        });
    });
}

/// Closing the picker releases the cached pictures as well.
#[gpui::test]
async fn closing_the_picker_releases_the_thumbnail_cache(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![source.clone()],
                None,
            );
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            panel.test_close_screen_share(window, cx);
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            assert_eq!(panel.test_screen_images_released(), 1);
        });
    });
}

/// A source disappearing from the snapshot drops its cached picture with a
/// release, so a tab switch or monitor unplug cannot keep the texture alive.
#[gpui::test]
async fn a_removed_source_preview_is_released(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let kept = monitor(3, "Экран A", 1920, 1080);
    let gone = monitor(5, "Экран B", 2560, 1440);
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![kept.clone(), gone.clone()],
                None,
            );
            assert!(panel.test_inject_screen_preview(window, &kept, 8, 8));
            assert!(panel.test_inject_screen_preview(window, &gone, 8, 8));
            assert_eq!(panel.test_screen_images_released(), 0);

            panel.test_apply_screen_sources(window, SourceKind::Monitor, vec![kept.clone()]);
            assert_eq!(panel.test_screen_preview_counts(), (1, 8 * 8 * 4));
            assert_eq!(
                panel.test_screen_images_released(),
                1,
                "the vanished source's picture is released"
            );
        });
    });
}

/// The incoming player holds one frame. Replacing it releases the previous
/// picture only after the new one is in the slot.
#[gpui::test]
async fn a_replaced_player_frame_is_released(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let first = plate(4, 4, [0x10, 0x20, 0x30, 0xff]);
    let first_id = first.id.0;
    let second = plate(4, 4, [0x30, 0x20, 0x10, 0xff]);
    let second_id = second.id.0;
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_player_frame(window, first);
            assert_eq!(panel.test_player_frame_image_id(), Some(first_id));
            assert_eq!(panel.test_player_images_released(), 0);

            panel.test_set_player_frame(window, second);
            assert_eq!(panel.test_player_frame_image_id(), Some(second_id));
            assert_eq!(
                panel.test_player_release_slot_image_id(),
                Some(second_id as u64),
                "the slot already held the replacement when the old frame was dropped"
            );
            assert_eq!(
                panel.test_player_images_released(),
                1,
                "the replaced player frame must reach Window::drop_image"
            );
        });
    });
}

/// Closing the player view releases its last frame.
#[gpui::test]
async fn leaving_the_player_releases_its_frame(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let frame = plate(4, 4, [0x40, 0x50, 0x60, 0xff]);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_player_frame(window, frame);
            panel.test_leave_stream(window, cx);
            assert_eq!(panel.test_player_frame_image_id(), None);
            assert_eq!(panel.test_player_images_released(), 1);
        });
    });
}

/// Leaving the call (or a lost session) resets the whole share state and
/// releases the on-air picture, every thumbnail and the player frame.
#[gpui::test]
async fn resetting_the_share_releases_every_held_image(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![source.clone()],
                None,
            );
            panel.test_set_screen_preview(window, plate(2, 2, [0x11, 0x22, 0x33, 0xff]));
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            panel.test_set_player_frame(window, plate(4, 4, [0x44, 0x55, 0x66, 0xff]));
            assert_eq!(panel.test_screen_images_released(), 0);
            assert_eq!(panel.test_player_images_released(), 0);
            panel.test_set_preview_capture_in_flight(true);

            panel.test_reset_screen(window);
            assert!(
                panel.test_preview_capture_in_flight(),
                "the next picker must wait for the capture that survived this reset"
            );
            assert_eq!(panel.test_screen_preview_image_id(), None);
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            assert_eq!(
                panel.test_screen_images_released(),
                2,
                "the on-air picture and the cached thumbnail are released"
            );

            // `VoiceUpdate::Ended` resets the share and then closes the player;
            // both paths must release what they held.
            panel.test_leave_stream(window, cx);
            assert_eq!(panel.test_player_frame_image_id(), None);
            assert_eq!(
                panel.test_player_images_released(),
                1,
                "the incoming player frame is released with the closed player"
            );
        });
    });
}

/// The real `VoiceUpdate::Ended` entry (lost call, reconnect ended) resets the
/// share and closes the player, releasing both kinds of held picture.
#[gpui::test]
async fn a_lost_call_releases_the_preview_and_player_images(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![source.clone()],
                None,
            );
            panel.test_set_screen_preview(window, plate(2, 2, [0x12, 0x23, 0x34, 0xff]));
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            panel.test_set_player_frame(window, plate(4, 4, [0x45, 0x56, 0x67, 0xff]));

            assert!(panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Ended(None),
                window,
                cx,
            ));
            assert_eq!(panel.test_screen_preview_image_id(), None);
            assert_eq!(panel.test_player_frame_image_id(), None);
            assert_eq!(panel.test_screen_images_released(), 2);
            assert_eq!(panel.test_player_images_released(), 1);
        });
    });
}

/// Stop releases the on-air picture that a frame already showed, so a stopped
/// stream cannot keep its texture until an Idle snapshot arrives.
#[gpui::test]
async fn stop_releases_the_shown_preview_card(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![source.clone()],
                None,
            );
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            panel.test_set_screen_preview(window, plate(2, 2, [0x01, 0x02, 0x03, 0xff]));
            assert_eq!(panel.test_screen_images_released(), 0);

            panel.test_stop_screen_share(window, cx);
            assert_eq!(
                panel.test_screen_preview_image_id(),
                None,
                "Stop drops the card that was already shown"
            );
            assert_eq!(
                panel.test_screen_images_released(),
                2,
                "Stop releases both the card and the cached thumbnail"
            );
        });
    });
}

/// A coalesced Idle snapshot releases a shown card even when the previous
/// snapshot was already Idle: Stop and the phase answer may arrive merged.
#[gpui::test]
async fn an_idle_update_releases_a_shown_preview_card(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            assert!(
                panel.test_apply_screen_sharing(window, Sharing::default()),
                "an Idle snapshot still repaints"
            );
            panel.test_set_screen_preview(window, plate(2, 2, [0x04, 0x05, 0x06, 0xff]));
            assert_eq!(panel.test_screen_images_released(), 0);

            assert!(panel.test_apply_screen_sharing(window, Sharing::default()));
            assert_eq!(
                panel.test_screen_preview_image_id(),
                None,
                "an Idle snapshot releases the card even after an Idle one"
            );
            assert_eq!(panel.test_screen_images_released(), 1);
        });
    });
}

/// Repeated Stop and reset calls are idempotent: each removed picture is
/// released exactly once, and a later preview gets a fresh image.
#[gpui::test]
async fn repeated_stop_and_reset_release_each_picture_once(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let first = plate(2, 2, [0x0a, 0x0b, 0x0c, 0xff]);
    let first_id = first.id.0;
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_screen_picker_with(window, SourceKind::Monitor, Vec::new(), None);
            panel.test_set_screen_preview(window, first);
            assert_eq!(panel.test_screen_preview_image_id(), Some(first_id));

            panel.test_stop_screen_share(window, cx);
            assert_eq!(panel.test_screen_images_released(), 1);
            assert_eq!(panel.test_screen_preview_image_id(), None);
            // A repeated Stop has nothing left and must not panic.
            panel.test_stop_screen_share(window, cx);
            assert_eq!(
                panel.test_screen_images_released(),
                1,
                "the second Stop releases nothing"
            );
            panel.test_reset_screen(window);
            assert_eq!(
                panel.test_screen_images_released(),
                1,
                "reset after Stop releases nothing"
            );
            panel.test_reset_screen(window);
            assert_eq!(
                panel.test_screen_images_released(),
                1,
                "a repeated reset releases nothing"
            );

            let second = plate(2, 2, [0x0d, 0x0e, 0x0f, 0xff]);
            let second_id = second.id.0;
            assert_ne!(second_id, first_id, "the new preview is a new texture");
            panel.test_set_screen_preview(window, second);
            assert_eq!(
                panel.test_screen_preview_image_id(),
                Some(second_id),
                "a new preview is installed after the resets"
            );
            assert_eq!(
                panel.test_screen_images_released(),
                1,
                "filling the empty slot replaces nothing"
            );
        });
    });
}
