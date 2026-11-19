//! The rendered voice form: every control of the scenario exists, both mode
//! switches keep their stored values when they are used, and the layout holds
//! at 125 %/150 % (the view scale parameter, not a physical monitor DPI).

use super::*;
use gpui::{px, size, Modifiers, TestAppContext, VisualTestContext};

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

fn device(id: &str, name: &str, input: bool) -> crate::voice_settings::Device {
    crate::voice_settings::Device {
        id: id.into(),
        name: name.into(),
        input,
    }
}

/// The settings dialog open on the voice tab, with two microphones, a speaker
/// and the default device rows.
fn voice_tab(cx: &mut TestAppContext) -> (&'static mut VisualTestContext, Entity<Voxhold>) {
    cx.update(|cx| gpui_component::init(cx));
    let client = voxhold_api::Client::new("http://127.0.0.1:1").unwrap();
    let (window, view) = super::settings_tests::make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            v.personal.as_mut().unwrap().dialog.active = 1;
            v.voice_settings.path = None;
            v.voice_settings.devices = vec![
                device("mic-a", "USB Mic", true),
                device("mic-b", "Headset", true),
                device("spk-a", "Speakers", false),
            ];
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    (visual, view)
}

fn center(visual: &mut VisualTestContext, id: &'static str) -> gpui::Point<gpui::Pixels> {
    let bounds = visual
        .debug_bounds(id)
        .unwrap_or_else(|| panic!("{id} is painted"));
    gpui::point(
        bounds.origin.x + bounds.size.width / 2.,
        bounds.origin.y + bounds.size.height / 2.,
    )
}

/// GPUI dispatches a click only to hovered hitboxes, so the test moves the
/// pointer first like a real one does.
fn click(visual: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>) {
    visual.simulate_mouse_move(position, None, Modifiers::default());
    visual.simulate_click(position, Modifiers::default());
}

/// The voice form is taller than the dialog, so a control below the fold must be
/// scrolled into view exactly as the user would; a clipped element cannot be
/// clicked.
fn scroll_into_view(visual: &mut VisualTestContext, view: &Entity<Voxhold>, id: &'static str) {
    let card = visual
        .debug_bounds("settings-card")
        .expect("the card is painted");
    let bounds = visual
        .debug_bounds(id)
        .unwrap_or_else(|| panic!("{id} is painted"));
    let body_bottom = f32::from(card.origin.y + card.size.height) - 40.;
    let y = f32::from(bounds.origin.y);
    if y <= body_bottom - 80.0 {
        return;
    }
    let offset = view.read_with(&*visual, |v, _| f32::from(v.settings_scroll.offset().y));
    // The offset is negative as the content scrolls up.
    let target = offset - (y - body_bottom + 80.0);
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.settings_scroll
                .set_offset(gpui::point(px(0.), px(target)))
        })
    });
    draw(visual);
}

fn click_in_form(visual: &mut VisualTestContext, view: &Entity<Voxhold>, id: &'static str) {
    scroll_into_view(visual, view, id);
    let at = center(visual, id);
    click(visual, at);
    draw(visual);
}

#[gpui::test]
async fn the_voice_form_renders_every_control_of_the_scenario(cx: &mut TestAppContext) {
    let (mut visual, view) = voice_tab(cx);
    // Two device selectors (input/output) with their arrows and the refresh
    // action inside the devices card.
    for id in [
        "audio-in-selector",
        "audio-in-name",
        "audio-in-prev",
        "audio-in-next",
        "audio-out-selector",
        "audio-out-name",
        "audio-out-prev",
        "audio-out-next",
        "audio-refresh",
    ] {
        assert!(visual.debug_bounds(id).is_some(), "{id} is missing");
    }
    // Levels, meter, preview and the test tone.
    assert!(visual.debug_bounds("input-volume").is_some());
    assert!(visual.debug_bounds("voice-test-mic").is_some());
    assert!(visual.debug_bounds("output-volume").is_some());
    for id in [
        "bitrate-0",
        "bitrate-1",
        "bitrate-2",
        "bitrate-3",
        "bitrate-4",
        "bitrate-5",
    ] {
        assert!(visual.debug_bounds(id).is_some(), "{id} is missing");
    }
    assert!(visual.debug_bounds("sounds").is_some());
    assert!(visual.debug_bounds("sound-volume").is_some());
    assert!(visual.debug_bounds("voice-test-tone").is_some());
    assert!(visual.debug_bounds("noise-mode-0").is_some());
    assert!(visual.debug_bounds("noise-mode-1").is_some());
    assert!(visual.debug_bounds("auto-gain").is_some());
    assert!(visual.debug_bounds("input-mode-voice").is_some());
    assert!(visual.debug_bounds("input-mode-ptt").is_some());
    assert!(visual.debug_bounds("voice-reset").is_some());
    assert!(visual.debug_bounds("voice-done").is_some());
    // The gate slider only exists in threshold mode; switching through the
    // form must reveal it.
    assert!(
        visual.debug_bounds("noise-gate").is_none(),
        "auto mode has no gate slider"
    );
    click_in_form(&mut visual, &view, "noise-mode-1");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.noise_mode, "threshold")
    });
    assert!(
        visual.debug_bounds("noise-gate").is_some(),
        "threshold mode shows the gate slider"
    );
}

