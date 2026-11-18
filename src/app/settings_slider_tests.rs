//! The real settings slider: pointer drag, a click on the track, step 1, both
//! ends and a fill that follows the measured flex width.

use super::*;
use gpui::{point, px, size, Modifiers, TestAppContext, VisualTestContext};

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

/// The settings dialog on the voice tab, with the microphone volume at 100 of
/// 200 and a measured track after the first paint.
fn fixture(cx: &mut TestAppContext) -> (&'static mut VisualTestContext, Entity<Voxhold>) {
    cx.update(|cx| gpui_component::init(cx));
    let client = voxhold_api::Client::new("http://127.0.0.1:1").unwrap();
    let (window, view) = super::settings_tests::make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            v.personal.as_mut().unwrap().dialog.active = 1;
            v.voice_settings.path = None;
            v.voice_settings.prefs.input_volume = 100;
            v.voice_settings.prefs.output_volume = 100;
            v.voice_settings.prefs.noise_mode = "threshold".into();
            v.voice_settings.prefs.noise_gate = 50;
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    (visual, view)
}

fn track(
    view: &Entity<Voxhold>,
    visual: &VisualTestContext,
    id: &str,
) -> super::settings_slider::SliderTrack {
    view.read_with(visual, |v, _| v.test_slider_track(id))
        .unwrap_or_else(|| panic!("{id} was measured"))
}

fn row_center(visual: &mut VisualTestContext, id: &'static str) -> f32 {
    let bounds = visual
        .debug_bounds(id)
        .unwrap_or_else(|| panic!("{id} is painted"));
    f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.0
}

/// GPUI only dispatches a click to hovered hitboxes, so a test click moves the
/// pointer first, exactly as it does in front of a real window.
fn click(visual: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>) {
    visual.simulate_mouse_move(position, None, Modifiers::default());
    visual.simulate_click(position, Modifiers::default());
}

#[gpui::test]
async fn a_click_on_the_track_sets_the_value_and_both_ends_are_reachable(cx: &mut TestAppContext) {
    let (mut visual, view) = fixture(cx);
    let measured = track(&view, &*visual, "input-volume");
    let y = row_center(&mut visual, "input-volume-track");
    assert_eq!(
        view.read_with(&*visual, |v, _| v.voice_settings.prefs.input_volume),
        100
    );

    // The very start of the track is 0, the very end is 200. Both ends must be
    // reachable without a margin, as in the Go slider.
    click(&mut visual, point(px(measured.origin_x), px(y)));
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_settings.prefs.input_volume, 0,
            "a click at the left edge selects 0"
        )
    });
    draw(&mut visual);
    let measured = track(&view, &*visual, "input-volume");
    click(
        &mut visual,
        point(px(measured.origin_x + measured.width), px(y)),
    );
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_settings.prefs.input_volume, 200,
            "a click at the right edge selects the maximum"
        )
    });
}

