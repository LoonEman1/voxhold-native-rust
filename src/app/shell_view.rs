use super::*;

impl Render for Voxhold {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        self.viewport = (viewport.width.into(), viewport.height.into());
        let (vw, vh) = self.viewport;
        let p = self.palette;
        let mut root = div()
            .relative()
            .size_full()
            .font_family("Inter")
            .bg(ink(p.bg))
            .text_color(ink(p.fg))
            .capture_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        s.show_search = false;
                        s.show_tools = false;
                        s.show_settings = false;
                        s.show_channel_settings = false;
                        s.show_server_settings = false;
                        s.show_inbox = false;
                        s.show_channels = false;
                        s.show_servers = false;
                        cx.notify();
                        cx.stop_propagation();
                    }
                    "f5" if s.client.is_some() => {
                        if s.access_pending {
                            s.invalidate_access(window, cx);
                        } else {
                            s.refresh_history(window, cx);
                        }
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .on_mouse_move(cx.listener(|s, event: &gpui::MouseMoveEvent, _, cx| {
                if let Some(drag) = s.scrollbar_drag {
                    if event.pressed_button != Some(MouseButton::Left) {
                        s.scrollbar_drag = None;
                        s.history_list.scrollbar_drag_ended();
                        cx.notify();
                        return;
                    }
                    let y: f32 = event.position.y.into();
                    let ratio = ((y - drag.grab - drag.top) / (drag.height - drag.thumb).max(1.))
                        .clamp(0., 1.);
                    let max: f32 = s.history_list.max_offset_for_scrollbar().height.into();
                    s.history_list
                        .set_offset_from_scrollbar(gpui::point(px(0.), px(ratio * max)));
                    cx.notify();
                    return;
                }
                let Some(drag) = s.drag else { return };
                if event.pressed_button != Some(MouseButton::Left) {
                    s.drag = None;
                    s.save_shell();
                    cx.notify();
                    return;
                }
                let x: f32 = event.position.x.into();
                match drag.side {
                    SplitterSide::Channels => {
                        s.shell.channel_width =
                            layout::proposed_width(drag.side, x, drag.grab_offset);
                    }
                    SplitterSide::Members => {
                        s.shell.members_width =
                            layout::members_width_from_pointer(s.viewport.0, x, drag.grab_offset);
                    }
                }
                s.shell = s.shell.normalized();
                s.drag = Some(SplitterDrag {
                    last_width: match drag.side {
                        SplitterSide::Channels => s.shell.channel_width,
                        SplitterSide::Members => s.shell.members_width,
                    },
                    ..drag
                });
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|s, _, _, cx| {
                    if s.scrollbar_drag.take().is_some() {
                        s.history_list.scrollbar_drag_ended();
                        cx.notify();
                    }
                    if s.drag.take().is_some() {
                        s.save_shell();
                        cx.notify();
                    }
                }),
            );
        if self.client.is_none() {
            if !self.initial_focus_done {
                self.initial_focus_done = true;
                // Focus after the first frame so the field's focus handle is
                // mounted, matching the Go `RequestFocus(user)` call.
                let weak = cx.entity().downgrade();
                window.on_next_frame(move |window, cx| {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |s, cx| {
                            let entity = if s.auth.is_instance() {
                                s.instance.clone()
                            } else {
                                s.username.clone()
                            };
                            entity.update(cx, |input, cx| input.focus(window, cx));
                        });
                    }
                });
            }
            return root.child(self.render_auth(cx)).into_any_element();
        }
        if !self.shell_initialized {
            self.shell_initialized = true;
            self.shell = self
                .shell_path
                .as_deref()
                .map(layout::Settings::load)
                .unwrap_or_default();
            // Go hides the members drawer on first creation for narrow windows;
            // the saved value is kept for the next wide start.
            self.show_members = self.shell.members_open && vw > layout::COMPACT_MAX_WIDTH;
            self.load_channel_members(cx);
        }
        let mobile = vw <= layout::MOBILE_MAX_WIDTH;
        let channels_open = !mobile || self.show_channels;
        let l = layout::compute_layout(vw, vh, self.shell, channels_open, self.show_members);
        if !mobile {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.rail.x))
                    .top(px(l.rail.y))
                    .w(px(l.rail.w))
                    .h(px(l.rail.h))
                    .child(self.rail(cx)),
            );
        }
        let selected = self.channels.iter().find(|c| c.id == self.channel).cloned();
        let readable = !self.access_pending && selected.as_ref().is_some_and(|c| c.has_history());
        let writable = !self.access_pending && selected.as_ref().is_some_and(|c| c.writable());
        let history = self.render_history(window, cx, l.chat, mobile);
        let title = selected
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Выберите канал".into());
        let explanation = selected
            .as_ref()
            .map(|c| {
                if c.e2ee_required {
                    "Этот канал требует E2EE; сообщения недоступны."
                } else if c.kind == "voice" && !c.readable() {
                    "Нет доступа к голосовому каналу."
                } else {
                    ""
                }
            })
            .unwrap_or("");
        let mut chat = div().relative().size_full().flex().flex_col().bg(ink(p.bg));
        let show_labels = l.chat.w > 860.0 && !mobile;
        let header = div()
            .h(px(if mobile { 58. } else { 67. }))
            .px(px(16.))
            .items_center()
            .bg(ink(p.surface))
            .border_b_1()
            .border_color(ink(p.line))
            .flex()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(mobile, |row| {
                        row.child(
                            div()
                                .id("channels-toggle")
                                .cursor_pointer()
                                .text_color(ink(p.muted))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_channels = !s.show_channels;
                                    cx.notify();
                                }))
                                .child(icon("icons/menu.svg", 18., ink(p.muted))),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(title),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_size(px(13.))
                    .text_color(ink(p.muted))
                    .child(
                        div()
                            .id("toggle-search")
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.show_search = !s.show_search;
                                cx.notify();
                            }))
                            .child(icon("icons/search.svg", 16., ink(p.muted)))
                            .when(show_labels, |button| button.child("Поиск")),
                    )
                    .child(
                        div()
                            .id("members-toggle")
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                let open = !s.show_members;
                                s.set_members_open(open, cx);
                            }))
                            .child(icon("icons/people.svg", 16., ink(p.muted)))
                            .when(show_labels, |button| button.child("Участники")),
                    ),
            );
        chat = chat.child(header);
        chat = chat.child(history);
        if self.access_pending {
            let status = div().p_2().text_color(ink(p.muted));
            chat = chat.child(if self.access_reload_in_flight {
                status.child("Проверяем доступ…")
            } else {
                status.child(
                    div()
                        .id("retry-channel-access")
                        .cursor_pointer()
                        .child("Повторить проверку доступа")
                        .on_click(cx.listener(|s, _, window, cx| s.invalidate_access(window, cx))),
                )
            });
        }
        if !explanation.is_empty() {
            chat = chat.child(
                div()
                    .px(px(16.))
                    .pb(px(6.))
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(explanation),
            );
        }
        if !self.error.is_empty() {
            chat = chat.child(
                div()
                    .px(px(16.))
                    .pb(px(6.))
                    .text_size(px(12.))
                    .text_color(ink(p.danger))
                    .child(self.error.clone()),
            );
        }
        if readable {
            if let Some(attachments) = &self.attachments {
                chat = chat.child(attachments.clone());
            }
        }
        if writable {
            chat = chat.child(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(47.))
                            .capture_key_down(cx.listener(
                                |s, event: &gpui::KeyDownEvent, window, cx| {
                                    let key = &event.keystroke;
                                    if key.key == "enter"
                                        && !key.modifiers.shift
                                        && !key.modifiers.control
                                        && !key.modifiers.alt
                                        && !key.modifiers.platform
                                    {
                                        s.send(window, cx);
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .child(
                                TextInput::new(&self.composer)
                                    .font_family("Inter")
                                    .text_size(px(14.))
                                    .h_full(),
                            ),
                    )
                    .child(
                        div()
                            .id("send")
                            .w(px(112.))
                            .h(px(40.))
                            .rounded(px(11.))
                            .bg(ink(p.lime))
                            .text_color(ink(0x10100f))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, w, cx| s.send(w, cx)))
                            .child(icon("icons/send.svg", 16., ink(0x10100f)))
                            .child("Отправить"),
                    ),
            );
        }
        if self.show_search {
            if let Some(search) = &self.search {
                chat = chat.child(div().h(px(300.)).child(search.clone()));
            }
        }
        if self.show_tools {
            if let Some(tools) = &self.message_tools {
                chat = chat.child(
                    div()
                        .h(px(360.))
                        .child(
                            div()
                                .id("close-message-tools")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_tools = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(tools.clone()),
                );
            }
        }
        #[cfg(feature = "voxhold-media")]
        if !self.access_pending && self.channel == self.voice_channel && self.voice_channel != 0 {
            if let Some(voice) = &self.voice {
                chat = chat.child(div().h(px(400.)).child(voice.clone()));
            }
        }
        if self.show_inbox {
            if let Some(inbox) = &self.inbox {
                chat = chat.child(
                    div()
                        .h(px(500.))
                        .child(
                            div()
                                .id("inbox-close")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_inbox = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(inbox.clone()),
                );
            }
        }
        if self.show_server_settings {
            if let Some(settings) = &self.server_settings {
                chat = chat.child(
                    div()
                        .h(px(500.))
                        .child(
                            div()
                                .id("server-settings-close")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_server_settings = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(settings.clone()),
                );
            }
        }
        if self.show_channel_settings {
            if let Some(settings) = &self.channel_settings {
                chat = chat.child(
                    div()
                        .h(px(500.))
                        .child(
                            div()
                                .id("channel-settings-close")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_channel_settings = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(settings.clone()),
                );
            }
        }
        if self.show_settings {
            if let Some(settings) = &self.settings {
                chat = chat.child(
                    div()
                        .h(px(500.))
                        .child(
                            div()
                                .id("settings-close")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_settings = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(settings.clone()),
                );
            }
        }
        root = root.child(
            div()
                .absolute()
                .left(px(l.chat.x))
                .top(px(l.chat.y))
                .w(px(l.chat.w))
                .h(px(l.chat.h))
                .child(chat),
        );
        if mobile {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.bottom.x))
                    .top(px(l.bottom.y))
                    .w(px(l.bottom.w))
                    .h(px(l.bottom.h))
                    .bg(ink(p.surface))
                    .border_t_1()
                    .border_color(ink(p.line))
                    .flex()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(
                        div()
                            .id("bottom-channels")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.show_channels = !s.show_channels;
                                cx.notify();
                            }))
                            .child("Каналы"),
                    )
                    .child(
                        div()
                            .id("bottom-members")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                let open = !s.show_members;
                                s.set_members_open(open, cx);
                            }))
                            .child("Участники"),
                    )
                    .child(
                        div()
                            .id("bottom-settings")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.show_settings = !s.show_settings;
                                if s.show_settings {
                                    if let Some(settings) = &s.settings {
                                        settings.update(cx, |settings, cx| settings.load(cx));
                                    }
                                }
                                cx.notify();
                            }))
                            .child("Настройки"),
                    )
                    .child(
                        div()
                            .id("bottom-logout")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, w, cx| s.logout(w, cx)))
                            .child("Выйти"),
                    ),
            );
        }
        if l.is_channels_open() {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.channels.x))
                    .top(px(l.channels.y))
                    .w(px(l.channels.w))
                    .h(px(l.channels.h))
                    .shadow_lg()
                    .child(self.channels_panel(cx)),
            );
        }
        if l.is_members_open() {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.members.x))
                    .top(px(l.members.y))
                    .w(px(l.members.w))
                    .h(px(l.members.h))
                    .child(self.members_panel(window, cx)),
            );
        }
        // Splitter handles sit 6 px before the panel edge with a 13 px hitbox.
        if l.is_channels_open() && !mobile {
            root = root.child(
                div()
                    .id("channel-splitter")
                    .absolute()
                    .left(px(l.channel_handle.x))
                    .top(px(l.channel_handle.y))
                    .w(px(l.channel_handle.w))
                    .h(px(l.channel_handle.h))
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|s, event: &gpui::MouseDownEvent, _, cx| {
                            let x: f32 = event.position.x.into();
                            s.drag = Some(SplitterDrag {
                                side: SplitterSide::Channels,
                                grab_offset: x - (layout::RAIL_WIDTH + s.shell.channel_width),
                                last_width: s.shell.channel_width,
                            });
                            cx.notify();
                        }),
                    ),
            );
        }
        if l.is_members_open() {
            root = root.child(
                div()
                    .id("members-splitter")
                    .absolute()
                    .left(px(l.member_handle.x))
                    .top(px(l.member_handle.y))
                    .w(px(l.member_handle.w))
                    .h(px(l.member_handle.h))
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|s, event: &gpui::MouseDownEvent, _, cx| {
                            let x: f32 = event.position.x.into();
                            s.drag = Some(SplitterDrag {
                                side: SplitterSide::Members,
                                grab_offset: x - (s.viewport.0 - s.shell.members_width),
                                last_width: s.shell.members_width,
                            });
                            cx.notify();
                        }),
                    ),
            );
        }
        if !mobile && self.show_servers {
            root = root.child(self.server_picker(cx));
        }
        root.into_any_element()
    }
}