#[gpui::test]
async fn switching_modes_through_the_form_keeps_their_values(cx: &mut TestAppContext) {
    let (mut visual, view) = voice_tab(cx);

    // Noise mode: threshold, set the gate with the slider, back to auto and to
    // threshold again. Only the form is used; no field is assigned directly.
    click_in_form(&mut visual, &view, "noise-mode-1");
    let track = view
        .read_with(&*visual, |v, _| v.test_slider_track("noise-gate"))
        .expect("the gate slider is measured");
    scroll_into_view(&mut visual, &view, "noise-gate-track");
    let row = visual
        .debug_bounds("noise-gate-track")
        .expect("gate track row");
    let y = row.origin.y + row.size.height / 2.;
    let gate_point = gpui::point(px(track.origin_x + track.width * 0.4), y);
    click(&mut visual, gate_point);
    draw(&mut visual);
    let gate = view.read_with(&*visual, |v, _| v.voice_settings.prefs.noise_gate);
    assert!(
        (gate - 40).abs() <= 1,
        "the gate follows the slider: {gate}"
    );

    click_in_form(&mut visual, &view, "noise-mode-0");
    click_in_form(&mut visual, &view, "noise-mode-1");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.noise_mode, "threshold");
        assert!(
            (v.voice_settings.prefs.noise_gate - gate).abs() <= 1,
            "switching modes must not reset the saved gate"
        );
    });

    // Input mode: push-to-talk, back to voice activity, then push-to-talk
    // again; the shortcut strings are untouched and both states persist.
    click_in_form(&mut visual, &view, "input-mode-ptt");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.input_mode, "push_to_talk")
    });
    click_in_form(&mut visual, &view, "input-mode-voice");
    click_in_form(&mut visual, &view, "input-mode-ptt");
    view.read_with(&*visual, |v, _| {
        let prefs = &v.voice_settings.prefs;
        assert_eq!(prefs.input_mode, "push_to_talk");
        assert_eq!(prefs.noise_mode, "threshold");
        assert_eq!(prefs.push_to_talk, "Space");
        assert_eq!(prefs.mute, "Ctrl+Shift+M");
        assert_eq!(prefs.deafen, "Ctrl+Shift+D");
    });
}

/// Every control the voice form paints for one stage of the scenario. The ids
/// are checked against the card box at each scale.
const FORM_CONTROLS: [&str; 16] = [
    "audio-in-selector",
    "audio-out-selector",
    "audio-refresh",
    "input-volume",
    "voice-test-mic",
    "output-volume",
    "bitrate-0",
    "bitrate-5",
    "sounds",
    "noise-mode-0",
    "noise-mode-1",
    "auto-gain",
    "input-mode-voice",
    "input-mode-ptt",
    "voice-diagnostics-toggle",
    "voice-done",
];

