use super::*;

impl Render for Voxhold {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        self.viewport = (viewport.width.into(), viewport.height.into());
        if !self.ui_scale_pinned {
            self.ui_scale = window.scale_factor();
        }
        let (vw, vh) = self.viewport;
        let p = self.palette;
        let mut root = div()
            .relative()
            .size_full()
            .font_family("Inter")
            .bg(ink(p.bg))
            .text_color(ink(p.fg))
            .capture_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, window, cx| {
                #[cfg(feature = "voxhold-media")]
                if s.voice_key_down(event, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                match event.keystroke.key.as_str() {
                    "escape" => {
                        if s.media.viewer.is_some() {
                            s.close_viewer(window);
                            cx.notify();
                            cx.stop_propagation();
                            return;
                        }
                        if s.recordings_dialog.is_some() {
                            s.close_recordings(true, window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        if s.show_thread {
                            s.close_thread(window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        if s.member_menu.take().is_some() {
                            cx.notify();
                            cx.stop_propagation();
                            return;
                        }
                        if s.show_members && s.viewport.0 <= layout::COMPACT_MAX_WIDTH {
                            s.set_members_open(false, cx);
                            cx.stop_propagation();
                            return;
                        }
                        if s.active_settings.is_some() {
                            s.close_settings(true, window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        s.show_search = false;
                        s.message_menu = None;
                        s.forward = None;
                        s.editing = None;
                        s.confirm_delete = None;
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
            .on_drop(cx.listener(|s, paths: &gpui::ExternalPaths, _window, cx| {
                s.drop_files(paths.paths().to_vec(), cx);
            }))
            .capture_key_up(cx.listener(|s, event: &gpui::KeyUpEvent, window, cx| {
                #[cfg(feature = "voxhold-media")]
                if s.voice_key_up(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_modifiers_changed(
                cx.listener(|s, event: &gpui::ModifiersChangedEvent, _, cx| {
                    #[cfg(feature = "voxhold-media")]
                    if s.voice_modifiers_changed(event.modifiers, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|s, event: &gpui::MouseMoveEvent, _, cx| {
                #[cfg(feature = "voxhold-media")]
                s.voice_pointer_moved(event.position, cx);
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
                    #[cfg(feature = "voxhold-media")]
                    s.voice_pointer_released(cx);
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
        #[cfg(feature = "voxhold-media")]
        {
            root = root
                .on_action(cx.listener(|s, _: &voice_panel::VoiceTab, window, cx| {
                    if let Some(voice) = &s.voice {
                        voice.update(cx, |panel, cx| {
                            panel.cycle_focus(false, window, cx);
                        });
                    }
                    cx.stop_propagation();
                }))
                .on_action(cx.listener(|s, _: &voice_panel::VoiceTabPrev, window, cx| {
                    if let Some(voice) = &s.voice {
                        voice.update(cx, |panel, cx| {
                            panel.cycle_focus(true, window, cx);
                        });
                    }
                    cx.stop_propagation();
                }));
        }
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
        *self.recordings_live.borrow_mut() = if self.conversation.is_none() {
            self.recordings
                .live(self.channel, super::recordings_ops::now_secs())
                .map(|record| record.owner_username)
        } else {
            None
        };
        let channels_open = !mobile || self.show_channels;
        let l = layout::compute_layout(vw, vh, self.shell, channels_open, self.show_members);
        #[cfg(feature = "voxhold-media")]
        self.sync_push_to_talk_block(cx);
        #[cfg(feature = "voxhold-media")]
        self.refresh_voice_call(window, cx);
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
        let readable = self.conversation.is_some()
            || (!self.access_pending && selected.as_ref().is_some_and(|c| c.has_history()));
        let writable = self.conversation.is_some()
            || (!self.access_pending && selected.as_ref().is_some_and(|c| c.writable()));
        let thread_active = self.show_thread && self.thread.is_some();
        let voice_room = self.voice_room_visible();
        let history = if voice_room {
            div().into_any_element()
        } else if thread_active {
            div()
                .flex_1()
                .min_h(px(0.))
                .child(self.thread.clone().expect("thread"))
                .into_any_element()
        } else {
            self.render_history(window, cx, l.chat, mobile)
        };
        let title = if let Some(conversation) = &self.conversation {
            inbox::conversation_title(conversation, self.self_user_id)
        } else {
            selected
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "Выберите канал".into())
        };
        let explanation = if self.conversation.is_some() {
            ""
        } else {
            selected
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
                .unwrap_or("")
        };
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
                            .debug_selector(|| "toggle-search".into())
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, window, cx| {
                                s.open_search_popup(false, window, cx)
                            }))
                            .child(icon("icons/search.svg", 16., ink(p.muted)))
                            .when(show_labels, |button| button.child("Поиск")),
                    )
                    .when(self.server != 0 && self.conversation.is_none(), |row| {
                        row.child(
                            div()
                                .id("toggle-recordings")
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .track_focus(&self.recordings_entry_focus)
                                .tab_index(0)
                                .on_click(cx.listener(|s, _, window, cx| {
                                    s.toggle_recordings("header", window, cx)
                                }))
                                .child(icon("icons/inbox.svg", 16., ink(p.muted)))
                                .when(show_labels, |button| button.child("Записи")),
                        )
                    })
                    .when(!self.conversation.is_some(), |row| {
                        row.child(
                            div()
                                .id("toggle-pins")
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .on_click(cx.listener(|s, _, window, cx| {
                                    s.open_search_popup(true, window, cx)
                                }))
                                .child(icon("icons/pin.svg", 16., ink(p.muted)))
                                .when(show_labels, |button| button.child("Закрепы")),
                        )
                    })
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
        if readable && !thread_active {
            if let Some(attachments) = &self.attachments {
                chat = chat.child(attachments.clone());
            }
        }
        if writable && !thread_active {
            let text = self.composer.read(cx).value().to_string();
            let files = self.drafts.borrow().files(self.chat_key());
            let reading = self
                .attachments
                .as_ref()
                .is_some_and(|attachments| attachments.read(cx).is_reading());
            let attachments_ready = self.attachments.is_some() && !self.is_sending() && !reading;
            let valid = draft::validate_send(&text, &files).is_ok();
            let can_send = valid && !self.is_sending() && !reading && !self.access_pending;
            let reply_id = self.drafts.borrow().reply(self.chat_key());
            if reply_id > 0 {
                let (author, excerpt) = self
                    .messages
                    .iter()
                    .find(|message| message.id == reply_id)
                    .map(|message| {
                        (
                            message.author.username.clone(),
                            message.content.replace('\n', " "),
                        )
                    })
                    .unwrap_or_else(|| ("Сообщение".into(), format!("#{reply_id}")));
                chat = chat.child(
                    div()
                        .mx(px(18.))
                        .mb(px(4.))
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(7.))
                        .bg(ink(p.surface))
                        .border_l_2()
                        .border_color(ink(p.lime))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(11.))
                                .text_color(ink(p.muted))
                                .child(format!("Ответ: {author} · {excerpt}")),
                        )
                        .child(
                            div()
                                .id("clear-reply")
                                .cursor_pointer()
                                .text_size(px(11.))
                                .text_color(ink(p.muted))
                                .on_click(cx.listener(|s, _, _, cx| s.clear_reply(cx)))
                                .child("Убрать"),
                        ),
                );
            }
            if let Some(counter) = draft::counter(&text) {
                chat = chat.child(
                    div()
                        .px(px(18.))
                        .pb(px(2.))
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .child(counter),
                );
            }
            if !self.composer_error.is_empty() {
                chat = chat.child(
                    div()
                        .px(px(18.))
                        .pb(px(2.))
                        .text_size(px(12.))
                        .text_color(ink(p.danger))
                        .child(self.composer_error.clone()),
                );
            }
            let send_button = div()
                .id("send")
                .w(px(112.))
                .h(px(40.))
                .rounded(px(11.))
                .bg(ink(if can_send { p.lime } else { p.surface }))
                .text_color(ink(if can_send { 0x10100f } else { p.muted }))
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .when(can_send, |button| {
                    button
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, w, cx| s.send(w, cx)))
                })
                .child(if self.is_sending() {
                    "Отправка…".into_any_element()
                } else {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(
                            "icons/send.svg",
                            16.,
                            ink(if can_send { 0x10100f } else { p.muted }),
                        ))
                        .child("Отправить")
                        .into_any_element()
                });
            chat = chat.child(
                div()
                    .px(px(18.))
                    .py(px(12.))
                    .flex()
                    .items_end()
                    .gap_2()
                    .when(attachments_ready, |row| {
                        row.child(
                            div()
                                .id("attach-files")
                                .w(px(37.))
                                .h(px(37.))
                                .rounded(px(10.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_click(cx.listener(|s, _, _, cx| {
                                    if let Some(attachments) = &s.attachments {
                                        attachments.update(cx, |panel, cx| panel.pick(cx));
                                    }
                                }))
                                .child(icon("icons/paperclip.svg", 17., ink(p.muted))),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(47.))
                            .key_context("Composer")
                            .on_action(cx.listener(|s, _: &composer::ComposerSend, window, cx| {
                                // While the viewer is open it owns the
                                // keyboard, like Go `imageViewerEvent`.
                                if s.media.viewer.is_some() {
                                    return;
                                }
                                s.send(window, cx);
                            }))
                            .on_action(cx.listener(
                                |s, _: &composer::ComposerPasteFiles, _window, cx| {
                                    if s.media.viewer.is_some() {
                                        return;
                                    }
                                    if !s.paste_attachments(cx) {
                                        // The clipboard has no files: let the
                                        // input widget paste ordinary text.
                                        cx.dispatch_action(&gpui_component::input::Paste);
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
                    .child(send_button),
            );
        }
        #[cfg(feature = "voxhold-media")]
        if voice_room {
            if let Some(voice) = &self.voice {
                let header_height = if mobile { 58. } else { 67. };
                let room_width = l.chat.w;
                let room_height = (l.chat.h - header_height).max(120.);
                voice.update(cx, |panel, cx| {
                    panel.set_room_size(room_width, room_height);
                    cx.notify();
                });
                chat = chat.child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(px(header_height))
                        .w(px(room_width))
                        .h(px(room_height))
                        .child(voice.clone()),
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
                            .track_focus(&self.settings_entry_focus)
                            .tab_index(0)
                            .on_click(cx.listener(|s, _, window, cx| {
                                let focus = s.settings_entry_focus.clone();
                                s.open_settings(SettingsKind::Personal, focus, window, cx);
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
                    .child(self.channels_panel(window, cx)),
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
        self.ensure_viewer_scale(window, cx);
        let popup_open = self.show_search
            || self.show_inbox
            || self.message_menu.is_some()
            || self.member_menu.is_some()
            || self.forward.is_some();
        if popup_open {
            root = root.child(
                div()
                    .id("overlay-backdrop")
                    .absolute()
                    .size_full()
                    .on_click(cx.listener(|s, _, _, cx| s.dismiss_overlays(cx))),
            );
        }
        if let Some(overlay) = self.inbox_overlay(window, cx) {
            root = root.child(overlay);
        }
        if let Some(overlay) = self.search_overlay(cx) {
            root = root.child(overlay);
        }
        if let Some(overlay) = self.forward_overlay(cx) {
            root = root.child(overlay);
        }
        if let Some(overlay) = self.message_menu_overlay(window, cx) {
            root = root.child(overlay);
        }
        if let Some(overlay) = self.member_menu_overlay(cx) {
            root = root.child(overlay);
        }
        if let Some(notice) = self.notice_overlay() {
            root = root.child(notice);
        }
        if let Some(layer) = self.settings_overlay(window, cx) {
            root = root.child(layer);
        }
        if let Some(layer) = self.recordings_overlay(window, cx) {
            root = root.child(layer);
        }
        #[cfg(feature = "voxhold-media")]
        if let Some(voice) = self.voice.clone() {
            let area =
                crate::voice_room::Rect::new(0., 0., vw, if mobile { l.bottom.y } else { vh });
            let scale = window.scale_factor();
            voice.update(cx, |panel, _| {
                panel.set_scale(scale);
                panel.set_player_area_and_place(area);
            });
            if let Some(overlay) = voice.update(cx, |panel, cx| panel.player_overlay(window, cx)) {
                root = root.child(overlay);
            }
        }
        if let Some(layer) = self.viewer_layer(window, cx) {
            root = root.child(layer);
        }
        root.into_any_element()
    }
}
