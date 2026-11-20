use gpui::{div, prelude::*, px, rgba, AnyElement};

pub(crate) fn ink(value: u32) -> gpui::Rgba {
    rgba((value << 8) | 0xff)
}

pub(crate) fn ink_alpha(value: u32, alpha: u8) -> gpui::Rgba {
    rgba((value << 8) | u32::from(alpha))
}

/// One icon from the shipped `assets/` directory. SVGs use `currentColor`, so
/// the caller controls the tint through `text_color`.
pub(crate) fn icon(resource: &'static str, size: f32, color: gpui::Rgba) -> AnyElement {
    gpui::svg()
        .path(resource)
        .w(px(size))
        .h(px(size))
        .text_color(color)
        .into_any_element()
}

/// A centered history divider: the line is masked by the label, matching Go's
/// date separator and the unread boundary.
pub(crate) fn history_divider(
    label: &str,
    accent: u32,
    line: u32,
    bg: u32,
    bold: bool,
) -> AnyElement {
    div()
        .relative()
        .w_full()
        .h(px(36.))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .absolute()
                .left(px(20.))
                .right(px(20.))
                .h(px(1.))
                .bg(ink(line)),
        )
        .child(
            div()
                .px(px(12.))
                .bg(ink(bg))
                .text_size(px(12.))
                .text_color(ink(accent))
                .when(bold, |text| text.font_weight(gpui::FontWeight::BOLD))
                .child(label.to_string()),
        )
        .into_any_element()
}

pub(crate) fn channel_icon(kind: &str, color: gpui::Rgba) -> AnyElement {
    let resource = match kind {
        "voice" => "icons/volume.svg",
        "news" => "icons/news.svg",
        _ => "icons/hash.svg",
    };
    icon(resource, 15., color)
}
