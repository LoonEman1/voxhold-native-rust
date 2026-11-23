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

/// Whether any part of `card` lies inside the clipped room viewport.
///
/// The card grid is painted in an `overflow_hidden` pane that scrolls with
/// `room_scroll`; a card scrolled completely out of that pane still exists in
/// the geometry but the user cannot see it. The preview demand follows only a
/// visible card. A zero-area overlap (edges merely touching) counts as hidden.
pub(super) fn card_in_viewport(card: Rect, viewport: Rect) -> bool {
    card.right() > viewport.x
        && card.x < viewport.right()
        && card.bottom() > viewport.y
        && card.y < viewport.bottom()
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
            // The picture is on screen only while the on-air card of this room
            // or the publisher role of the compact PiP is really painted, so a
            // publication that keeps running elsewhere requests nothing.
            self.screen_card_painted(),
            self.window_minimized(window),
            self.screen_preview_rect(),
            window.scale_factor(),
        );
        handle.set_screen_preview_demand(request);
    }

    /// The same visibility predicate as the preview demand. The take path uses
    /// it too, so a hidden or minimized card discards the prepared picture
    /// instead of creating a `RenderImage` and counting it as shown.
    pub(in crate::voice_panel) fn screen_preview_wanted(&self, window: &Window) -> bool {
        preview_request(
            self.screen_card_painted(),
            self.window_minimized(window),
            self.screen_preview_rect(),
            window.scale_factor(),
        )
        .is_some()
    }

    /// The one foreground predicate of the own publication: the card takes a
    /// grid slot only while the displayed room is both the call room and the
    /// room that owns the publication, and that room pane is really painted.
    /// Every other case is the compact PiP.
    pub(in crate::voice_panel) fn screen_card_in_room(&self) -> bool {
        self.room_painted
            && self.screen_card_active()
            && self.call_channel != 0
            && self.room == self.call_channel
            && self.publisher_room() == self.call_channel
    }

    /// The own publication is floating: it is still owned but its room is not
    /// the grid on screen (another voice room, a text channel or a DM).
    pub(in crate::voice_panel) fn screen_pip_active(&self) -> bool {
        self.screen_card_active() && !self.screen_card_in_room()
    }

    /// Whether the PiP currently paints the publisher role, whose picture is
    /// the same preview slot the on-air card uses.
    pub(in crate::voice_panel) fn screen_pip_publisher_painted(&self) -> bool {
        !self.fullscreen_player()
            && self.screen_pip_active()
            && self.active_pip_role() == crate::voice_panel::pip::PipRole::Publisher
    }

    /// The picture rect the compact PiP paints for the publisher role, derived
    /// from the same pure geometry the overlay uses.
    ///
    /// This deliberately does not depend on `pip_overlay` having run: the shell
    /// builds the overlay while it renders its own tree, before the child panel
    /// paints, so a value stored by the overlay would be reset by the panel's
    /// next `render`. `player_box()` is `None` when the overlay area is empty,
    /// which is exactly when no PiP is painted at all.
    pub(in crate::voice_panel) fn screen_pip_picture_rect(&self) -> Option<Rect> {
        if !self.screen_pip_publisher_painted() {
            return None;
        }
        let player = self.player_box()?;
        Some(voice_room::pip_geometry(player, self.pip_tabs()).picture)
    }

    /// The picture rect of the own publication actually on screen this frame:
    /// the on-air card of the displayed owner room, or the publisher picture of
    /// the compact PiP anywhere else. `None` when neither surface is painted.
    fn screen_preview_rect(&self) -> Option<Rect> {
        if self.screen_card_in_room() {
            // The grid card keeps the rect its last paint stored; a card
            // scrolled out of the viewport stored `None`.
            return self.screen_card_rect;
        }
        self.screen_pip_picture_rect()
    }

    /// The publication picture is on screen: either the on-air card of the
    /// room grid or the publisher role of the compact PiP. The PiP is a
    /// window-level overlay, so it does not depend on the room pane being
    /// painted; only the grid card does.
    fn screen_card_painted(&self) -> bool {
        !self.fullscreen_player()
            && (self.screen_card_in_room() || self.screen_pip_publisher_painted())
    }

    /// The card paints its stop action only while a stop still has something
    /// to do. A stopping attempt paints its status instead, so its handle must
    /// not be a Tab stop either.
    pub(in crate::voice_panel) fn screen_card_stop_painted(&self) -> bool {
        self.screen_card_active() && self.screen.sharing.phase != Phase::Stopping
    }

    /// The status the on-air card and the PiP paint for the current attempt.
    pub(in crate::voice_panel) fn screen_card_status(sharing: &Sharing) -> String {
        match sharing.phase {
            Phase::Live => {
                if sharing.status.is_empty() {
                    "В ЭФИРЕ".to_owned()
                } else {
                    sharing.status.clone()
                }
            }
            Phase::Failed => "ОШИБКА".to_owned(),
            Phase::Stopping => "ОСТАНОВКА…".to_owned(),
            _ => sharing.status.clone(),
        }
    }

    /// The card that replaces the local publication in the room grid while a
    /// screen is on air. `None` when nothing is being published.
    pub(in crate::voice_panel) fn screen_card(
        &mut self,
        rect: Rect,
        viewport: Rect,
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
        // a zero-sized card (a collapsed room pane) requests nothing, and a
        // card scrolled completely out of the viewport requests nothing until
        // it is painted again.
        self.screen_card_rect = card_in_viewport(rect, viewport).then_some(rect);
        let c = self.colors();
        let live = sharing.phase == Phase::Live;
        let status = Self::screen_card_status(&sharing);
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
            if sharing.cpu_fallback {
                let cpu_focus = self.screen_focus[1].clone();
                actions = actions.child(
                    div()
                        .id("screen-continue-cpu")
                        .debug_selector(|| "screen-continue-cpu".into())
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
                        .track_focus(&cpu_focus)
                        .on_click(cx.listener(|panel, _, _, cx| panel.continue_screen_on_cpu(cx)))
                        .child("Продолжить на процессоре"),
                );
            }
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
                    .child(if sharing.phase == Phase::Failed {
                        "Закрыть"
                    } else {
                        "Завершить"
                    }),
            );
            card = card.child(actions);
        }
        Some(card.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::{card_in_viewport, preview_request};
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

    #[test]
    fn a_card_outside_the_scrolled_viewport_is_hidden() {
        let viewport = Rect::new(24.0, 46.0, 800.0, 500.0);
        let inside = Rect::new(100.0, 100.0, 320.0, 180.0);
        assert!(card_in_viewport(inside, viewport));
        // Partly visible is still shown and keeps the demand.
        assert!(card_in_viewport(
            Rect::new(viewport.right() - 40.0, 100.0, 320.0, 180.0),
            viewport
        ));
        assert!(card_in_viewport(
            Rect::new(viewport.x, viewport.bottom() - 40.0, 320.0, 180.0),
            viewport
        ));
        // Scrolled fully above, below or sideways: no pixel is visible.
        assert!(!card_in_viewport(
            Rect::new(100.0, viewport.y - 200.0, 320.0, 180.0),
            viewport
        ));
        assert!(!card_in_viewport(
            Rect::new(100.0, viewport.bottom(), 320.0, 180.0),
            viewport
        ));
        assert!(!card_in_viewport(
            Rect::new(viewport.right(), 100.0, 320.0, 180.0),
            viewport
        ));
        assert!(!card_in_viewport(
            Rect::new(viewport.x - 320.0, 100.0, 320.0, 180.0),
            viewport
        ));
        assert!(
            !card_in_viewport(inside, Rect::new(24.0, 46.0, 0.0, 500.0)),
            "a collapsed pane hides every card"
        );
    }
}
