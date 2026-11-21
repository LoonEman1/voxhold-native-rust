//! The neutral start surface shown while the one startup check runs.
//!
//! It deliberately has no address, login or password field: on a successful
//! restoration the first interactive surface is the workspace, and on a failed
//! one it is the address form. The only action here, «Выбрать инстанс»,
//! cancels the attempt immediately and is reachable by Tab and Enter/Space.

use super::*;

impl Voxhold {
    pub(super) fn render_startup(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (vw, vh) = self.viewport;
        let p = self.palette;
        let l = auth::layout_auth(vw, vh, true, false);
        let compact = vw <= 760.0;
        let background = self.auth_background(vw.round() as u32, vh.round() as u32);
        let mut root = div()
            .relative()
            .size_full()
            .font_family(theme::UI_FONT)
            .text_color(ink(p.fg))
            .bg(ink(p.page_base))
            .debug_selector(|| "auth-restore".into())
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(0.))
                    .w(px(vw))
                    .h(px(vh))
                    .child(gpui::img(background).size_full()),
            );
        if l.story > 0.0 {
            let story = auth::story_layout(vw, vh);
            root = root.child(self.auth_story(story, vw, cx));
        }
        // Card and theme toggle keep the auth page's geometry, so switching to
        // the manual form is not a layout jump.
        root = root
            .child(
                div()
                    .absolute()
                    .left(px(l.card.x))
                    .top(px(l.card.y))
                    .w(px(l.card.w))
                    .h(px(l.card.h))
                    .rounded(px(l.radius))
                    .bg(ink(p.card))
                    .border_1()
                    .border_color(ink(p.card_border)),
            )
            .child(
                div()
                    .id("auth-theme")
                    .absolute()
                    .left(px(l.toggle.x))
                    .top(px(l.toggle.y))
                    .w(px(l.toggle.w))
                    .h(px(l.toggle.h))
                    .rounded(px(8.))
                    .bg(ink(p.toggle_bg))
                    .border_1()
                    .border_color(ink(p.toggle_border))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(16.))
                    .text_color(ink(p.toggle_ink))
                    .on_click(cx.listener(|s, _, window, cx| s.toggle_theme(window, cx)))
                    .child(icon(
                        if p.light {
                            "icons/moon.svg"
                        } else {
                            "icons/sun.svg"
                        },
                        16.,
                        ink(p.toggle_ink),
                    )),
            );
        if l.story == 0.0 {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.brand.x))
                    .top(px(l.brand.y))
                    .w(px(39.))
                    .h(px(39.))
                    .rounded(px(11.))
                    .bg(ink(p.lime))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(20.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(0x10100f))
                    .child("V"),
            );
        }
        let inner = l.inner;
        let font = if compact { 30.0 } else { 34.0 };
        root = root
            .child(
                div()
                    .absolute()
                    .left(px(inner.x))
                    .top(px(inner.y + 4.0))
                    .w(px(inner.w))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.story_accent))
                    .child("ПОДКЛЮЧЕНИЕ"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(inner.x))
                    .top(px(inner.y + 34.0))
                    .w(px(inner.w))
                    .text_size(px(font))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Восстанавливаем вход…"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(inner.x))
                    .top(px(inner.y + 88.0))
                    .w(px(inner.w))
                    .text_size(px(15.))
                    .text_color(ink(p.muted))
                    .child("Проверяем сохранённый вход. Это займёт несколько секунд."),
            );
        let action = l.primary;
        root = root.child(
            div()
                .id("auth-restore-cancel")
                .debug_selector(|| "auth-restore-cancel".into())
                .absolute()
                .left(px(action.x))
                .top(px(inner.y + 142.0))
                .w(px(if action.w > 260.0 { 260.0 } else { action.w }))
                .h(px(44.))
                .rounded(px(10.))
                .bg(ink(p.surface))
                .border_1()
                .border_color(ink(p.line))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .track_focus(&self.restore_cancel_focus)
                .tab_index(0)
                .text_size(px(14.))
                .on_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, window, cx| {
                    let key = &event.keystroke;
                    if key.key == "enter" || key.key == "space" {
                        s.cancel_startup_restore(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_click(cx.listener(|s, _, window, cx| s.cancel_startup_restore(window, cx)))
                .child("Выбрать инстанс"),
        );
        if !self.restore_focus_done {
            self.restore_focus_done = true;
            // The action is mounted by the time the next frame runs.
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |s, _cx| {
                        s.restore_cancel_focus.focus(window);
                    });
                }
            });
        }
        root.into_any_element()
    }
}