fn assert_form_fits(visual: &mut VisualTestContext, scale: f32) {
    let card = visual
        .debug_bounds("settings-card")
        .expect("the card is painted");
    let (left, right) = (
        f32::from(card.origin.x),
        f32::from(card.origin.x + card.size.width),
    );
    let (top, bottom) = (
        f32::from(card.origin.y),
        f32::from(card.origin.y + card.size.height),
    );
    let viewport = visual.update(|window, _| {
        let size = window.viewport_size();
        (f32::from(size.width), f32::from(size.height))
    });
    assert!(
        left >= -0.5 && right <= viewport.0 + 0.5 && top >= -0.5 && bottom <= viewport.1 + 0.5,
        "the card stays inside the window at {scale}: {card:?} in {viewport:?}"
    );
    let tab = visual
        .debug_bounds("settings-tab-1")
        .expect("the active tab is painted");
    assert!(
        f32::from(tab.origin.y) >= top - 0.5
            && f32::from(tab.origin.y + tab.size.height) <= bottom + 0.5,
        "the tab strip stays inside the card at {scale}: {tab:?}"
    );
    let mut boxes = Vec::new();
    for id in FORM_CONTROLS {
        let bounds = visual
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("{id} is painted at {scale}"));
        let (x, w) = (f32::from(bounds.origin.x), f32::from(bounds.size.width));
        let (y, h) = (f32::from(bounds.origin.y), f32::from(bounds.size.height));
        assert!(
            w > 0.0 && h > 0.0,
            "{id} has no size at {scale}: {bounds:?}"
        );
        // The body scrolls vertically, so only horizontal clipping and a
        // control above the card top are layout errors.
        assert!(
            x >= left - 0.5 && x + w <= right + 0.5,
            "{id} is clipped by the card at {scale}: {bounds:?} vs {card:?}"
        );
        assert!(
            y >= top - 0.5,
            "{id} starts above the card at {scale}: {bounds:?} vs {card:?}"
        );
        // A control that fits vertically must also stay inside the card.
        if y + h <= bottom {
            assert!(
                y + h <= bottom + 0.5,
                "{id} leaves the card at {scale}: {bounds:?}"
            );
        }
        boxes.push((id, (x, y, w, h)));
    }
    // A narrower card must not squeeze the volume slider into a stub: the
    // draggable track keeps a usable width at every scale.
    let slider = boxes
        .iter()
        .find(|(id, _)| *id == "input-volume")
        .expect("the volume slider is painted");
    assert!(
        slider.1 .2 >= 200.0,
        "the volume slider track is at least 200 px at {scale}: {:?}",
        slider.1
    );
    // Controls must not overlap each other; a squeezed form that paints two
    // rows on top of each other fails here.
    for (index, (first_id, first)) in boxes.iter().enumerate() {
        for (second_id, second) in boxes.iter().skip(index + 1) {
            let (ax, ay, aw, ah) = *first;
            let (bx, by, bw, bh) = *second;
            let overlap_x = ax < bx + bw - 0.5 && bx < ax + aw - 0.5;
            let overlap_y = ay < by + bh - 0.5 && by < ay + ah - 0.5;
            assert!(
                !(overlap_x && overlap_y),
                "{first_id} overlaps {second_id} at {scale}: {first:?} / {second:?}"
            );
        }
    }
}

/// The card geometry must depend on the pinned view scale, not just on the
/// window: `680.min(vw - 16*scale)` leaves a constant physical margin. A window
/// narrower than 680+16 makes that observable; ignoring the scale (or pinning
/// the old 16 px inset) changes the expected width and fails below.
#[gpui::test]
async fn the_settings_card_follows_the_view_scale(cx: &mut TestAppContext) {
    let (viewport_width, viewport_height) = (690.0_f32, 700.0_f32);
    let mut widths = Vec::new();
    for scale in [1.0_f32, 1.25, 1.5] {
        let (mut visual, view) = voice_tab(cx);
        visual.update(|window, app| {
            view.update(app, |v, _| v.test_pin_ui_scale(scale));
            let _ = window;
        });
        visual.simulate_resize(size(px(viewport_width), px(viewport_height)));
        draw(&mut visual);
        draw(&mut visual);
        let card = visual
            .debug_bounds("settings-card")
            .expect("the card is painted");
        let width = f32::from(card.size.width);
        let expected = 680.0_f32.min(viewport_width - 16.0 * scale).max(260.0);
        assert!(
            (width - expected).abs() < 0.5,
            "scale {scale}: the card must be {expected} px wide, got {width}"
        );
        let left = f32::from(card.origin.x);
        assert!(
            (left - (viewport_width - width) / 2.0).abs() < 0.5,
            "scale {scale}: the card stays centered, left {left} width {width}"
        );
        widths.push(width);
        assert_form_fits(&mut visual, scale);
    }
    assert!(
        widths[1] < widths[0] && widths[2] < widths[1],
        "a larger view scale must shrink the card: {widths:?}"
    );
}