#[gpui::test]
async fn dragging_the_track_moves_the_value_with_step_one(cx: &mut TestAppContext) {
    let (mut visual, view) = fixture(cx);
    let measured = track(&view, &*visual, "input-volume");
    let y = row_center(&mut visual, "input-volume-track");

    let start = point(px(measured.origin_x + measured.width * 0.25), px(y));
    visual.simulate_mouse_move(start, None, Modifiers::default());
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    view.read_with(&*visual, |v, _| {
        let value = v.voice_settings.prefs.input_volume;
        assert!(
            (value - 50).abs() <= 1,
            "a press at 25 % selects about 50, got {value}"
        )
    });
    draw(&mut visual);
    visual.simulate_mouse_move(
        point(px(measured.origin_x + measured.width * 0.5), px(y)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    view.read_with(&*visual, |v, _| {
        let value = v.voice_settings.prefs.input_volume;
        assert!(
            (value - 100).abs() <= 1,
            "dragging to the middle selects about 100, got {value}"
        )
    });
    draw(&mut visual);
    visual.simulate_mouse_move(
        point(px(measured.origin_x + measured.width * 0.75), px(y)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    view.read_with(&*visual, |v, _| {
        let value = v.voice_settings.prefs.input_volume;
        assert!(
            (value - 150).abs() <= 1,
            "dragging to 75 % selects about 150, got {value}"
        )
    });
    let value = view.read_with(&*visual, |v, _| v.voice_settings.prefs.input_volume);
    visual.simulate_mouse_up(
        point(px(measured.origin_x + measured.width * 0.75), px(y)),
        MouseButton::Left,
        Modifiers::default(),
    );
    // The release must not change the value again.
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.input_volume, value)
    });
}

#[gpui::test]
async fn the_keyboard_steps_by_one_and_home_end_hit_the_bounds(cx: &mut TestAppContext) {
    let (mut visual, view) = fixture(cx);
    visual.update(|window, app| {
        let focus = view
            .read(app)
            .test_slider_focus("input-volume")
            .expect("the slider is a tab stop");
        window.focus(&focus);
    });
    draw(&mut visual);
    visual.simulate_keystrokes("right");
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_settings.prefs.input_volume, 101,
            "the arrow key moves exactly one step, not five"
        )
    });
    visual.simulate_keystrokes("end");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.input_volume, 200)
    });
    visual.simulate_keystrokes("left");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.input_volume, 199)
    });
    visual.simulate_keystrokes("home");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.input_volume, 0)
    });
}

#[gpui::test]
async fn dragging_past_the_card_keeps_updating_and_clamps_to_the_ends(cx: &mut TestAppContext) {
    let (mut visual, view) = fixture(cx);
    let measured = track(&view, &*visual, "input-volume");
    let y = row_center(&mut visual, "input-volume-track");
    let card = visual
        .debug_bounds("settings-card")
        .expect("the card is painted");
    let start = point(px(measured.origin_x + measured.width * 0.5), px(y));
    visual.simulate_mouse_move(start, None, Modifiers::default());
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.input_volume, 100)
    });

    // The Go slider captures the pointer: dragging far outside the card keeps
    // updating the value and clamps at the ends instead of freezing.
    let outside_right = f32::from(card.origin.x + card.size.width) + 150.0;
    visual.simulate_mouse_move(
        point(px(outside_right), px(y)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_settings.prefs.input_volume, 200,
            "a pointer right of the card clamps at 200"
        )
    });
    draw(&mut visual);
    let outside_left = f32::from(card.origin.x) - 150.0;
    visual.simulate_mouse_move(
        point(px(outside_left), px(y)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_settings.prefs.input_volume, 0,
            "a pointer left of the card clamps at 0"
        )
    });
    visual.simulate_mouse_up(
        point(px(outside_left), px(y)),
        MouseButton::Left,
        Modifiers::default(),
    );
    // The release outside the card must also end the drag: a later move
    // without the button cannot keep changing the value.
    visual.simulate_mouse_move(point(px(outside_right), px(y)), None, Modifiers::default());
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.input_volume, 0)
    });
}

#[gpui::test]
async fn the_fill_tracks_the_measured_width_at_two_card_sizes(cx: &mut TestAppContext) {
    let (mut visual, view) = fixture(cx);
    let wide = track(&view, &*visual, "input-volume");
    assert!(
        (wide.fill / wide.width - 0.5).abs() < 0.01,
        "100 of 200 paints half of the measured track: {wide:?}"
    );

    // A narrower card gives the flex track a different real width; the fill
    // must follow it instead of a fixed 220 px.
    visual.simulate_resize(size(px(600.), px(700.)));
    draw(&mut visual);
    draw(&mut visual);
    let narrow = track(&view, &*visual, "input-volume");
    assert!(
        (wide.width - narrow.width).abs() > 40.0,
        "the card resize must change the track width: {wide:?} vs {narrow:?}"
    );
    assert!(
        (narrow.fill / narrow.width - 0.5).abs() < 0.01,
        "the fill keeps the value ratio at the new width: {narrow:?}"
    );
    assert!(
        (narrow.fill - narrow.width * 0.5).abs() < 1.0,
        "the painted fill is exactly width * value / max"
    );
}
