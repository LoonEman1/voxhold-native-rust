//! Preview demand of the publisher PiP.
//!
//! The PiP is a window-level overlay, not a room-pane child: it is painted on a
//! text channel, another voice room and the personal surface, so the capture
//! demand must follow its picture there too. This module sits next to the room
//! card preview tests because both read the same single predicate, while the
//! overlay geometry lives in the PiP tests.

use super::voice_screen_tests::draw;
use gpui::TestAppContext;

/// P1: the publisher PiP is a window-level overlay, so its preview demand must
/// follow the painted picture on every surface the user can navigate to, not
/// only inside the room pane. The owner room keeps the O02 on-air card rule.
///
/// The three PiP surfaces render in a different order than the shell stores the
/// card rect of the room grid: the overlay is built by the shell, the child
/// panel renders afterwards and clears that rect. Asking the pure PiP geometry
/// instead makes the demand independent of that order.
#[gpui::test]
async fn the_publisher_pip_requests_its_picture_on_every_surface(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            super::voice_pip_tests::publish_live(panel, window, cx, 5);
        });
    });

    // O02: inside the owner room the on-air card still sets the demand, and the
    // width is the card the card paints.
    super::voice_pip_tests::open_room(&mut visual, &view, 7);
    assert!(
        visual.debug_bounds("screen-on-air").is_some(),
        "the owner room paints the on-air card"
    );
    let expected = painted_preview_width(&mut visual, "screen-on-air");
    let (enabled, width) = sync_screen_preview_demand(&mut visual, &voice);
    assert!(enabled, "the on-air card keeps the owner-room demand");
    assert_eq!(width, expected, "the owner-room demand follows its card");

    // A text channel: the panel itself is not rendered, only the overlay is.
    super::voice_pip_tests::open_room(&mut visual, &view, 9);
    assert!(
        visual.debug_bounds("voice-pip").is_some(),
        "a text channel paints the PiP"
    );
    let expected = painted_preview_width(&mut visual, "pip-picture");
    let (enabled, width) = sync_screen_preview_demand(&mut visual, &voice);
    assert!(enabled, "the PiP in a text channel requests its picture");
    assert_eq!(width, expected, "the demand is the PiP picture width");

    // Another voice room: the child panel does render here and resets the rect
    // of the grid card, which must not starve the PiP.
    super::voice_pip_tests::open_room(&mut visual, &view, 8);
    assert!(visual.debug_bounds("voice-pip").is_some());
    let expected = painted_preview_width(&mut visual, "pip-picture");
    let (enabled, width) = sync_screen_preview_demand(&mut visual, &voice);
    assert!(
        enabled,
        "the PiP in another voice room requests its picture"
    );
    assert_eq!(width, expected, "the demand is the PiP picture width");

    // The personal surface (Messages/DM) paints the same overlay.
    visual.update(|_window, app| {
        view.update(app, |view, cx| {
            view.personal_mode = true;
            cx.notify();
        });
    });
    draw(&mut visual);
    let expected = painted_preview_width(&mut visual, "pip-picture");
    let (enabled, width) = sync_screen_preview_demand(&mut visual, &voice);
    assert!(
        enabled,
        "the PiP on the personal surface requests a picture"
    );
    assert_eq!(width, expected, "the demand is the PiP picture width");
}

#[gpui::test]
async fn fullscreen_viewing_suspends_the_hidden_publisher_preview(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            super::voice_pip_tests::publish_live(panel, window, cx, 5);
            super::voice_pip_tests::watch_publication(panel, window, cx);
        });
    });
    super::voice_pip_tests::open_room(&mut visual, &view, 9);
    voice.update(&mut *visual, |panel, _| {
        panel.test_select_pip_role(crate::voice_panel::PipRole::Publisher);
    });
    draw(&mut visual);
    assert!(sync_screen_preview_demand(&mut visual, &voice).0);

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.player_toggle_fullscreen(window);
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(voice.read_with(&*visual, |panel, _| panel.player_fullscreen()));
    assert!(!sync_screen_preview_demand(&mut visual, &voice).0);

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.exit_fullscreen(window);
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(visual.debug_bounds("voice-pip").is_some());
    assert!(sync_screen_preview_demand(&mut visual, &voice).0);
}

/// The physical preview width the painted `selector` asks for, following the
/// same scale and clamp as the production request.
fn painted_preview_width(visual: &mut gpui::VisualTestContext, selector: &'static str) -> u32 {
    use voxhold_media::screen_capture::MAX_PREVIEW_WIDTH;
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"));
    let scale = visual.update(|window, _app| window.scale_factor());
    let physical = (f32::from(bounds.size.width) * scale.max(0.1)).ceil();
    (physical.min(MAX_PREVIEW_WIDTH as f32) as u32).clamp(2, MAX_PREVIEW_WIDTH)
}

/// Run the production demand sync and read back what it wrote.
fn sync_screen_preview_demand(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
) -> (bool, u32) {
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_sync_screen_preview_demand(window);
            panel
                .test_screen_preview_demand()
                .expect("the fixture attached a detached handle")
        })
    })
}
