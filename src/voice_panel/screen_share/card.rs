//! On-air card of the local screen publication. It reads the same snapshot as
//! the picker and paints the real audio scope of the running attempt.

use super::*;
use voxhold_media::screen_capture::MAX_PREVIEW_WIDTH;

/// Physical width of the local preview requested by the painted card.
///
/// Pure, so the visibility, minimize, zero-rect and DPI rules are tested
/// without a window. The published stream never depends on this value: the
/// capture keeps its target, frame rate and I420 conversion, and only the
/// extra BGRA picture for the card follows the request.
pub(super) fn preview_request(
    active: bool,
    minimized: bool,
    rect: Option<Rect>,
    scale: f32,
) -> Option<u32> {
    if !active || minimized {
        return None;
    }
    let rect = rect?;
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return None;
    }
    // `set_scale` clamps the same way; a zero or negative scale is never
    // treated as "smaller than a pixel".
    let physical = (rect.w * scale.max(0.1)).ceil();
    if !physical.is_finite() || physical < 1.0 {
        return None;
    }
    Some((physical.min(MAX_PREVIEW_WIDTH as f32) as u32).clamp(2, MAX_PREVIEW_WIDTH))
}

impl VoicePanel {
    /// Whether the local on-air card takes one room slot.
    pub(in crate::voice_panel) fn screen_card_active(&self) -> bool {
        matches!(
            self.screen.sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping | Phase::Failed
        )
    }

    /// Hand the painted card's preview request to the media layer.
    ///
    /// Runs on the same 100 ms poll that takes a frame, so the request follows
    /// the last painted card instead of a value an old frame computed. Only
    /// atomics are written; the UI thread never waits for the capture worker,
    /// and no command is queued. No handle means no capture and nothing to
    /// disable.
    pub(in crate::voice_panel) fn sync_screen_preview_demand(&self, window: &Window) {
        let Some(handle) = &self.handle else {
            return;
        };
        let request = preview_request(
            // `room_painted` is the shell's own predicate for the voice room
            // pane of this frame, so a card in a hidden pane requests nothing
            // even while the publication keeps running.
            self.room_painted && self.screen_card_active(),
            crate::window_state::is_minimized(window),
            self.screen_card_rect,
            window.scale_factor(),
        );
        handle.set_screen_preview_demand(request);
    }

