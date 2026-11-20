use super::ui::channel_icon;
use super::*;

impl Voxhold {
    pub(super) fn rail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let mut recents = div()
            .id("rail-recent-conversations")
            .w_full()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .gap_2();
        let conversations: Vec<_> = self
            .conversations
            .iter()
            .filter(|row| !row.saved)
            .take(12)
            .cloned()
            .collect();
        for row in conversations {
            let id = row.id;
            let name = inbox::conversation_title(&row, self.self_user_id);
            let peer = inbox::conversation_peer(&row, self.self_user_id);
            let unread = row.unread_count.min(99);
            let face = if row.direct {
                let image = self.member_avatar(peer, window, cx);
                super::ui::avatar(image, &name, 36.)
            } else {
                icon("icons/people.svg", 21., ink(p.muted))
            };
            let selected = self.conversation.as_ref().is_some_and(|chat| chat.id == id);
            recents = recents.child(
                self.rail_button(("rail-chat", id as u64), selected, face)
                    .when(unread > 0, |button| {
                        button.child(
                            div()
                                .absolute()
                                .right_0()
                                .bottom_0()
                                .rounded(px(6.))
                                .px(px(4.))
                                .bg(ink(p.lime))
                                .text_color(ink(0x17170f))
                                .text_size(px(10.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(unread.to_string()),
                        )
                    })
                    .on_click(cx.listener(move |s, _, window, cx| {
                        if let Some(row) = s.conversations.iter().find(|row| row.id == id).cloned()
                        {
                            if !s.personal_mode {
                                s.enter_personal_mode(window, cx);
                            }
                            s.select_conversation(row, window, cx);
                        }
                    })),
            );
        }
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
                    .id("messages-open")
                    .debug_selector(|| "messages-open".into())
                    .w(px(48.))
                    .h(px(48.))
                    .rounded(px(14.))
                    .bg(if self.personal_mode {
                        ink(p.selection)
                    } else {
                        ink(p.surface)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(icon(
                        "icons/message.svg",
                        18.,
                        ink(if self.personal_mode { p.fg } else { p.muted }),
                    ))
                    .on_click(cx.listener(|s, _, window, cx| {
                        if s.personal_mode {
                            s.return_to_channels(window, cx);
                        } else {
                            s.enter_personal_mode(window, cx);
                        }
                    })),
            );
        rail.child(div().w(px(32.)).h(px(1.)).bg(ink(p.line)))
            .child(recents)
            .child(
                self.rail_button(
                    "rail-new-conversation",
                    false,
                    icon("icons/add.svg", 20., ink(p.muted)),
                )
                .on_click(cx.listener(|s, _, window, cx| s.open_conversation_creator(window, cx))),
            )
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

    fn rail_button(
        &self,
        id: impl Into<gpui::ElementId>,
        active: bool,
        face: AnyElement,
    ) -> gpui::Stateful<gpui::Div> {
        let p = self.palette;
        div()
            .id(id)
            .relative()
            .size(px(48.))
            .flex_shrink_0()
            .rounded(px(14.))
            .bg(ink(if active { p.selection } else { p.surface }))
            .hover(|row| row.bg(ink(p.selection)))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(face)
    }

    pub(super) fn open_server(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.show_servers = false;
        if self.personal_mode {
            // Save the open DM under `conversation(id)` before the channel and
            // conversation are cleared, otherwise the draft is lost.
            self.save_composer_draft(cx);
            self.personal_mode = false;
            self.personal_return = None;
            self.pending_return_channel = None;
            self.show_members = self.personal_members;
            self.personal_members = false;
            self.messaging.epoch = self.messaging.epoch.wrapping_add(1);
            self.messaging.clear_search();
            self.messaging.creator = None;
            self._creator_subscriptions.clear();
            self.messaging.notifications = None;
            self.messaging.older_request = None;
            self.messaging.open_request = None;
            self.conversation = None;
            self.channel = 0;
            self.reset_personal_chat(window, cx);
        }
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
                    .flex_shrink_0()
                    .px(px(17.))
                    .border_b_1()
                    .border_color(ink(p.line))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .line_height(px(14.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(ink(p.muted))
                                    .child("ПРОСТРАНСТВО"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(17.))
                                    .line_height(px(22.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(name),
                            ),
                    )
                    .when(owner, |header| {
                        header.child(
                            div()
                                .id("server-settings-open")
                                .debug_selector(|| "server-settings-open".into())
                                .flex_shrink_0()
                                .flex()
                                .size(px(32.))
                                .items_center()
                                .justify_center()
                                .rounded(px(7.))
                                .hover(|button| button.bg(ink(p.selection)))
                                .cursor_pointer()
                                .track_focus(&self.server_entry_focus)
                                .tab_index(0)
                                .child(icon("icons/settings.svg", 18., ink(p.muted)))
                                .on_click(cx.listener(|s, _, window, cx| {
                                    let focus = s.server_entry_focus.clone();
                                    s.open_settings(SettingsKind::Server, focus, window, cx);
                                })),
                        )
                    }),
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
        let now = super::recordings_ops::now_secs();
        // The list grows with the participants of every voice channel, so it
        // scrolls inside its own area and never pushes the account footer off
        // the panel.
        let mut list = div().flex().flex_col().px(px(8.));
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
                    .px(px(9.))
                    .pt(px(17.))
                    .pb(px(6.))
                    .text_size(px(12.))
                    .line_height(px(17.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.muted))
                    .child(title),
            );
            for channel in items {
                let id = channel.id;
                let focus = window
                    .use_keyed_state(("channel-focus", id as u64), cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone();
                let entry = focus.clone();
                let edit_entry = focus.clone();
                let can_manage = self.can_manage_channel_now() && !self.access_pending;
                let active = id == self.channel && self.conversation.is_none();
                let locked = channel.kind == "news" && !channel.writable() || !channel.readable();
                let color = ink(if locked {
                    p.muted
                } else if active {
                    p.fg
                } else {
                    p.label
                });
                #[cfg(feature = "voxhold-media")]
                let streams = self.voice_stream_list(self.server, id);
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
                        .track_focus(&focus)
                        .tab_index(0)
                        .group("sidebar-channel")
                        .h(px(36.))
                        .mx_1()
                        .px(px(10.))
                        .relative()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(7.))
                        .hover(|row| row.bg(ink(p.selection)))
                        .cursor_pointer()
                        .bg(if active {
                            ink(p.selection)
                        } else {
                            rgba(0x00000000)
                        })
                        .text_color(color)
                        .child(channel_icon(&channel.kind, color))
                        .when(active, |row| {
                            row.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top(px(9.))
                                    .w(px(2.))
                                    .h(px(18.))
                                    .rounded_full()
                                    .bg(ink(p.lime)),
                            )
                        })
                        .when(
                            channel.access_restricted
                                || channel.e2ee_required
                                || !channel.readable(),
                            |row| {
                                row.child(
                                    div()
                                        .size(px(21.))
                                        .rounded(px(5.))
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(ink(p.selection))
                                        .child(icon("icons/lock.svg", 14., ink(p.accent_ink))),
                                )
                            },
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(15.))
                                .line_height(px(22.))
                                .when(active, |name| name.font_weight(gpui::FontWeight::BOLD))
                                .child(channel.name.clone()),
                        )
                        .when(
                            channel.kind == "voice" && self.recordings.live(id, now).is_some(),
                            |row| {
                                row.child(
                                    div().w(px(7.)).h(px(7.)).rounded_full().bg(ink(p.danger)),
                                )
                            },
                        )
                        .when(channel.kind == "voice" && self.voice_count(id) > 0, |row| {
                            row.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(ink(p.muted))
                                    .child(self.voice_count(id).to_string()),
                            )
                        })
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |s, event: &gpui::MouseDownEvent, _, cx| {
                                s.open_channel_menu(id, event.position, entry.clone(), cx);
                                cx.stop_propagation();
                            }),
                        )
                        .on_click(cx.listener(move |s, _, w, cx| s.select_channel(id, w, cx)))
                        .when(can_manage, |row| {
                            row.child(
                                div()
                                    .id(("channel-settings", id as u64))
                                    .flex_shrink_0()
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(5.))
                                    .invisible()
                                    .group_hover("sidebar-channel", |style| style.visible())
                                    .hover(|button| button.bg(ink(p.surface)))
                                    .child(icon("icons/edit.svg", 14., ink(p.muted)))
                                    .on_click(cx.listener(move |s, _, window, cx| {
                                        s.open_channel_settings_menu_action(
                                            id,
                                            edit_entry.clone(),
                                            window,
                                            cx,
                                        );
                                        cx.stop_propagation();
                                    })),
                            )
                        }),
                );
                #[cfg(feature = "voxhold-media")]
                for participant in &roster {
                    list =
                        list.child(self.voice_participant_row(
                            participant,
                            id,
                            voice_colors,
                            streams.iter().find(|item| {
                                item.publisher_connection_id == participant.connection_id
                            }),
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
            .child(self.account_footer(window, cx))
            .into_any_element()
    }

    /// The per-account footer. It is shared by the channel sidebar and the
    /// personal "Сообщения" sidebar, so the account controls never disappear
    /// while the personal surface is open (web `CommunityNavigation`).
    pub(super) fn account_footer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let id = self.self_user_id;
        let name = self.auth.username.clone();
        let avatar = self.member_avatar(id, window, cx);
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
                    .id("account-profile")
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, event: &gpui::ClickEvent, _, cx| {
                        s.open_member_profile(id, event.position(), cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |s, event: &gpui::MouseDownEvent, _, cx| {
                            s.open_member_menu(id, event.position, false, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(super::ui::avatar(avatar, &name, 32.))
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(14.))
                            .text_color(ink(p.fg))
                            .child(format!("@{name}")),
                    ),
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
        stream: Option<&voxhold_media::controller::Publication>,
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
        let speaking = active_room && reports_speaking && !participant.reconnecting && !muted;
        let state = crate::voice_room::voice_state_icon(
            participant.reconnecting,
            participant.self_deaf,
            muted,
        );
        let name_color = if speaking || self_user {
            if self_user {
                p.story_accent
            } else {
                voice.accent_text
            }
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
        let avatar = self.member_avatar(participant.user_id, window, cx);
        let face = super::ui::avatar(avatar, &name, 27.);
        let user_id = participant.user_id;
        let stream = stream.cloned();
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
            .h(px(38.))
            .rounded(px(7.))
            .cursor_pointer()
            .hover(|row| row.bg(ink(p.selection)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |s, event: &gpui::MouseDownEvent, _, cx| {
                    s.open_voice_member_menu(user_id, event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener({
                let stream = stream.clone();
                move |s, event: &gpui::ClickEvent, window, cx| {
                    if let Some(stream) = &stream {
                        s.watch_sidebar_stream(channel, stream, window, cx);
                    } else {
                        s.open_member_profile(user_id, event.position(), cx);
                    }
                }
            }))
            .pl(px(31.))
            .pr(px(16.))
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(31.))
                    .h(px(31.))
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
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(ink(name_color))
                    .child(name),
            )
            .when(stream.is_some(), |row| {
                row.child(
                    div()
                        .debug_selector(move || format!("channel-live-{user_id}").into())
                        .flex_shrink_0()
                        .px(px(5.))
                        .py(px(2.))
                        .rounded(px(5.))
                        .bg(ink(0xb92d36))
                        .text_color(ink(0xffffff))
                        .text_size(px(11.))
                        .line_height(px(14.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("LIVE"),
                )
            })
            .child(badge)
            .into_any_element()
    }
}