#[gpui::test]
async fn the_voice_form_stays_usable_at_the_125_and_150_logical_viewport(cx: &mut TestAppContext) {
    // The test window has no platform DPI; this pins the view's scale parameter
    // and the logical viewport a 1600x900 panel reports at that scale. It is a
    // layout check, not a physical DPI measurement.
    for (scale, width, height) in [(1.25_f32, 1280.0_f32, 720.0_f32), (1.5, 1067.0, 600.0)] {
        let (mut visual, view) = voice_tab(cx);
        visual.update(|window, app| {
            view.update(app, |v, _| v.test_pin_ui_scale(scale));
            let _ = window;
        });
        visual.simulate_resize(size(px(width), px(height)));
        draw(&mut visual);
        draw(&mut visual);
        assert_form_fits(&mut visual, scale);
        // The 680 px card still fits inside this logical viewport.
        let card = visual.debug_bounds("settings-card").unwrap();
        assert!(f32::from(card.size.width) <= width + 0.5);
    }
}

/// The selector neighbours are endpoint ids, never row indexes: the first
/// arrow at the system default reaches the first real endpoint, the last arrow
/// stays on the last endpoint, and a saved id that is no longer listed recovers
/// to the system default instead of picking whatever row now has that position.
#[test]
fn adjacent_device_ids_use_the_endpoint_id_and_recover_from_a_missing_one() {
    use super::settings_voice_view::adjacent_device_ids;
    let ids = vec!["".to_owned(), "mic-a".into(), "mic-b".into()];
    assert_eq!(
        adjacent_device_ids(&ids, ""),
        ("".to_owned(), "mic-a".to_owned())
    );
    assert_eq!(
        adjacent_device_ids(&ids, "mic-a"),
        ("".to_owned(), "mic-b".to_owned())
    );
    assert_eq!(
        adjacent_device_ids(&ids, "mic-b"),
        ("mic-a".to_owned(), "mic-b".to_owned())
    );
    assert_eq!(
        adjacent_device_ids(&ids, "gone"),
        ("".to_owned(), "".to_owned()),
        "an unavailable id recovers to a valid choice"
    );
    assert_eq!(adjacent_device_ids(&[], ""), ("".to_owned(), "".to_owned()));
}

/// The form width must subtract both the dialog padding and the 16 px inset
/// the card paints on each side. Ignoring the card inset turned the bitrate row
/// on 32 px too early.
#[test]
fn form_width_subtracts_the_dialog_and_card_insets() {
    use super::settings_view::{settings_card_metrics, CARD_INSET};
    assert_eq!(CARD_INSET, 16.0);
    let design = settings_card_metrics((1200., 900.), 1.0);
    assert_eq!((design.width, design.pad), (680., 24.));
    assert_eq!(design.form_width(), 680. - 2. * 24. - 2. * 16.);
    assert_eq!(design.form_width(), 600.);
    let narrow = settings_card_metrics((400., 900.), 1.0);
    assert_eq!((narrow.width, narrow.pad), (384., 16.));
    assert_eq!(narrow.form_width(), 384. - 2. * 16. - 2. * 16.);
}

/// The one-row threshold is derived from six padded cells and five 8 px gaps
/// with a conservative 65 px label. GPUI exposes no glyph bounds to a test, so
/// the decision asserts the cell budget instead of literal text measurement.
#[test]
fn bitrate_one_row_threshold_uses_the_padded_widest_label() {
    use super::settings_view::settings_card_metrics;
    use super::settings_voice_view::{
        bitrate_cell_width, BITRATE_BUTTON_PADDING, BITRATE_LABEL_WIDTH, BITRATE_ONE_ROW_MIN_WIDTH,
    };
    assert_eq!(BITRATE_LABEL_WIDTH, 65.0);
    assert_eq!(BITRATE_ONE_ROW_MIN_WIDTH, 6. * (65. + 28.) + 5. * 8.);
    assert_eq!(BITRATE_ONE_ROW_MIN_WIDTH, 598.);
    let design = settings_card_metrics((1200., 900.), 1.0).form_width();
    assert_eq!(design, 600.);
    assert!(design >= BITRATE_ONE_ROW_MIN_WIDTH);
    assert!(
        bitrate_cell_width(design) - BITRATE_BUTTON_PADDING >= BITRATE_LABEL_WIDTH,
        "one-row cell budget {} px must fit the {} px label",
        bitrate_cell_width(design) - BITRATE_BUTTON_PADDING,
        BITRATE_LABEL_WIDTH
    );
    let compact = settings_card_metrics((636., 900.), 1.0);
    assert_eq!(compact.width, 620.);
    assert_eq!(compact.form_width(), 540.);
    assert!(compact.form_width() < BITRATE_ONE_ROW_MIN_WIDTH);
}

