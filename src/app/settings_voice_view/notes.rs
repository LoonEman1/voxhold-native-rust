//! Diagnostic note and capability-row painters of the voice settings tab.
//!
//! Both are small view helpers with debug selectors, kept apart from the form
//! builder so `settings_voice_view.rs` stays within its size budget.

use super::*;

impl Voxhold {
    /// A note with a debug selector, so the layout tests can prove that the
    /// diagnostics block reveals and hides the technical text.
    pub(super) fn voice_note(&self, id: &'static str, text: &str, danger: bool) -> AnyElement {
        div()
            .w_full()
            .debug_selector(move || id.to_string())
            .child(self.form_text(text, danger))
            .into_any_element()
    }

    /// One capability row: a label, a short support tag and the honest note.
    pub(super) fn voice_capability(
        &self,
        id: &'static str,
        label: &str,
        support: Support,
        note: &str,
    ) -> AnyElement {
        let p = self.palette;
        let tag = match support {
            Support::Applied => "применяется",
            Support::Automatic => "автоматически",
            Support::Unavailable => "недоступно",
        };
        let tag_color = match support {
            Support::Applied => p.lime,
            Support::Automatic => p.muted,
            Support::Unavailable => p.danger,
        };
        // Tests cannot read painted text; a selector that carries the tag
        // keeps the promise ("применяется" vs "недоступно") assertable.
        let tag_debug = format!("{id}-tag-{tag}");
        div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || id.to_string())
            .w_full()
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_size(px(13.)).child(format!("{label} · ")))
                    .child(
                        div()
                            .debug_selector(move || tag_debug.clone())
                            .text_size(px(12.))
                            .text_color(ink(tag_color))
                            .child(tag.to_owned()),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(note.to_owned()),
            )
            .into_any_element()
    }
}
