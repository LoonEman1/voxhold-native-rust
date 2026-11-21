//! Pickable source card and the pure geometry of the picker grid.
//!
//! The picture slot is deliberately independent of the text footer and of the
//! scroll viewport. GPUI derives an image's aspect ratio from its intrinsic
//! size (`gpui::elements::img` sets `style.aspect_ratio`), so a percentage
//! sized `img` inside a short slot can lay out taller than the slot and be
//! cropped by its mask. This module therefore computes the slot and the
//! painted image size explicitly and paints exactly one centred image.

use super::*;
use voxhold_media::screen_capture::MAX_THUMBNAIL_WIDTH;

/// Dialog padding on both sides. The available inner width below uses it.
pub(super) const PICKER_PADDING: f32 = 18.;
/// Gap between cards and between grid rows.
pub(super) const PICKER_GAP: f32 = 8.;
/// A card is not narrower than this at the two-column threshold.
pub(super) const CARD_MIN_WIDTH: f32 = 260.;
/// One card border; the picture slot uses the inner width.
pub(super) const CARD_BORDER: f32 = 1.;
/// The slot keeps the source aspect container at 16:9 before the height cap.
pub(super) const SLOT_ASPECT: f32 = 16. / 9.;
/// The picture slot never grows taller than this.
pub(super) const SLOT_MAX_HEIGHT: f32 = 160.;
/// Title + size + padding of a card without a preview error.
pub(super) const CARD_FOOTER_HEIGHT: f32 = 46.;
/// The same footer with the two-line preview reason.
pub(super) const CARD_FOOTER_FAILURE_HEIGHT: f32 = 74.;
/// The grid is a separate scroll area with this preferred height.
pub(super) const LIST_MAX_HEIGHT: f32 = 280.;
/// At a very small window the grid still scrolls instead of disappearing.
pub(super) const LIST_MIN_HEIGHT: f32 = 80.;
/// Everything in the dialog except the source grid: padding, header, tabs,
/// presets, quality rows, footer and the vertical gaps. Keeping the grid
/// height derived from the dialog height leaves the actions visible in a
/// compact window.
pub(super) const DIALOG_CHROME: f32 = 390.;

/// One card width for the available inner width, never below one column.
pub(super) fn picker_columns(inner_width: f32) -> usize {
    if inner_width >= CARD_MIN_WIDTH * 2. + PICKER_GAP {
        2
    } else {
        1
    }
}

pub(super) fn card_width(columns: usize, inner_width: f32) -> f32 {
    let inner_width = inner_width.max(CARD_MIN_WIDTH);
    if columns >= 2 {
        ((inner_width - PICKER_GAP) / 2.).max(CARD_MIN_WIDTH)
    } else {
        inner_width
    }
}

/// Slot size in logical pixels. The width is the card's inner width; the
/// height is 16:9 of it, capped at [`SLOT_MAX_HEIGHT`].
pub(super) fn picture_slot(card_width: f32) -> (f32, f32) {
    let width = (card_width - CARD_BORDER * 2.).max(1.);
    (width, (width / SLOT_ASPECT).min(SLOT_MAX_HEIGHT).max(1.))
}

/// `min(slot_w / frame_w, slot_h / frame_h)` with rounding kept only at the
/// final pixel write. Both axes use the same scale, so the frame is never
/// stretched and every edge stays inside the slot.
pub(super) fn fitted_frame(frame_w: u32, frame_h: u32, slot_w: f32, slot_h: f32) -> (f32, f32) {
    if frame_w == 0 || frame_h == 0 || slot_w <= 0. || slot_h <= 0. {
        return (0., 0.);
    }
    let scale = (slot_w / frame_w as f32).min(slot_h / frame_h as f32);
    (
        (frame_w as f32 * scale).max(1.),
        (frame_h as f32 * scale).max(1.),
    )
}

/// Height of one card. The footer is reserved explicitly, so a late error
/// line cannot push the size line out of the fixed card.
pub(super) fn card_height(card_width: f32, with_failure: bool) -> f32 {
    let (_, slot_height) = picture_slot(card_width);
    slot_height
        + if with_failure {
            CARD_FOOTER_FAILURE_HEIGHT
        } else {
            CARD_FOOTER_HEIGHT
        }
        + CARD_BORDER * 2.
}

/// The grid keeps a whole normal row when the dialog has its usual height and
/// scrolls in a compact window; the action row is never inside this budget.
pub(super) fn list_height(dialog_height: f32) -> f32 {
    (dialog_height - DIALOG_CHROME).clamp(LIST_MIN_HEIGHT, LIST_MAX_HEIGHT)
}

/// Physical width to request for a card. The media layer clamps it to
/// [`MAX_THUMBNAIL_WIDTH`] and caps the height at [`MAX_THUMBNAIL_HEIGHT`], so
/// this is only the scale-aware upper request.
pub(super) fn thumbnail_request_width(card_width: f32, scale: f32) -> u32 {
    let width = (card_width.max(1.) * scale.max(1.)).ceil();
    (width as u32).clamp(2, MAX_THUMBNAIL_WIDTH)
}