/// The unavailable label comes from the shared `UNAVAILABLE_DEVICE` constant,
/// so `device_choices` and the danger styling in the selector cannot drift. The
/// selector's endpoint-id selection is not affected by the label.
#[test]
fn unavailable_device_label_drives_the_danger_path() {
    use super::settings_voice_view::device_name_is_unavailable;
    use crate::voice_settings::{device_choices, VoicePrefs, UNAVAILABLE_DEVICE};
    assert_eq!(UNAVAILABLE_DEVICE, "Устройство недоступно");
    assert!(device_name_is_unavailable(UNAVAILABLE_DEVICE));
    assert!(!device_name_is_unavailable("Speakers"));
    assert!(!device_name_is_unavailable(""));

    let mut prefs = VoicePrefs::default();
    prefs.input = "gone".into();
    let devices = [device("mic-a", "USB Mic", true)];
    let (_, ids, current) = device_choices(&prefs, &devices, true);
    assert_eq!(current, UNAVAILABLE_DEVICE);
    assert!(
        device_name_is_unavailable(&current),
        "an unavailable saved id paints the danger name"
    );
    // The arrows still walk the listed endpoint ids, not the label.
    assert_eq!(ids, vec!["".to_owned(), "mic-a".to_owned()]);
}

/// Resize, repaint twice (the first frame measures the new card) and return.
fn resize(visual: &mut VisualTestContext, width: f32, height: f32) {
    visual.simulate_resize(size(px(width), px(height)));
    draw(visual);
    draw(visual);
}

#[gpui::test]
async fn device_selectors_are_side_by_side_when_wide_and_stack_when_narrow(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view) = voice_tab(cx);
    resize(&mut visual, 1000., 800.);
    let wide_in = visual
        .debug_bounds("audio-in-selector")
        .expect("the input selector is painted");
    let wide_out = visual
        .debug_bounds("audio-out-selector")
        .expect("the output selector is painted");
    let (wide_in_x, wide_in_y) = (f32::from(wide_in.origin.x), f32::from(wide_in.origin.y));
    let (wide_in_w, wide_in_h) = (
        f32::from(wide_in.size.width),
        f32::from(wide_in.size.height),
    );
    let (wide_out_x, wide_out_y) = (f32::from(wide_out.origin.x), f32::from(wide_out.origin.y));
    let wide_out_w = f32::from(wide_out.size.width);
    assert!(
        wide_in_y < wide_out_y + 0.5 && wide_out_y < wide_in_y + wide_in_h - 0.5,
        "wide form keeps both selectors on one row: {wide_in:?} / {wide_out:?}"
    );
    assert!(
        wide_out_x >= wide_in_x + wide_in_w - 0.5,
        "the output selector starts after the input selector: {wide_in:?} / {wide_out:?}"
    );
    assert!(
        (wide_in_w - wide_out_w).abs() < 1.0,
        "both columns share the row: {wide_in_w} vs {wide_out_w}"
    );

    resize(&mut visual, 600., 800.);
    let narrow_in = visual
        .debug_bounds("audio-in-selector")
        .expect("the input selector is still painted");
    let narrow_out = visual
        .debug_bounds("audio-out-selector")
        .expect("the output selector is still painted");
    let (narrow_in_x, narrow_in_y) = (f32::from(narrow_in.origin.x), f32::from(narrow_in.origin.y));
    let (narrow_in_w, narrow_in_h) = (
        f32::from(narrow_in.size.width),
        f32::from(narrow_in.size.height),
    );
    let (narrow_out_x, narrow_out_y) = (
        f32::from(narrow_out.origin.x),
        f32::from(narrow_out.origin.y),
    );
    assert!(
        narrow_out_y >= narrow_in_y + narrow_in_h - 0.5,
        "narrow form stacks the output selector below the input one: {narrow_in:?} / {narrow_out:?}"
    );
    assert!(
        (narrow_in_x - narrow_out_x).abs() < 0.5,
        "stacked selectors share the left edge: {narrow_in:?} / {narrow_out:?}"
    );
    assert!(
        narrow_in_w > wide_in_w + 1.0,
        "a stacked selector uses the full form width: {narrow_in_w} vs {wide_in_w}"
    );
}