    /// The card that replaces the local publication in the room grid while a
    /// screen is on air. `None` when nothing is being published.
    pub(in crate::voice_panel) fn screen_card(
        &mut self,
        rect: Rect,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let sharing = self.screen.sharing.clone();
        if !matches!(
            sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping | Phase::Failed
        ) {
            self.screen_card_rect = None;
            return None;
        }
        // The demand is computed from the rect this card is really painted at;
        // a zero-sized card (a collapsed room pane) requests nothing.
        self.screen_card_rect = Some(rect);
        let c = self.colors();
        let live = sharing.phase == Phase::Live;
        let status = if live {
            // The publisher reports what really happened: a live bitrate
            // change, a running restart preflight or the plain on-air state.
            if sharing.status.is_empty() {
                "В ЭФИРЕ".to_owned()
            } else {
                sharing.status.clone()
            }
        } else if sharing.phase == Phase::Failed {
            "ОШИБКА".to_owned()
        } else if sharing.phase == Phase::Stopping {
            "ОСТАНОВКА…".to_owned()
        } else {
            sharing.status.clone()
        };
        let stats = sharing.stats.map(|stats| {
            // FPS, drop, real resolution, limitation and source count come
            // from OutboundRtpStats; this build has no GPU/CPU percentage.
            stats.describe()
        });
        let quality = self.screen.quality_label();
        let sound = self.screen.sound_label();
        // One shared helper builds the line for the card and the test hook, so
        // a painted-card test checks the real rendering rather than a copy.
        let line = screen_on_air_line(&sharing, &quality, &sound, stats.as_deref());
        // GPUI's test context has no text query. The exact painted line is a
        // debug selector on the text box, so a painted-card test can prove that
        // the scope is really in the card. The inner line keeps the stable
        // `screen-on-air-quality` key used by layout tests.
        let selector_line = line.clone();
        let title = self
            .room_streams()
            .iter()
            .find(|item| {
                live && Some(&item.publisher_connection_id) == self.self_connection.as_ref()
            })
            .map(|publication| voice_room::live_metadata(publication, voice_room::now_millis()))
            .unwrap_or_else(|| "Ваш экран".into());
        let mut card = div()
            .id("screen-on-air")
            .debug_selector(|| "screen-on-air".into())
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(12.))
            .bg(rgb(c.surface))
            .border_1()
            .border_color(rgb(if live { c.danger } else { c.line }))
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top(px(12.))
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(5.))
                    .bg(rgb(c.live_bg))
                    .text_size(px(10.))
                    .text_color(rgb(c.danger))
                    .child(if live { "LIVE" } else { "ВАШ ЭКРАН" }),
            )
            .child(
                div()
                    .absolute()
                    .right(px(12.))
                    .top(px(12.))
                    .text_size(px(10.))
                    .text_color(rgb(c.muted))
                    .truncate()
                    .child(status),
            )
            .child(
                div()
                    .absolute()
                    .left(px(10.))
                    .top(px(34.))
                    .w(px((rect.w - 20.).max(1.)))
                    .h(px((rect.h - 108.).max(1.)))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .bg(rgb(0x080808))
                    .when_some(self.screen.preview.clone(), |picture, image| {
                        picture.child(
                            img(image)
                                .w_full()
                                .h_full()
                                .object_fit(gpui::ObjectFit::Contain),
                        )
                    }),
            )
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top(px(rect.h - 66.))
                    .w(px((rect.w - 24.).max(0.)))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .truncate()
                    .child(title),
            )
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top(px(rect.h - 50.))
                    .w(px((rect.w - 24.).max(0.)))
                    .debug_selector(move || format!("screen-on-air-line:{selector_line}").into())
                    .child(
                        div()
                            .w_full()
                            .text_size(px(10.))
                            .text_color(rgb(c.muted))
                            .truncate()
                            .debug_selector(|| "screen-on-air-quality".into())
                            .child(line),
                    ),
            );
        if sharing.phase != Phase::Stopping {
            let mut actions = div()
                .absolute()
                .left(px(10.))
                .top(px(rect.h - 36.))
                .w(px((rect.w - 20.).max(0.)))
                .h(px(28.))
                .flex()
                .gap_1();
            // The dialog only applies changes while the stream is Live; in
            // Publishing the action would be inert, so it is not shown.
            if sharing.phase == Phase::Live {
                let quality_focus = self.screen_focus[2].clone();
                actions = actions.child(
                    div()
                        .id("screen-quality")
                        .debug_selector(|| "screen-quality".into())
                        .flex_1()
                        .h_full()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(c.line))
                        .text_color(rgb(c.fg))
                        .text_size(px(11.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .track_focus(&quality_focus)
                        .on_click(cx.listener(|panel, _, window, cx| {
                            panel.open_screen_settings(window, cx)
                        }))
                        .child("Качество"),
                );
            }
            let stop_focus = self.screen_focus[0].clone();
            actions = actions.child(
                div()
                    .id("screen-stop")
                    .debug_selector(|| "screen-stop".into())
                    .flex_1()
                    .h_full()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(c.danger))
                    .text_color(rgb(c.danger))
                    .text_size(px(11.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&stop_focus)
                    .on_click(
                        cx.listener(|panel, _, window, cx| panel.stop_screen_share(window, cx)),
                    )
                    .child("Завершить"),
            );
            card = card.child(actions);
        }
        Some(card.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::preview_request;
    use crate::voice_room::Rect;
    use voxhold_media::screen_capture::MAX_PREVIEW_WIDTH;

    fn card() -> Rect {
        Rect::new(0.0, 0.0, 320.0, 180.0)
    }

    #[test]
    fn a_hidden_or_idle_card_requests_nothing() {
        assert_eq!(preview_request(false, false, Some(card()), 1.0), None);
        assert_eq!(preview_request(false, false, None, 1.0), None);
    }

    #[test]
    fn a_minimized_window_requests_nothing() {
        assert_eq!(preview_request(true, true, Some(card()), 1.0), None);
    }

    #[test]
    fn a_zero_sized_card_requests_nothing() {
        assert_eq!(preview_request(true, false, None, 1.0), None);
        let collapsed = Rect::new(0.0, 0.0, 0.0, 180.0);
        assert_eq!(preview_request(true, false, Some(collapsed), 1.0), None);
        let flat = Rect::new(0.0, 0.0, 320.0, 0.0);
        assert_eq!(preview_request(true, false, Some(flat), 1.0), None);
    }

    #[test]
    fn the_request_follows_the_physical_card_width() {
        assert_eq!(
            preview_request(true, false, Some(card()), 1.0),
            Some(320),
            "100% DPI asks for the logical width"
        );
        assert_eq!(
            preview_request(true, false, Some(card()), 1.5),
            Some(480),
            "a fractional DPI is rounded up"
        );
        assert_eq!(preview_request(true, false, Some(card()), 1.25), Some(400));
        // A scale below the clamp is not treated as a sub-pixel card.
        assert_eq!(
            preview_request(true, false, Some(Rect::new(0.0, 0.0, 3.0, 3.0)), 0.0),
            Some(2)
        );
    }

    #[test]
    fn the_request_is_clamped_to_the_store_bound() {
        let wide = Rect::new(0.0, 0.0, 4000.0, 2000.0);
        assert_eq!(
            preview_request(true, false, Some(wide), 2.0),
            Some(MAX_PREVIEW_WIDTH)
        );
        let tiny = Rect::new(0.0, 0.0, 1.0, 1.0);
        assert_eq!(
            preview_request(true, false, Some(tiny), 1.0),
            Some(2),
            "the media layer never gets an unusable width"
        );
    }
}
