//! Real settings sliders, ported from the Go `settingsSlider`.
//!
//! Pointer drag, a click on the track, `Home`/`End` and the arrow keys all map
//! a position or a key to a value with step 1. The fill is painted against the
//! measured track box, so it follows the flex width instead of a fixed 220 px.

use super::*;
use gpui::{Bounds, ElementId, Pixels};

/// Go `settingsSlider` keeps 8 px of padding on both ends of the box.
const SLIDER_INSET: f32 = 8.0;
/// Painted track height, the row the pointer can grab and the handle radius.
const TRACK_HEIGHT: f32 = 4.0;
const ROW_HEIGHT: f32 = 22.0;
const HANDLE_RADIUS: f32 = 6.0;

/// The measured geometry of one slider, recorded while it is painted. The
/// tests read the same numbers the pointer math uses.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct SliderTrack {
    /// Window-space x of the usable track start (after the 8 px inset).
    pub origin_x: f32,
    /// Usable track width in logical pixels.
    pub width: f32,
    /// Painted fill width: `width * value / max`.
    pub fill: f32,
}

impl SliderTrack {
    fn measure(bounds: Bounds<Pixels>, ratio: f32) -> Self {
        let origin_x = f32::from(bounds.origin.x) + SLIDER_INSET;
        let width = (f32::from(bounds.size.width) - 2.0 * SLIDER_INSET).max(1.0);
        Self {
            origin_x,
            width,
            fill: width * ratio,
        }
    }
}

/// A value from a pointer position inside the measured track. `round` matches
/// the Go `math.Round`, so both ends are reachable exactly.
pub(super) fn slider_value(origin_x: f32, width: f32, max: i64, x: f32) -> i64 {
    if width <= 0.0 || max <= 0 {
        return 0;
    }
    let ratio = ((x - origin_x) / width).clamp(0.0, 1.0);
    (ratio * max as f32).round().clamp(0.0, max as f32) as i64
}

/// The slider the pointer is currently dragging. It survives the repaint that
/// each value change triggers, so a drag across the row keeps its track.
pub(super) struct SliderDrag {
    max: i64,
    value: i64,
    origin_x: f32,
    width: f32,
    action: std::rc::Rc<dyn Fn(i64) -> SettingsAct>,
}

