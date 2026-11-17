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
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.show_inbox = !s.show_inbox;
                        if s.show_inbox {
                            s.show_search = false;
                            s.message_menu = None;
                            s.forward = None;
                            s.refresh_recents(cx);
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
                    .track_focus(&self.settings_entry_focus)
                    .tab_index(0)
                    .child(icon("icons/settings.svg", 18., ink(p.muted)))
                    .on_click(cx.listener(|s, _, window, cx| {
                        let focus = s.settings_entry_focus.clone();
                        s.open_settings(SettingsKind::Personal, focus, window, cx);
                    })),
            )
            .into_any_element()
    }

    pub(super) fn open_server(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.show_servers = false;
        self.show_inbox = false;
        self.close_stale_settings(id, 0, window, cx);
        if self.server != id
            || self.access_pending
            || self.channels.is_empty()
            || self.conversation.is_some()
        {
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

    pub(super) fn channels_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        #[cfg(feature = "voxhold-media")]
        let voice_colors = crate::voice_panel::VoiceColors::resolve(p);
        let can_manage = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .is_some_and(|s| s.role == "owner" || s.role == "admin");
        let owner = self.servers.iter().any(|server| {
            server.id == self.server
                && crate::settings::Role::parse(&server.role).can_manage_server()
        });
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
                    .track_focus(&self.server_entry_focus)
                    .tab_index(0)
                    .child("Настройки сервера")
                    .on_click(cx.listener(|s, _, window, cx| {
                        let focus = s.server_entry_focus.clone();
                        s.open_settings(SettingsKind::Server, focus, window, cx);
                    })),
            );
        }
        if self.channel != 0 && self.conversation.is_none() {
            if can_manage {
                panel = panel.child(
                    div()
                        .id("channel-settings-open")
                        .px(px(16.))
                        .py(px(6.))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .track_focus(&self.channel_entry_focus)
                        .tab_index(0)
                        .child("Настройки канала")
                        .on_click(cx.listener(|s, _, window, cx| {
                            let focus = s.channel_entry_focus.clone();
                            s.open_settings(SettingsKind::Channel, focus, window, cx);
                        })),
                );
            }
            // Notifications are a per-account preference; a regular member
            // sees this action without the channel administration form.
            let notification_channel = self
                .channels
                .iter()
                .find(|channel| channel.id == self.channel && channel.has_history())
                .cloned();
            if let Some(channel) = notification_channel {
                panel = panel.child(
                    div()
                        .id("channel-notifications-open")
                        .px(px(16.))
                        .py(px(6.))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .track_focus(&self.notifications_entry_focus)
                        .tab_index(0)
                        .child(format!("Уведомления · {}", channel.name))
                        .on_click(cx.listener(|s, _, window, cx| {
                            let focus = s.notifications_entry_focus.clone();
                            s.open_settings(SettingsKind::ChannelNotifications, focus, window, cx);
                        })),
                );
            }
        }
        let now = super::recordings_ops::now_secs();
        // The list grows with the participants of every voice channel, so it
        // scrolls inside its own area and never pushes the account footer off
        // the panel.
        let mut list = div().flex().flex_col();
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
            list = list.child(
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
                let active = id == self.channel && self.conversation.is_none();
                let locked = channel.kind == "news" && !channel.writable() || !channel.readable();
                let color = if active { ink(p.fg) } else { ink(p.muted) };
                #[cfg(feature = "voxhold-media")]
                let roster = if channel.kind == "voice" {
                    self.voice_roster(self.server, id)
                } else {
                    Vec::new()
                };
                list = list.child(
                    div()
                        .id(("channel", id as u64))
                        .debug_selector(move || format!("channel-{id}").into())
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
                        .when(
                            channel.kind == "voice" && self.recordings.live(id, now).is_some(),
                            |row| {
                                row.child(
                                    div().w(px(7.)).h(px(7.)).rounded_full().bg(ink(p.danger)),
                                )
                            },
                        )
                        .when(
                            channel.kind == "voice" && self.voice_has_stream(id),
                            |row| row.child(icon("icons/monitor.svg", 12., ink(p.lime))),
                        )
                        .when(channel.kind == "voice" && self.voice_count(id) > 0, |row| {
                            row.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(ink(p.muted))
                                    .child(self.voice_count(id).to_string()),
                            )
                        })
                        .when(locked, |row| row.text_color(ink(p.muted)))
                        .on_click(cx.listener(move |s, _, w, cx| s.select_channel(id, w, cx))),
                );
                #[cfg(feature = "voxhold-media")]
                for participant in &roster {
                    list = list.child(self.voice_participant_row(
                        participant,
                        id,
                        voice_colors,
                        window,
                        cx,
                    ));
                }
            }
        }
        panel = panel.child(
            div()
                .id("channel-list")
                .debug_selector(|| "channel-list".into())
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(list),
        );
        #[cfg(feature = "voxhold-media")]
        if let Some(dock) = self.voice_dock(cx) {
            panel = panel.child(dock);
        }
        panel
            .child(
                div()
                    .h(px(58.))
                    .debug_selector(|| "account-footer".into())
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

    /// One row under a voice channel: the same avatar cache as the members
    /// panel, the display name and the participant state. Like Go, rooms other
    /// than the active call are dimmed and the local user is highlighted;
    /// speech uses the same green as the room card.
    #[cfg(feature = "voxhold-media")]
    fn voice_participant_row(
        &mut self,
        participant: &voxhold_media::controller::Participant,
        channel: i64,
        voice: crate::voice_panel::VoiceColors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        // Only the active call is analysed for speech; the local mutes live in
        // the panel too.
        let (reports_speaking, local_muted) = self.voice.as_ref().map_or((false, false), |panel| {
            let panel = panel.read(cx);
            (
                panel.speaking(&participant.connection_id),
                panel.is_local_muted(&participant.connection_id),
            )
        });
        let self_user = self.self_user_id != 0 && self.self_user_id == participant.user_id;
        let active_room = self.voice_channel != 0 && self.voice_channel == channel;
        let muted = participant.self_mute
            || participant.server_mute
            || participant.self_deaf
            || local_muted;
        // A muted or reconnecting member keeps the badge instead of a stale
        // speaking ring.
        let speaking = reports_speaking
            && !participant.reconnecting
            && !participant.self_mute
            && !participant.server_mute;
        let state = crate::voice_room::voice_state_icon(
            participant.reconnecting,
            participant.self_deaf,
            muted,
        );
        let name_color = if speaking || self_user {
            voice.accent_text
        } else if muted || !active_room {
            p.muted
        } else {
            p.fg
        };
        let member = self
            .members
            .iter()
            .find(|member| member.user_id == participant.user_id)
            .cloned();
        let name = member
            .as_ref()
            .map(|member| member.username.clone())
            .unwrap_or_else(|| format!("Участник {}", participant.user_id));
        let avatar = member
            .as_ref()
            .and_then(|member| member.avatar_version.clone())
            .filter(|version| !version.is_empty())
            .and_then(|version| {
                let key = (participant.user_id, version.clone());
                if let Some((image, used)) = self.roster.avatars.get_mut(&key) {
                    *used = std::time::Instant::now();
                    return Some(image.clone());
                }
                self.request_avatar(participant.user_id, version, window, cx);
                None
            });
        let circle = div()
            .w(px(22.))
            .h(px(22.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full();
        let face = if let Some(image) = avatar {
            circle.child(
                gpui::img(image)
                    .size_full()
                    .rounded_full()
                    .object_fit(gpui::ObjectFit::Cover),
            )
        } else {
            circle
                .bg(ink(crate::members::avatar_color(&name)))
                .text_color(ink(0x151510))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::BOLD)
                .child(crate::members::initials(&name))
        };
        let connection = participant.connection_id.clone();
        // The badge mirrors the room card: any non-healthy state uses the
        // danger tone, the healthy dot uses the accent ink.
        let badge = if state.is_some() {
            let path = if participant.reconnecting {
                "icons/wifi-off.svg"
            } else if participant.self_deaf {
                "icons/headphones-off.svg"
            } else {
                "icons/mic-off.svg"
            };
            div()
                .flex_shrink_0()
                .debug_selector({
                    let connection = connection.clone();
                    move || format!("voice-state-{connection}").into()
                })
                .child(icon(path, 12., ink(p.danger)))
                .into_any_element()
        } else {
            div()
                .w(px(7.))
                .h(px(7.))
                .flex_shrink_0()
                .rounded_full()
                .bg(ink(if active_room {
                    voice.accent_text
                } else {
                    p.muted
                }))
                .into_any_element()
        };
        div()
            .id(gpui::ElementId::named_usize(
                connection.clone(),
                channel.max(0) as usize,
            ))
            .debug_selector(move || format!("voice-member-{connection}").into())
            .h(px(32.))
            .pl(px(42.))
            .pr(px(16.))
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(26.))
                    .h(px(26.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_2()
                    .border_color(if speaking {
                        ink(voice.accent_text)
                    } else {
                        rgba(0x00000000)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(face),
            )
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(ink(name_color))
                    .child(name),
            )
            .child(badge)
            .into_any_element()
    }
}
