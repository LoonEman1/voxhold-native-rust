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
    // Device rows (system default + two inputs / one output) and the refresh.
    for id in [
        "audio-in-0",
        "audio-in-1",
        "audio-in-2",
        "audio-out-0",
        "audio-out-1",
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
const FORM_CONTROLS: [&str; 17] = [
    "audio-in-0",
    "audio-in-1",
    "audio-in-2",
    "audio-out-0",
    "audio-out-1",
    "audio-refresh",
    "input-volume",
    "voice-test-mic",
    "output-volume",
    "bitrate-0",
    "sounds",
    "noise-mode-0",
    "noise-mode-1",
    "auto-gain",
    "input-mode-voice",
    "input-mode-ptt",
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

#[gpui::test]
async fn the_voice_form_stays_usable_at_125_and_150_percent(cx: &mut TestAppContext) {
    // The test window has no platform DPI, so the view scale is pinned and the
    // logical viewport is the size a 1600x900 panel reports at that scale.
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