impl Voxhold {
    /// One slider row: the title with the current value, then the track. The
    /// value is stored through `action`, so the caller keeps its own ranges and
    /// the form state stays the single source.
    pub(super) fn settings_slider(
        &mut self,
        id: &str,
        label: &str,
        value: i64,
        max: i64,
        action: impl Fn(i64) -> SettingsAct + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let max = max.max(1);
        let value = value.clamp(0, max);
        let ratio = value as f32 / max as f32;
        let key = id.to_owned();
        let focus = self.focus_id(ElementId::Name(format!("{id}-track").into()), window, cx);
        let focused = focus.is_focused(window);
        // Arrow keys and Home/End reach the row through the dialog ring, so it
        // is a Tab stop like every button.
        self.settings_ring.push((focus.clone(), None));
        self.settings_slider_focus
            .push((key.clone(), focus.clone()));
        let cell = self.slider_tracks.entry(key.clone()).or_default().clone();
        let action: std::rc::Rc<dyn Fn(i64) -> SettingsAct> = std::rc::Rc::new(action);
        let paint_cell = cell.clone();
        let canvas = gpui::canvas(
            move |bounds, _, _| {
                let track = SliderTrack::measure(bounds, ratio);
                paint_cell.set(track);
                track
            },
            move |bounds, track, window, _| {
                let y = f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.0;
                let line = |width: f32, color: gpui::Rgba| {
                    gpui::fill(
                        Bounds::new(
                            gpui::point(px(track.origin_x), px(y - TRACK_HEIGHT / 2.0)),
                            gpui::size(px(width), px(TRACK_HEIGHT)),
                        ),
                        color,
                    )
                    .corner_radii(px(TRACK_HEIGHT / 2.0))
                };
                window.paint_quad(line(track.width, ink(p.selection)));
                if track.fill > 0.0 {
                    window.paint_quad(line(track.fill, ink(p.lime)));
                }
                let handle = gpui::fill(
                    Bounds::new(
                        gpui::point(
                            px(track.origin_x + track.fill - HANDLE_RADIUS),
                            px(y - HANDLE_RADIUS),
                        ),
                        gpui::size(px(HANDLE_RADIUS * 2.0), px(HANDLE_RADIUS * 2.0)),
                    ),
                    ink(p.lime),
                )
                .corner_radii(px(HANDLE_RADIUS));
                window.paint_quad(handle);
            },
        )
        .w_full()
        .h(px(ROW_HEIGHT));

        let down_action = action.clone();
        let down_cell = cell.clone();
        let down_focus = focus.clone();
        let key_action = action.clone();
        let debug_row = key.clone();
        let debug_track = format!("{key}-track");
        let debug_value = format!("{key}-value");
        let id_field = key.clone();
        let id_track = format!("{key}-track");
        div()
            .id(ElementId::Name(id_field.into()))
            .debug_selector(move || debug_row.clone())
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                // Web `voice-range`: label on the left, the percentage on the
                // right. Only the header changes; the track below keeps the
                // measured geometry the pointer math uses.
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(label.to_owned()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(ink(p.lime))
                            .debug_selector(move || debug_value.clone())
                            .child(format!("{value}%")),
                    ),
            )
            .child(
                div()
                    .id(ElementId::Name(id_track.into()))
                    .debug_selector(move || debug_track.clone())
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .rounded(px(6.))
                    .when(focused, |row| {
                        row.border_1()
                            .border_color(super::ui::ink_alpha(p.lime, 128))
                    })
                    .cursor_pointer()
                    .track_focus(&focus)
                    .tab_index(0)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |s, event: &gpui::MouseDownEvent, window, cx| {
                            window.focus(&down_focus);
                            let track = down_cell.get();
                            let next = slider_value(
                                track.origin_x,
                                track.width,
                                max,
                                f32::from(event.position.x),
                            );
                            s.begin_slider_drag(max, next, track, down_action.clone(), window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_key_down(
                        cx.listener(move |s, event: &gpui::KeyDownEvent, window, cx| {
                            let next = match event.keystroke.key.as_str() {
                                "left" | "down" => Some(value - 1),
                                "right" | "up" => Some(value + 1),
                                "home" => Some(0),
                                "end" => Some(max),
                                _ => None,
                            };
                            if let Some(next) = next {
                                s.settings_action(key_action(next.clamp(0, max)), window, cx);
                                cx.stop_propagation();
                            }
                        }),
                    )
                    .child(canvas),
            )
            .into_any_element()
    }

    fn begin_slider_drag(
        &mut self,
        max: i64,
        value: i64,
        track: SliderTrack,
        action: std::rc::Rc<dyn Fn(i64) -> SettingsAct>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.slider_drag = Some(SliderDrag {
            max,
            value,
            origin_x: track.origin_x,
            width: track.width,
            action: action.clone(),
        });
        self.settings_action(action(value), window, cx);
    }

    /// Apply one pointer position to the active drag. The value is derived from
    /// the row geometry each time, so an older position can never win.
    pub(super) fn drag_slider_to(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.slider_drag.as_mut() else {
            return;
        };
        let value = slider_value(drag.origin_x, drag.width, drag.max, x);
        if value == drag.value {
            return;
        }
        drag.value = value;
        let action = drag.action.clone();
        self.settings_action(action(value), window, cx);
    }

    pub(super) fn end_slider_drag(&mut self) {
        self.slider_drag = None;
    }

    /// One pointer move for an active slider drag. The dialog listens to it on
    /// the card (pointer over the form) and on the backdrop (pointer outside
    /// the card), which together reproduce the Go pointer capture without
    /// losing moves at the card edge.
    pub(super) fn slider_pointer_moved(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.slider_drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_slider_drag();
            return;
        }
        self.drag_slider_to(f32::from(event.position.x), window, cx);
    }

    /// The most recent measurement of one slider, for the layout regressions.
    #[cfg(test)]
    pub(super) fn test_slider_track(&self, id: &str) -> Option<SliderTrack> {
        self.slider_tracks.get(id).map(|cell| cell.get())
    }

    /// The focus handle of a drawn slider, so a test can drive Home/End and the
    /// arrow keys through the real key path.
    #[cfg(test)]
    pub(super) fn test_slider_focus(&self, id: &str) -> Option<FocusHandle> {
        self.settings_slider_focus
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, handle)| handle.clone())
    }
}