/// A tooltip view for the full source title. GPUI paints it above the dialog,
/// so the scroll viewport and the card mask cannot crop it.
struct SourceTitleTooltip {
    title: String,
    colors: VoiceColors,
}

impl Render for SourceTitleTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.colors;
        div()
            .debug_selector(|| "share-title-tooltip".into())
            .max_w(px(360.))
            .p(px(8.))
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(c.line))
            .bg(rgb(c.surface))
            .text_size(px(11.))
            .line_height(px(15.))
            .text_color(rgb(c.fg))
            .child(self.title.clone())
    }
}

impl VoicePanel {
    /// One picker card. The root id, the click and the focus handle all use the
    /// stable `kind + id`, never a grid position, so a reorder keeps both the
    /// selection and the keyboard focus on the same source.
    pub(super) fn source_card(
        &self,
        source: &ScreenSource,
        id: SourceId,
        selected: bool,
        enabled: bool,
        focus: Option<FocusHandle>,
        card_width: f32,
        locked: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let key = ScreenShareUi::source_kind_name(id);
        let identity = source.identity();
        let thumb = identity
            .as_ref()
            .and_then(|identity| self.screen.previews.get(identity));
        let failure = identity
            .as_ref()
            .and_then(|identity| self.screen.preview_failures.get(identity));
        let size = if source.width > 0 && source.height > 0 {
            format!("{}×{}", source.width, source.height)
        } else {
            "Размер неизвестен".into()
        };
        let card_selector = format!("share-source-card-{key}-{}", id.id);
        let slot_selector = format!("share-slot-{key}-{}", id.id);
        let thumb_selector = format!("share-thumb-{key}-{}", id.id);
        let placeholder_selector = format!("share-thumb-placeholder-{key}-{}", id.id);
        let error_selector = format!("share-thumb-error-{key}-{}", id.id);
        let title_selector = format!("share-title-{key}-{}", id.id);
        let size_selector = format!("share-size-{key}-{}", id.id);
        let failure_selector = format!("share-thumb-error-line-{key}-{}", id.id);
        let selected_selector = format!("share-selected-{key}-{}", id.id);

        // The slot is a real box with a definite size. The card footer starts
        // below it and cannot scroll the picture away.
        let (slot_w, slot_h) = picture_slot(card_width);
        let mut slot = div()
            .debug_selector(move || slot_selector.clone())
            .w(px(slot_w))
            .h(px(slot_h))
            .flex_shrink_0()
            .overflow_hidden()
            .bg(rgb(0x080808))
            .flex()
            .items_center()
            .justify_center();
        if let Some(thumb) = thumb {
            let (image_w, image_h) = fitted_frame(thumb.width, thumb.height, slot_w, slot_h);
            if image_w > 0. && image_h > 0. {
                slot = slot.child(
                    img(thumb.image.clone())
                        .debug_selector(move || thumb_selector.clone())
                        // Explicit final size: `Contain` inside an
                        // intrinsically aspect-sized img used to be larger
                        // than the slot and lost its edges to the mask.
                        .w(px(image_w))
                        .h(px(image_h))
                        .object_fit(gpui::ObjectFit::Contain),
                );
            }
        } else if failure.is_some() {
            slot = slot.child(
                div()
                    .debug_selector(move || error_selector.clone())
                    .px(px(8.))
                    .text_size(px(10.))
                    .text_color(rgb(c.danger))
                    .child("Превью недоступно"),
            );
        } else {
            slot = slot.child(
                div()
                    .debug_selector(move || placeholder_selector.clone())
                    .px(px(8.))
                    .text_size(px(10.))
                    .text_color(rgb(c.muted))
                    .child(if locked {
                        "Текущий источник"
                    } else {
                        "Превью…"
                    }),
            );
        }

        // The old footer had no width of its own. Its `truncate()` children
        // then measured against a content-sized block, which is why even the
        // short monitor label became an ellipsis on the screenshots. The
        // footer and both text rows now take the measured inner card width.
        let title_text = source.title.clone();
        let footer_height = if failure.is_some() {
            CARD_FOOTER_FAILURE_HEIGHT
        } else {
            CARD_FOOTER_HEIGHT
        };
        let tooltip_colors = c;
        let tooltip_title = source.title.clone();
        let title = div()
            // A tooltip is a stateful interaction in GPUI 0.2.2, so the title
            // needs its own element id in addition to the debug selector.
            .id(gpui::ElementId::from(("share-title", id.id)))
            .debug_selector(move || title_selector.clone())
            .w_full()
            .min_w(px(0.))
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::BOLD)
            .truncate()
            .tooltip(move |_window, cx| {
                cx.new(|_| SourceTitleTooltip {
                    title: tooltip_title.clone(),
                    colors: tooltip_colors,
                })
                .into()
            })
            .child(title_text);
        let size_line = div()
            .debug_selector(move || size_selector.clone())
            .w_full()
            .min_w(px(0.))
            .text_size(px(9.))
            .text_color(rgb(c.muted))
            .truncate()
            .child(size);
        let mut footer = div()
            .w_full()
            .h(px(footer_height))
            .flex_shrink_0()
            .p(px(6.))
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(title)
            .child(size_line);
        if let Some(message) = failure {
            footer = footer.child(
                div()
                    .debug_selector(move || failure_selector.clone())
                    .w_full()
                    .min_w(px(0.))
                    .text_size(px(9.))
                    .line_height(px(12.))
                    .text_color(rgb(c.danger))
                    .line_clamp(2)
                    .child(message.clone()),
            );
        }

