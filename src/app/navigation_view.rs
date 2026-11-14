use super::ui::channel_icon;
use super::*;

impl Voxhold {
    pub(super) fn rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let rail = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .py(px(12.))
            .bg(ink(p.panel))
            .child(
                div()
                    .id("open-instance")
                    .w(px(39.))
                    .h(px(39.))
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, window, cx| {
                        if s.servers.len() == 1 {
                            let server = s.servers[0].id;
                            s.open_server(server, window, cx);
                        } else if !s.servers.is_empty() {
                            s.show_servers = !s.show_servers;
                            cx.notify();
                        }
                    }))
                    .child(
                        gpui::img("icons/brand.svg")
                            .size_full()
                            .with_fallback(move || {
                                div()
                                    .size_full()
                                    .flex()
                                    .justify_center()
                                    .items_center()
                                    .rounded(px(11.))
                                    .bg(ink(p.lime))
                                    .text_color(ink(0x151510))
                                    .text_size(px(20.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("V")
                                    .into_any_element()
                            }),
                    ),
            )
            .child(
                div()
                    .id("inbox-open")
                    .w(px(48.))
                    .h(px(48.))
                    .rounded(px(14.))
                    .bg(ink(p.surface))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(icon("icons/inbox.svg", 18., ink(p.muted)))
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_inbox = !s.show_inbox;
                        if s.show_inbox {
                            if let Some(inbox) = &s.inbox {
                                inbox.update(cx, |inbox, cx| inbox.load(window, cx));
                            }
                        }
                        cx.notify();
                    })),
            );
        rail.child(div().flex_1())
            .child(
                div()
                    .id("settings-open")
                    .w(px(48.))
                    .h(px(48.))
                    .rounded(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(icon("icons/settings.svg", 18., ink(p.muted)))
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.show_settings = !s.show_settings;
                        if s.show_settings {
                            if let Some(settings) = &s.settings {
                                settings.update(cx, |settings, cx| settings.load(cx));
                            }
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    pub(super) fn open_server(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.show_servers = false;
        self.show_inbox = false;
        self.show_settings = false;
        if self.server != id || self.access_pending || self.channels.is_empty() {
            self.select_server(id, window, cx);
        }
        cx.notify();
    }

    pub(super) fn server_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        div()
            .id("server-picker")
            .absolute()
            .left(px(layout::RAIL_WIDTH))
            .top(px(10.))
            .w(px(230.))
            .max_h(px(self.viewport.1 - 20.))
            .overflow_y_scroll()
            .occlude()
            .p_2()
            .rounded(px(10.))
            .shadow_lg()
            .bg(ink(p.surface))
            .border_1()
            .border_color(ink(p.line))
            .children(self.servers.iter().map(|server| {
                let id = server.id;
                div()
                    .id(("server-choice", id as u64))
                    .p_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(id == self.server, |row| row.bg(ink(p.selection)))
                    .hover(|row| row.bg(ink(p.selection)))
                    .on_click(cx.listener(move |s, _, window, cx| s.open_server(id, window, cx)))
                    .child(server.name.clone())
            }))
            .into_any_element()
    }

    pub(super) fn channels_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let can_manage = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .is_some_and(|s| s.role == "owner" || s.role == "admin");
        let owner = self
            .servers
            .iter()
            .any(|s| s.id == self.server && s.role == "owner");
        let name = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Voxhold".into());
        let mut panel = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ink(p.surface))
            .border_r_1()
            .border_color(ink(p.line))
            .child(
                div()
                    .h(px(67.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(15.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(name)
                    .child(div().text_size(px(12.)).text_color(ink(p.muted)).child("‹")),
            );
        // Without the rail (narrow layout) the channels overlay is the only
        // place to switch servers, so it carries the server strip.
        if self.viewport.0 <= layout::MOBILE_MAX_WIDTH && self.servers.len() > 1 {
            let mut strip = div()
                .flex()
                .items_center()
                .gap_2()
                .px(px(12.))
                .py(px(8.))
                .border_b_1()
                .border_color(ink(p.line));
            for server in self.servers.clone() {
                let id = server.id;
                let active = id == self.server;
                strip = strip.child(
                    div()
                        .id(("mobile-server", id as u64))
                        .w(px(34.))
                        .h(px(34.))
                        .rounded(px(10.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .bg(if active {
                            ink(p.lime)
                        } else {
                            ink(p.selection)
                        })
                        .text_color(if active { ink(0x10100f) } else { ink(p.fg) })
                        .font_weight(gpui::FontWeight::BOLD)
                        .on_click(cx.listener(move |s, _, window, cx| {
                            s.select_server(id, window, cx);
                            s.show_channels = false;
                            cx.notify();
                        }))
                        .child(server.name.chars().next().unwrap_or('?').to_string()),
                );
            }
            panel = panel.child(strip);
        }
        if owner {
            panel = panel.child(
                div()
                    .id("server-settings-open")
                    .px(px(16.))
                    .py(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Настройки сервера")
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_server_settings = true;
                        if let Some(settings) = &s.server_settings {
                            settings.update(cx, |settings, cx| settings.load(window, cx));
                        }
                        cx.notify();
                    })),
            );
        }
        if can_manage && self.channel != 0 {
            panel = panel.child(
                div()
                    .id("channel-settings-open")
                    .px(px(16.))
                    .py(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Настройки канала")
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_channel_settings = true;
                        if let Some(settings) = &s.channel_settings {
                            settings.update(cx, |settings, cx| settings.load(window, cx));
                        }
                        cx.notify();
                    })),
            );
        }
        for (kind, title) in [
            ("news", "НОВОСТНЫЕ КАНАЛЫ"),
            ("text", "ТЕКСТОВЫЕ КАНАЛЫ"),
            ("voice", "ГОЛОСОВЫЕ КАНАЛЫ"),
        ] {
            let items: Vec<Channel> = self
                .channels
                .iter()
                .filter(|c| c.kind == kind)
                .cloned()
                .collect();
            if items.is_empty() {
                continue;
            }
            panel = panel.child(
                div()
                    .px(px(16.))
                    .pt(px(10.))
                    .pb(px(4.))
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.muted))
                    .child(title),
            );
            for channel in items {
                let id = channel.id;
                let active = id == self.channel;
                let locked = channel.kind == "news" && !channel.writable() || !channel.readable();
                let color = if active { ink(p.fg) } else { ink(p.muted) };
                panel = panel.child(
                    div()
                        .id(("channel", id as u64))
                        .h(px(35.))
                        .px(px(16.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .bg(if active {
                            ink(p.selection)
                        } else {
                            rgba(0x00000000)
                        })
                        .text_color(color)
                        .child(channel_icon(&channel.kind, color))
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .text_size(px(14.))
                                .child(channel.name.clone()),
                        )
                        .when(
                            channel.access_restricted
                                || channel.e2ee_required
                                || !channel.readable(),
                            |row| row.child(icon("icons/lock.svg", 12., ink(p.muted))),
                        )
                        .when(locked, |row| row.text_color(ink(p.muted)))
                        .on_click(cx.listener(move |s, _, w, cx| s.select_channel(id, w, cx))),
                );
            }
        }
        panel = panel.child(div().flex_1());
        #[cfg(feature = "voxhold-media")]
        if let Some(voice) = &self.voice {
            if !self.access_pending && self.channel == self.voice_channel && self.voice_channel != 0
            {
                panel = panel.child(div().h(px(260.)).child(voice.clone()));
            }
        }
        panel
            .child(
                div()
                    .h(px(58.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(ink(p.line))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .text_color(ink(p.fg))
                            .child(format!("@{}", self.auth.username)),
                    )
                    .child(
                        div()
                            .id("theme-toggle")
                            .w(px(28.))
                            .h(px(28.))
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, window, cx| s.toggle_theme(window, cx)))
                            .child(icon(
                                if p.light {
                                    "icons/moon.svg"
                                } else {
                                    "icons/sun.svg"
                                },
                                15.,
                                ink(p.muted),
                            )),
                    )
                    .child(
                        div()
                            .id("logout")
                            .w(px(28.))
                            .h(px(28.))
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, w, cx| s.logout(w, cx)))
                            .child(icon("icons/logout.svg", 15., ink(p.muted))),
                    ),
            )
            .into_any_element()
    }
}