#[gpui::test]
async fn selector_arrows_move_by_endpoint_id_and_recover_from_a_missing_device(
    cx: &mut TestAppContext,
) {
    let (mut visual, view) = voice_tab(cx);
    let read_input = |visual: &VisualTestContext| {
        view.read_with(visual, |v, _| v.voice_settings.prefs.input.clone())
    };
    let read_output = |visual: &VisualTestContext| {
        view.read_with(visual, |v, _| v.voice_settings.prefs.output.clone())
    };

    assert_eq!(read_input(&visual), "", "the default is the system device");
    click_in_form(&mut visual, &view, "audio-in-next");
    assert_eq!(read_input(&visual), "mic-a");
    click_in_form(&mut visual, &view, "audio-in-next");
    assert_eq!(read_input(&visual), "mic-b");
    click_in_form(&mut visual, &view, "audio-in-next");
    assert_eq!(
        read_input(&visual),
        "mic-b",
        "the last endpoint does not wrap or fall off the list"
    );
    click_in_form(&mut visual, &view, "audio-in-prev");
    assert_eq!(read_input(&visual), "mic-a");
    click_in_form(&mut visual, &view, "audio-in-prev");
    assert_eq!(
        read_input(&visual),
        "",
        "the previous arrow reaches the empty system-default id"
    );

    click_in_form(&mut visual, &view, "audio-out-next");
    assert_eq!(read_output(&visual), "spk-a");
    click_in_form(&mut visual, &view, "audio-out-prev");
    assert_eq!(read_output(&visual), "");

    // A saved endpoint that is no longer listed stays visible (danger name) but
    // the arrows must recover to a valid choice instead of a row index.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.voice_settings.prefs.input = "gone".into();
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("audio-in-name").is_some(),
        "the unavailable device stays visible by name"
    );
    click_in_form(&mut visual, &view, "audio-in-next");
    assert_eq!(read_input(&visual), "");
    view.read_with(&*visual, |v, _| {
        assert!(v.voice_settings.device_notice.is_empty())
    });
}

#[gpui::test]
async fn long_identical_device_names_are_truncated_and_select_by_endpoint_id(
    cx: &mut TestAppContext,
) {
    const LONG: &str =
        "Микрофон настольного компьютера Realtek High Definition Audio — вход (копия)";
    let (mut visual, view) = voice_tab(cx);
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.voice_settings.devices = vec![
                device("mic-long-a", LONG, true),
                device("mic-long-b", LONG, true),
                device("spk-a", "Speakers", false),
            ];
            v.voice_settings.prefs.input = "mic-long-a".into();
            cx.notify();
        });
    });
    draw(&mut visual);
    let selector = visual
        .debug_bounds("audio-in-selector")
        .expect("the selector is painted");
    let name = visual
        .debug_bounds("audio-in-name")
        .expect("the current name is painted");
    let (selector_x, selector_w) = (f32::from(selector.origin.x), f32::from(selector.size.width));
    let (name_x, name_w) = (f32::from(name.origin.x), f32::from(name.size.width));
    assert!(
        name_x >= selector_x - 0.5 && name_x + name_w <= selector_x + selector_w + 0.5,
        "the long name is clipped inside the selector instead of overflowing it: {name:?} vs {selector:?}"
    );
    assert!(
        name_w <= selector_w - 64.0,
        "the name yields room to both arrows: {name_w} of {selector_w}"
    );

    // Identical labels are not the identity: the arrow selects the next
    // endpoint id even though both rows paint the same truncated text.
    click_in_form(&mut visual, &view, "audio-in-next");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.input, "mic-long-b")
    });
    let after = visual
        .debug_bounds("audio-in-name")
        .expect("the name is still painted");
    assert_eq!(
        f32::from(after.size.width),
        name_w,
        "the truncated box keeps the same width for the identical label"
    );
}

#[gpui::test]
async fn the_refresh_action_sits_in_the_devices_card_header(cx: &mut TestAppContext) {
    let (mut visual, _view) = voice_tab(cx);
    let card = visual
        .debug_bounds("voice-devices-card")
        .expect("the devices card is painted");
    let refresh = visual
        .debug_bounds("audio-refresh")
        .expect("the refresh action is painted");
    let (card_x, card_y) = (f32::from(card.origin.x), f32::from(card.origin.y));
    let (card_w, card_h) = (f32::from(card.size.width), f32::from(card.size.height));
    let (x, y) = (f32::from(refresh.origin.x), f32::from(refresh.origin.y));
    let (w, h) = (
        f32::from(refresh.size.width),
        f32::from(refresh.size.height),
    );
    assert!(
        x >= card_x - 0.5
            && x + w <= card_x + card_w + 0.5
            && y >= card_y - 0.5
            && y + h <= card_y + card_h + 0.5,
        "the refresh action is inside the devices card: {refresh:?} vs {card:?}"
    );
    let selector = visual
        .debug_bounds("audio-in-selector")
        .expect("the selector is painted");
    assert!(
        y + h <= f32::from(selector.origin.y) + 0.5,
        "the refresh action is in the card header, above the selectors"
    );
}