        let height = card_height(card_width, failure.is_some());
        let mut card = div()
            .id(Self::source_element_id(source))
            .debug_selector(move || card_selector.clone())
            .w(px(card_width))
            .h(px(height))
            .flex_shrink_0()
            .relative()
            .overflow_hidden()
            .rounded(px(9.))
            .border_1()
            .border_color(rgb(if selected { c.accent_text } else { c.line }))
            .bg(rgb(if selected { c.avatar_bg } else { c.surface }))
            .flex()
            .flex_col();
        if let Some(focus) = focus {
            card = card.track_focus(&focus);
        }
        if enabled {
            card = card
                .cursor_pointer()
                .on_click(cx.listener(move |panel, _, _, cx| panel.select_screen_source(id, cx)));
        }
        if selected {
            card = card.child(
                div()
                    .debug_selector(move || selected_selector.clone())
                    .absolute()
                    .top(px(5.))
                    .right(px(5.))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(rgb(c.accent))
                    .text_color(rgb(c.ink))
                    .text_size(px(8.))
                    .child("ВЫБРАНО"),
            );
        }
        card.child(slot).child(footer).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_threshold_is_derived_from_a_measurable_card_width() {
        assert_eq!(picker_columns(2. * CARD_MIN_WIDTH + PICKER_GAP - 1.), 1);
        assert_eq!(picker_columns(2. * CARD_MIN_WIDTH + PICKER_GAP), 2);
        assert_eq!(picker_columns(584.), 2);
        assert_eq!(picker_columns(482.), 1);
        assert!((card_width(2, 584.) - 288.).abs() < 0.01);
        assert!((card_width(1, 482.) - 482.).abs() < 0.01);
    }

    #[test]
    fn the_picture_slot_is_sixteen_by_nine_but_never_taller_than_160() {
        assert!((picture_slot(288.).1 - 160.).abs() < 0.01);
        assert!((picture_slot(250.).1 - (248. / SLOT_ASPECT)).abs() < 0.01);
        assert!((picture_slot(452.).1 - 160.).abs() < 0.01);
    }

    #[test]
    fn a_frame_fits_with_one_scale_and_centred_margins() {
        let (slot_w, slot_h) = picture_slot(288.);
        let (w, h) = fitted_frame(320, 180, slot_w, slot_h);
        assert!(w <= slot_w + 0.01 && h <= slot_h + 0.01);
        assert!((w / h - 16. / 9.).abs() < 0.01);
        let (pw, ph) = fitted_frame(90, 160, slot_w, slot_h);
        assert!(pw <= slot_w && ph <= slot_h);
        assert!((pw / ph - 9. / 16.).abs() < 0.01);
        let (uw, uh) = fitted_frame(420, 180, slot_w, slot_h);
        assert!(uw <= slot_w && uh <= slot_h);
        assert!((uw / uh - 21. / 9.).abs() < 0.01);
        assert_eq!(fitted_frame(0, 180, slot_w, slot_h), (0., 0.));
    }

    #[test]
    fn the_requested_thumbnail_covers_the_card_at_125_and_150_percent() {
        assert_eq!(thumbnail_request_width(288., 1.), 288);
        assert_eq!(thumbnail_request_width(288., 1.25), 360);
        assert_eq!(thumbnail_request_width(288., 1.5), 432);
        assert_eq!(thumbnail_request_width(452., 1.5), 480);
        assert!(thumbnail_request_width(288., 1.5) <= MAX_THUMBNAIL_WIDTH);
        assert!(voxhold_media::screen_capture::MAX_THUMBNAIL_HEIGHT >= 270);
    }

    #[test]
    fn a_compact_dialog_keeps_a_scrolling_grid_and_the_action_row() {
        assert_eq!(list_height(660.), 660. - DIALOG_CHROME);
        assert_eq!(list_height(320.), LIST_MIN_HEIGHT);
        let card = card_height(288., true);
        assert!(
            list_height(660.) >= card,
            "a normal picker shows a whole card, including its error line"
        );
    }
}
