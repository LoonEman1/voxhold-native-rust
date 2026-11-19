use super::*;

impl Voxhold {
    pub(super) fn auth_error_row(&self, rect: layout::Rect) -> AnyElement {
        let p = self.palette;
        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(8.))
            .bg(ink(p.error_bg))
            .border_1()
            .border_color(ink(p.error_border))
            .flex()
            .items_center()
            .gap_2()
            .px(px(12.))
            .child(
                div()
                    .text_size(px(16.))
                    .text_color(ink(p.error_text))
                    .child("×"),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .text_color(ink(p.error_text))
                    .child(self.auth.status.clone()),
            )
            .into_any_element()
    }

    pub(super) fn auth_field(
        &mut self,
        entity: &Entity<InputState>,
        rect: layout::Rect,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(9.))
            .bg(ink(p.field_bg))
            .border_1()
            .border_color(ink(p.field_border))
            .flex()
            .items_center()
            .capture_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, window, cx| {
                let key = &event.keystroke;
                if key.key == "enter"
                    && !key.modifiers.shift
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && !key.modifiers.platform
                {
                    // Enter belongs to the form: GPUI would otherwise insert a
                    // newline into a single-line field.
                    if s.auth.is_instance() {
                        s.submit_instance(window, cx);
                    } else {
                        s.submit_login(window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(
                // The wrapper owns the only background and border; the inner
                // input stays transparent so no second plate appears.
                div().w_full().px(px(14.)).child(
                    TextInput::new(entity)
                        .text_size(px(if compact { 17. } else { 16. }))
                        .font_family("Inter")
                        .text_color(ink(p.fg))
                        .bg(gpui::transparent_black())
                        .bordered(false)
                        .h(px(rect.h - 2.0)),
                ),
            )
            .into_any_element()
    }

    pub(super) fn auth_story(
        &self,
        l: auth::StoryLayout,
        vw: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let mut story = div()
            .absolute()
            .left(px(0.))
            .top(px(0.))
            .w(px(if vw > 980.0 { vw * 0.55 } else { vw / 2.0 }))
            .h_full()
            .child(
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
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.brand.x + 50.0))
                    .top(px(l.brand.y))
                    .h(px(39.))
                    .flex()
                    .items_center()
                    .text_size(px(21.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.fg))
                    .child("voxhold"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.tagline.x - 16.0 + 3.0))
                    .top(px(l.tagline.y + 8.0 - 3.5))
                    .w(px(7.))
                    .h(px(7.))
                    .rounded_full()
                    .bg(ink(p.lime)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.tagline.x))
                    .top(px(l.tagline.y))
                    .h(px(l.tagline.h))
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.fg))
                    .child("Голос. Текст. Свои люди."),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.line1.x))
                    .top(px(l.line1.y))
                    .w(px(l.line1.w))
                    .text_size(px(l.font))
                    .line_height(px(l.font * 0.96))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.fg))
                    .child("Место, где"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.line2.x))
                    .top(px(l.line2.y))
                    .w(px(l.line2.w))
                    .text_size(px(l.font))
                    .line_height(px(l.font * 0.96))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.fg))
                    .child("разговоры"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.line3.x))
                    .top(px(l.line3.y))
                    .w(px(l.line3.w))
                    .text_size(px(l.font))
                    .line_height(px(l.font * 0.96))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.story_accent))
                    .child("живут."),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.paragraph.x))
                    .top(px(l.paragraph.y))
                    .w(px(l.paragraph.w))
                    .text_size(px(l.body))
                    .line_height(px(l.body * 1.65))
                    .text_color(ink(p.muted))
                    .child("Без шума и лишнего. Только ваши комнаты, сообщения и люди, с которыми хочется оставаться на связи."),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.footer.x))
                    .top(px(l.footer.y))
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("© 2026 Voxhold"),
            );
        if vw > 980.0 {
            story = story.child(self.auth_preview(l.preview, cx));
        }
        story.into_any_element()
    }

    /// Decorative workspace mock-up used by the wide auth story column. It is
    /// drawn from primitives so no bitmap asset is needed and it follows the
    /// active theme like the reference raster does.
    pub(super) fn auth_preview(&self, r: layout::Rect, _cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let dark = !p.light;
        let panel = if dark { 0x121311 } else { 0xf7f7f2 };
        let rail = if dark { 0x0e0e0d } else { 0xefefe8 };
        let tile = if dark { 0x292925 } else { 0xe3e3da };
        let white = if dark { 0xf1efe8 } else { 0x20211d };
        let grey = if dark { 0xa6a49b } else { 0x62635b };
        let dim = if dark { 0x98978f } else { 0x8a8b81 };
        let violet = if p.light { 0x7c64df } else { 0xa993ff };
        let mut preview = div()
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .h(px(r.h))
            .rounded(px(16.))
            .bg(ink(panel))
            .border_1()
            .border_color(rgba(0xffffff24))
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(0.))
                    .w(px(47.))
                    .h_full()
                    .bg(ink(rail))
                    .border_r_1()
                    .border_color(rgba(0xffffff17)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(192.))
                    .top(px(0.))
                    .w(px(1.))
                    .h_full()
                    .bg(rgba(0xffffff17)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(9.))
                    .top(px(14.))
                    .w(px(30.))
                    .h(px(30.))
                    .rounded(px(9.))
                    .bg(ink(p.lime))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(16.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(0x111111))
                    .child("V"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(208.))
                    .top(px(22.))
                    .w(px(27.))
                    .h(px(27.))
                    .rounded(px(9.))
                    .bg(ink(p.lime)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(208.))
                    .top(px(71.))
                    .w(px(27.))
                    .h(px(27.))
                    .rounded(px(9.))
                    .bg(ink(violet)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(16.))
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(white))
                    .child("Design room"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(50.))
                    .text_size(px(9.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(dim))
                    .child("ТЕКСТОВЫЕ КАНАЛЫ"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(68.))
                    .text_size(px(12.))
                    .text_color(ink(white))
                    .child("# general"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(88.))
                    .text_size(px(12.))
                    .text_color(ink(grey))
                    .child("# inspiration"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(112.))
                    .text_size(px(9.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(dim))
                    .child("ГОЛОСОВЫЕ КАНАЛЫ"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(132.))
                    .text_size(px(12.))
                    .text_color(ink(grey))
                    .child("● coffee break"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(245.))
                    .top(px(24.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(white))
                    .child("mira"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(276.))
                    .top(px(25.))
                    .text_size(px(10.))
                    .text_color(ink(dim))
                    .child("12:42"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(245.))
                    .top(px(43.))
                    .text_size(px(12.))
                    .text_color(ink(grey))
                    .child("Собираемся через пять?"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(245.))
                    .top(px(73.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(white))
                    .child("niko"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(276.))
                    .top(px(74.))
                    .text_size(px(10.))
                    .text_color(ink(dim))
                    .child("12:43"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(245.))
                    .top(px(92.))
                    .text_size(px(12.))
                    .text_color(ink(grey))
                    .child("Уже здесь. Заходите в coffee break"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(208.))
                    .top(px(126.))
                    .text_size(px(12.))
                    .text_color(ink(dim))
                    .child("mira печатает"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(290.))
                    .top(px(126.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.lime))
                    .child("..."),
            );
        for index in 0..3 {
            preview = preview.child(
                div()
                    .absolute()
                    .left(px(10.))
                    .top(px(56. + index as f32 * 39.))
                    .w(px(27.))
                    .h(px(27.))
                    .rounded(px(9.))
                    .bg(ink(tile)),
            );
        }
        preview.into_any_element()
    }

    /// Cached auth backdrop keyed by client area and theme, like the Go
    /// `pageBackground` raster. It is regenerated only when either changes.
    pub(super) fn auth_background(&mut self, w: u32, h: u32) -> std::sync::Arc<gpui::RenderImage> {
        if let Some((cw, ch, light, image)) = &self.auth_background {
            if *cw == w && *ch == h && *light == self.palette.light {
                return image.clone();
            }
        }
        let rgba = background::page_background(w.max(1), h.max(1), self.palette.light);
        let image = std::sync::Arc::new(gpui::RenderImage::new(smallvec::smallvec![
            image::Frame::new(rgba)
        ]));
        self.auth_background = Some((w, h, self.palette.light, image.clone()));
        image
    }

    pub(super) fn render_auth(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (vw, vh) = self.viewport;
        let p = self.palette;
        let instance = self.auth.is_instance();
        let l = auth::layout_auth(vw, vh, instance, !self.auth.status.is_empty());
        let compact = vw <= 760.0;
        let background = self.auth_background(vw.round() as u32, vh.round() as u32);
        let mut root = div()
            .relative()
            .size_full()
            .font_family("Inter")
            .text_color(ink(p.fg))
            .bg(ink(p.page_base))
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
        // Card.
        root = root.child(
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
        );
        // Theme toggle.
        root = root.child(
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
        // Mobile brand above the card when the story column is hidden.
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
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.brand.x + 50.0))
                    .top(px(l.brand.y))
                    .h(px(39.))
                    .flex()
                    .items_center()
                    .text_size(px(21.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("voxhold"),
            );
        }
        let inner = l.inner;
        if instance {
            root = root
                .child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y))
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
                        .top(px(inner.y + 27.0))
                        .w(px(inner.w))
                        .text_size(px(35.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Адрес инстанса"),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y + 75.0))
                        .w(px(inner.w))
                        .text_size(px(15.))
                        .text_color(ink(p.muted))
                        .child("Укажите адрес сервера Voxhold."),
                );
        } else {
            let font = (vw * 0.03).clamp(32.0, 42.0);
            // Back to the address step. Kept above the card so it does not
            // change the measured heading rows.
            root = root
                .child(
                    div()
                        .id("auth-back")
                        .absolute()
                        .left(px(l.back.x))
                        .top(px(l.back.y))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .on_click(cx.listener(|s, _, window, cx| s.back_to_instance(window, cx)))
                        .child(icon("icons/up.svg", 12., ink(p.muted)))
                        .child("Назад"),
                )
                // The confirmed origin of this step. It is plain text, not a
                // new control: the address is changed through «Назад». Its own
                // measured column starts after the link and wraps, so a long
                // address is fully visible and cannot cover the link or the
                // theme toggle.
                .child(
                    div()
                        .absolute()
                        .left(px(l.origin.x))
                        .top(px(l.origin.y))
                        .w(px(l.origin.w))
                        .line_height(px(15.))
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .child(self.auth.origin.clone()),
                )
                // Every label is positioned in client coordinates directly, not
                // inside another absolutely positioned container.
                .child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y + 4.0))
                        .w(px(inner.w))
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(p.story_accent))
                        .child("ДОБРО ПОЖАЛОВАТЬ"),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y + 34.0))
                        .w(px(inner.w))
                        .text_size(px(font))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("С возвращением"),
                );
            if inner.w < 330.0 {
                root = root
                    .child(
                        div()
                            .absolute()
                            .left(px(inner.x))
                            .top(px(inner.y + 81.0))
                            .w(px(inner.w))
                            .text_size(px(15.))
                            .text_color(ink(p.muted))
                            .child("Войдите в существующий аккаунт"),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(inner.x))
                            .top(px(inner.y + 104.0))
                            .w(px(inner.w))
                            .text_size(px(15.))
                            .text_color(ink(p.muted))
                            .child("Voxhold."),
                    );
            } else {
                root = root.child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y + 81.0))
                        .w(px(inner.w))
                        .text_size(px(15.))
                        .text_color(ink(p.muted))
                        .child("Войдите в существующий аккаунт Voxhold."),
                );
            }
            root = root
                .child(
                    div()
                        .absolute()
                        .left(px(l.username_label.x))
                        .top(px(l.username_label.y))
                        .w(px(l.username_label.w))
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(p.label))
                        .child("Имя пользователя"),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(l.password_label.x))
                        .top(px(l.password_label.y))
                        .w(px(l.password_label.w))
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(p.label))
                        .child("Пароль"),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(l.terms.x))
                        .top(px(l.terms.y))
                        .w(px(l.terms.w))
                        .text_size(px(12.))
                        .line_height(px(18.6))
                        .text_color(ink(p.muted))
                        .child("Продолжая, вы принимаете правила сообщества и политику конфиденциальности."),
                );
        }
        if instance {
            let field = self.instance.clone();
            root = root.child(self.auth_field(&field, l.user, compact, cx));
            // A separate checkbox under the address field. It owns only the
            // preference; the origin is written after the form is confirmed.
            let checked = self.auth.remember_instance;
            let box_bg = if checked { p.lime } else { p.field_bg };
            let box_border = if checked { p.lime } else { p.field_border };
            root = root.child(
                div()
                    .id("auth-remember-instance")
                    .debug_selector(|| "auth-remember-instance".into())
                    .absolute()
                    .left(px(l.remember.x))
                    .top(px(l.remember.y))
                    .w(px(l.remember.w))
                    .h(px(l.remember.h))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.auth.remember_instance = !s.auth.remember_instance;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(18.))
                            .h(px(18.))
                            .rounded(px(4.))
                            .bg(ink(box_bg))
                            .border_1()
                            .border_color(ink(box_border))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(if checked {
                                icon("icons/check.svg", 12., ink(p.accent_button_ink))
                            } else {
                                div().into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(ink(p.muted))
                            .child("Запомнить адрес"),
                    ),
            );
        } else {
            let username = self.username.clone();
            let password = self.password.clone();
            root = root.child(self.auth_field(&username, l.user, compact, cx));
            root = root.child(self.auth_field(&password, l.password, compact, cx));
        }
        // Primary action: "Продолжить" on the address step, then sign-in.
        let busy = self.auth.busy;
        let accent = if busy {
            rgba((p.lime << 8) | 0x8c)
        } else {
            ink(p.lime)
        };
        root = root.child(
            div()
                .id("auth-primary")
                .absolute()
                .left(px(l.primary.x))
                .top(px(l.primary.y))
                .w(px(l.primary.w))
                .h(px(l.primary.h))
                .rounded(px(11.))
                .bg(accent)
                .text_size(px(16.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(ink(p.accent_button_ink))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(|s, _, window, cx| {
                    if s.auth.is_instance() {
                        s.submit_instance(window, cx);
                    } else {
                        s.submit_login(window, cx);
                    }
                }))
                .child(if busy {
                    "…".to_string()
                } else if instance {
                    "Продолжить".to_string()
                } else {
                    "Войти в Voxhold".to_string()
                }),
        );
        if !self.auth.status.is_empty() {
            root = root.child(self.auth_error_row(l.error));
        }
        root.into_any_element()
    }
}