#[gpui::test]
async fn slider_headers_paint_the_label_left_and_the_percentage_right(cx: &mut TestAppContext) {
    let (mut visual, _view) = voice_tab(cx);
    for (row_id, value_id) in [
        ("input-volume", "input-volume-value"),
        ("output-volume", "output-volume-value"),
    ] {
        let row = visual
            .debug_bounds(row_id)
            .unwrap_or_else(|| panic!("{row_id} is painted"));
        let value = visual
            .debug_bounds(value_id)
            .unwrap_or_else(|| panic!("{value_id} is painted"));
        let row_left = f32::from(row.origin.x);
        let row_right = f32::from(row.origin.x + row.size.width);
        let value_left = f32::from(value.origin.x);
        let value_right = f32::from(value.origin.x + value.size.width);
        assert!(
            (row_right - value_right).abs() < 0.5,
            "{value_id} sits at the right edge of {row_id}: {value_right} vs {row_right}"
        );
        assert!(
            value_left > row_left + (row_right - row_left) / 2.0,
            "{value_id} is in the right half of the header"
        );
        assert!(
            f32::from(value.origin.y) >= f32::from(row.origin.y) - 0.5
                && f32::from(value.origin.y + value.size.height)
                    <= f32::from(row.origin.y + row.size.height) + 0.5,
            "{value_id} stays inside the header row"
        );
    }
}

#[gpui::test]
async fn six_bitrate_presets_form_one_row_when_wide_and_two_rows_when_narrow(
    cx: &mut TestAppContext,
) {
    const IDS: [&str; 6] = [
        "bitrate-0",
        "bitrate-1",
        "bitrate-2",
        "bitrate-3",
        "bitrate-4",
        "bitrate-5",
    ];
    let (mut visual, view) = voice_tab(cx);
    resize(&mut visual, 1000., 800.);
    let wide: Vec<_> = IDS
        .iter()
        .map(|id| {
            visual
                .debug_bounds(*id)
                .unwrap_or_else(|| panic!("{id} is painted"))
        })
        .collect();
    let wide_y = f32::from(wide[0].origin.y);
    for (index, bounds) in wide.iter().enumerate() {
        assert!(
            (f32::from(bounds.origin.y) - wide_y).abs() < 0.5,
            "wide form keeps preset {index} on the single row"
        );
    }
    // Default is 64 kbit/s, the fourth of 24/32/48/64/96/128.
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.bitrate, 64);
        assert_eq!(
            crate::voice_settings::BITRATES
                .iter()
                .position(|value| *value == v.voice_settings.prefs.bitrate),
            Some(3),
            "the active preset index is the stored bitrate"
        );
    });
    click_in_form(&mut visual, &view, "bitrate-5");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.bitrate, 128, "128 kbit/s is active")
    });

    resize(&mut visual, 600., 800.);
    let narrow: Vec<_> = IDS
        .iter()
        .map(|id| {
            visual
                .debug_bounds(*id)
                .unwrap_or_else(|| panic!("{id} is still painted"))
        })
        .collect();
    let first_row_y = f32::from(narrow[0].origin.y);
    let second_row_y = f32::from(narrow[3].origin.y);
    for bounds in &narrow[..3] {
        assert!((f32::from(bounds.origin.y) - first_row_y).abs() < 0.5);
    }
    for bounds in &narrow[3..] {
        assert!((f32::from(bounds.origin.y) - second_row_y).abs() < 0.5);
    }
    assert!(
        second_row_y > first_row_y + f32::from(narrow[0].size.height) - 0.5,
        "narrow form puts three presets on a second row: {first_row_y} vs {second_row_y}"
    );
    click_in_form(&mut visual, &view, "bitrate-1");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.prefs.bitrate, 32)
    });
}

/// GPUI exposes no glyph bounds to the test, so the rendered check asserts the
/// row structure at the two real card sizes; the conservative cell budget is
/// asserted against the metric in
/// `bitrate_one_row_threshold_uses_the_padded_widest_label` (the painted row
/// also loses a few px to the card border and the form scrollbar). At the 680 px
/// design card (600 px inside) six presets share one row; at a 620 px card
/// (540 px inside) they fall back to 3 + 3, while the selectors (threshold 540)
/// stay side by side.
#[gpui::test]
async fn bitrate_row_uses_the_true_inner_width_at_680_and_620(cx: &mut TestAppContext) {
    const IDS: [&str; 6] = [
        "bitrate-0",
        "bitrate-1",
        "bitrate-2",
        "bitrate-3",
        "bitrate-4",
        "bitrate-5",
    ];

    let (visual, view) = voice_tab(cx);
    // The test window reports scale 2.0; pin the logical scale so the viewport
    // numbers below map to the pure `settings_card_metrics` assertions.
    visual.update(|_, app| view.update(app, |v, _| v.test_pin_ui_scale(1.0)));

    // A 1000 px viewport paints the 680 px card: 600 px inside, one row.
    resize(visual, 1000., 800.);
    let one_row: Vec<_> = IDS
        .iter()
        .map(|id| {
            visual
                .debug_bounds(*id)
                .unwrap_or_else(|| panic!("{id} is painted"))
        })
        .collect();
    let row_y = f32::from(one_row[0].origin.y);
    for (index, bounds) in one_row.iter().enumerate() {
        assert!(
            (f32::from(bounds.origin.y) - row_y).abs() < 0.5,
            "680 px card keeps preset {index} on the single row"
        );
    }

    // A 636 px viewport is the 620 px card: 540 px inside, below the 598 px
    // one-row threshold but still wide enough for two device selectors.
    resize(visual, 636., 800.);
    let compact: Vec<_> = IDS
        .iter()
        .map(|id| {
            visual
                .debug_bounds(*id)
                .unwrap_or_else(|| panic!("{id} is still painted"))
        })
        .collect();
    let first_row_y = f32::from(compact[0].origin.y);
    let second_row_y = f32::from(compact[3].origin.y);
    for bounds in &compact[..3] {
        assert!(
            (f32::from(bounds.origin.y) - first_row_y).abs() < 0.5,
            "620 px card keeps the first three presets on one row"
        );
    }
    for bounds in &compact[3..] {
        assert!(
            (f32::from(bounds.origin.y) - second_row_y).abs() < 0.5,
            "620 px card puts the last three presets on the second row"
        );
    }
    assert!(
        second_row_y > first_row_y + f32::from(compact[0].size.height) - 0.5,
        "620 px card uses 3 + 3: {first_row_y} vs {second_row_y}"
    );

    let input = visual
        .debug_bounds("audio-in-selector")
        .expect("the input selector is painted");
    let output = visual
        .debug_bounds("audio-out-selector")
        .expect("the output selector is painted");
    assert!(
        f32::from(output.origin.y) < f32::from(input.origin.y + input.size.height) - 0.5,
        "the 540 px inner width keeps the selectors side by side: {input:?} / {output:?}"
    );
}

#[gpui::test]
async fn diagnostics_are_collapsed_by_default_and_toggle_reveals_capability_rows(
    cx: &mut TestAppContext,
) {
    const DIAGNOSTICS: [&str; 8] = [
        "voice-capture-note",
        "cap-echo",
        "cap-noise",
        "cap-gain",
        "cap-gate",
        "cap-output",
        "cap-bitrate",
        "voice-chain-note",
    ];
    let (mut visual, view) = voice_tab(cx);
    view.read_with(&*visual, |v, _| {
        assert!(
            !v.voice_settings.diagnostics_open,
            "«Подробнее» starts collapsed"
        )
    });
    for id in DIAGNOSTICS {
        assert!(
            visual.debug_bounds(id).is_none(),
            "{id} must not be painted while the section is collapsed"
        );
    }
    // The real processing and hotkey controls stay outside the collapsible
    // block and must be painted without opening it.
    assert!(visual.debug_bounds("noise-mode-0").is_some());
    assert!(visual.debug_bounds("noise-mode-1").is_some());
    assert!(visual.debug_bounds("auto-gain").is_some());
    assert!(visual.debug_bounds("input-mode-voice").is_some());
    assert!(visual.debug_bounds("voice-shortcut-0").is_some());

    click_in_form(&mut visual, &view, "voice-diagnostics-toggle");
    view.read_with(&*visual, |v, _| {
        assert!(v.voice_settings.diagnostics_open);
        assert_eq!(
            v.voice_settings.prefs,
            crate::voice_settings::VoicePrefs::default(),
            "opening diagnostics must not change a saved preference"
        );
    });
    for id in DIAGNOSTICS {
        assert!(
            visual.debug_bounds(id).is_some(),
            "{id} is revealed by «Подробнее»"
        );
    }
}
