use super::*;

impl Render for Voxhold {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.finish_personal_cleanup(window, cx);
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
                if s.record_shortcut_key(event, cx) {
                    cx.stop_propagation();
                    return;
                }
                #[cfg(feature = "voxhold-media")]
                if s.screen_picker_key_down(event, window, cx) {
                    cx.stop_propagation();
                    return;
                }
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
                        if s.update.panel_open {
                            s.close_update_panel(cx);
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
                            if panel.screen_share_open() {
                                panel.screen_share_key("tab", false, window, cx);
                            } else {
                                panel.cycle_focus(false, window, cx);
                            }
                        });
                    }
                    cx.stop_propagation();
                }))
                .on_action(cx.listener(|s, _: &voice_panel::VoiceTabPrev, window, cx| {
                    if let Some(voice) = &s.voice {
                        voice.update(cx, |panel, cx| {
                            if panel.screen_share_open() {
                                panel.screen_share_key("tab", true, window, cx);
                            } else {
                                panel.cycle_focus(true, window, cx);
                            }
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
        let members_open = self.show_members && !self.personal_mode;
        let l = layout::compute_layout(vw, vh, self.shell, channels_open, members_open);
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
        let personal_empty = self.personal_mode && self.conversation.is_none();
        let selected = self.channels.iter().find(|c| c.id == self.channel).cloned();
        let readable = self.conversation.is_some() || self.channel_chat_readable(self.channel);
        let writable = self.conversation.is_some() || self.channel_chat_writable(self.channel);
        let thread_active = self.show_thread && self.thread.is_some();
        let voice_room = !personal_empty && self.voice_room_visible();
        let room_chat = voice_room && self.voice_chat_open();
        let history = if personal_empty {
            self.personal_center(window, cx)
        } else if voice_room {
            div().into_any_element()
        } else if thread_active {
            div()
                .flex_1()
                .min_h(px(0.))
                .child(self.thread.clone().expect("thread"))
                .into_any_element()
        } else {
            self.render_history(window, cx, l.chat, mobile, false)
        };
        let title = if personal_empty {
            if self.messaging.creator.is_some() {
                "Новый разговор".into()
            } else {
                "Сообщения".into()
            }
        } else if let Some(conversation) = &self.conversation {
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
        let header: AnyElement = if personal_empty {
            div()
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
                                    .id("messages-toggle")
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
                        .id("messages-return")
                        .debug_selector(|| "messages-return".into())
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(ink(p.muted))
                        .on_click(cx.listener(|s, _, window, cx| s.return_to_channels(window, cx)))
                        .child(icon("icons/hash.svg", 16., ink(p.muted)))
                        .child("К каналам"),
                )
                .into_any_element()
        } else {
            div()
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
                        .child(self.update_icon(cx))
                        .when(self.personal_mode, |row| {
                            row.child(
                                div()
                                    .id("messages-return")
                                    .debug_selector(|| "messages-return".into())
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|s, _, window, cx| {
                                        s.return_to_channels(window, cx)
                                    }))
                                    .child(icon("icons/hash.svg", 16., ink(p.muted)))
                                    .when(show_labels, |button| button.child("К каналам")),
                            )
                        })
                        .when(
                            voice_room
                                && !self.access_pending
                                && self.voice_chat_available(self.channel),
                            |row| {
                                let color = if room_chat { p.fg } else { p.muted };
                                row.child(
                                    div()
                                        .id("toggle-room-chat")
                                        .debug_selector(|| "toggle-room-chat".into())
                                        .px(px(7.))
                                        .py(px(4.))
                                        .rounded(px(7.))
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .cursor_pointer()
                                        .bg(ink(if room_chat { p.selection } else { p.surface }))
                                        .text_color(ink(color))
                                        .on_click(cx.listener(|s, _, window, cx| {
                                            s.toggle_voice_chat(window, cx)
                                        }))
                                        .child(icon("icons/chat.svg", 16., ink(color)))
                                        .when(show_labels, |button| button.child("Чат")),
                                )
                            },
                        )
                        .when(!self.personal_mode, |row| {
                            row.child(
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
                        })
                        .when(
                            self.server != 0 && !self.personal_mode && self.conversation.is_none(),
                            |row| {
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
                            },
                        )
                        .when(!self.personal_mode && !self.conversation.is_some(), |row| {
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
                        .when(!self.personal_mode, |row| {
                            row.child(
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
                            )
                        })
                        .when(self.in_conversation(), |row| {
                            let note = self
                                .messaging
                                .notifications
                                .as_ref()
                                .filter(|note| note.id == self.channel);
                            let pending = note.is_some_and(|note| note.pending);
                            let error = note.map(|note| note.error.clone()).unwrap_or_default();
                            let muted = note
                                .map(|note| note.muted)
                                .or_else(|| self.conversation.as_ref().map(|row| row.muted))
                                .unwrap_or(false);
                            let label = if pending {
                                "…"
                            } else if muted {
                                "Без звука"
                            } else {
                                "Уведомления"
                            };
                            row.child(
                                div()
                                    .id("conversation-mute")
                                    .debug_selector(|| "conversation-mute".into())
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .cursor_pointer()
                                    .when(pending, |button| button.opacity(0.55))
                                    .on_click(
                                        cx.listener(|s, _, _, cx| s.activate_conversation_mute(cx)),
                                    )
                                    .child(icon("icons/bell.svg", 16., ink(p.muted)))
                                    .when(show_labels, |button| button.child(label))
                                    .when(!error.is_empty(), |button| {
                                        button.child(
                                            div()
                                                .id("conversation-mute-error")
                                                .debug_selector(|| "conversation-mute-error".into())
                                                .max_w(px(180.))
                                                .truncate()
                                                .text_size(px(10.))
                                                .text_color(ink(p.danger))
                                                .child(error.clone()),
                                        )
                                    }),
                            )
                            .when(
                                // W08a intentionally offers leaving only for a
                                // group; a direct chat has no leave action.
                                self.conversation.as_ref().is_some_and(|chat| !chat.direct),
                                |row| {
                                    row.child(
                                        div()
                                            .id("conversation-leave")
                                            .debug_selector(|| "conversation-leave".into())
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .cursor_pointer()
                                            .text_color(ink(p.danger))
                                            .on_click(cx.listener(|s, _, _, cx| {
                                                s.request_leave_conversation(cx)
                                            }))
                                            .child("Покинуть беседу"),
                                    )
                                },
                            )
                        }),
                )
                .into_any_element()
        };
        chat = chat.child(header);
        if self.in_conversation()
            && (self.messaging.leave_confirm || !self.messaging.leave_error.is_empty())
        {
            let mut bar = div()
                .px(px(16.))
                .py(px(6.))
                .flex()
                .items_center()
                .gap_3()
                .text_size(px(12.))
                .border_b_1()
                .border_color(ink(p.line))
                .child("Покинуть беседу?");
            if !self.messaging.leave_error.is_empty() {
                bar = bar.child(
                    div()
                        .text_color(ink(p.danger))
                        .child(self.messaging.leave_error.clone()),
                );
            }
            bar = bar
                .child(
                    div()
                        .id("conversation-leave-yes")
                        .debug_selector(|| "conversation-leave-yes".into())
                        .cursor_pointer()
                        .text_color(ink(p.danger))
                        .on_click(
                            cx.listener(|s, _, window, cx| {
                                s.confirm_leave_conversation(window, cx)
                            }),
                        )
                        .child("Да"),
                )
                .child(
                    div()
                        .id("conversation-leave-no")
                        .debug_selector(|| "conversation-leave-no".into())
                        .cursor_pointer()
                        .text_color(ink(p.muted))
                        .on_click(cx.listener(|s, _, _, cx| s.cancel_leave_conversation(cx)))
                        .child("Нет"),
                );
            chat = chat.child(bar);
        }
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
        if readable && !thread_active && !voice_room {
            if let Some(attachments) = &self.attachments {
                chat = chat.child(attachments.clone());
            }
        }
        if writable && !thread_active && !voice_room {
            chat = chat.child(self.render_composer(cx, false));
        }
        #[cfg(feature = "voxhold-media")]
        if voice_room {
            if let Some(voice) = self.voice.clone() {
                let header_height = if mobile { 58. } else { 67. };
                let room_height = (l.chat.h - header_height).max(120.);
                let area = layout::Rect::new(0., 0., l.chat.w, room_height);
                let split = layout::voice_chat_layout(area, room_chat);
                voice.update(cx, |panel, cx| {
                    panel.set_room_size(split.room.w, split.room.h);
                    cx.notify();
                });
                chat = chat.child(
                    div()
                        .id("voice-room-pane")
                        .debug_selector(|| "voice-room-pane".into())
                        .absolute()
                        .left(px(split.room.x))
                        .top(px(header_height + split.room.y))
                        .w(px(split.room.w.max(1.)))
                        .h(px(split.room.h.max(1.)))
                        .overflow_hidden()
                        .child(voice),
                );
                if let Some(panel) = split.chat {
                    let absolute = layout::Rect::new(
                        l.chat.x + panel.x,
                        l.chat.y + header_height + panel.y,
                        panel.w,
                        panel.h,
                    );
                    let local =
                        layout::Rect::new(panel.x, header_height + panel.y, panel.w, panel.h);
                    chat = chat.child(self.voice_chat_panel(
                        absolute,
                        local,
                        panel.y > 0.0,
                        mobile,
                        window,
                        cx,
                    ));
                }
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
                            .child(if self.personal_mode {
                                "Сообщения"
                            } else {
                                "Каналы"
                            }),
                    )
                    .when(!self.personal_mode, |row| {
                        row.child(
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
                    })
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
            let panel = if self.personal_mode {
                self.messages_panel(window, cx)
            } else {
                self.channels_panel(window, cx)
            };
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.channels.x))
                    .top(px(l.channels.y))
                    .w(px(l.channels.w))
                    .h(px(l.channels.h))
                    .shadow_lg()
                    .child(panel),
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
            || self.update.panel_open
            || self.message_menu.is_some()
            || self.member_menu.is_some()
            || self.forward.is_some();
        if popup_open {
            root = root.child(
                div()
                    .id("overlay-backdrop")
                    .debug_selector(|| "overlay-backdrop".into())
                    .absolute()
                    .size_full()
                    .on_click(cx.listener(|s, _, _, cx| s.dismiss_overlays(cx))),
            );
        }
        // The popup sits above the backdrop so a click inside it is not a
        // dismiss, and below the modal layers that follow.
        if self.update.panel_open {
            root = root.child(self.update_panel(l.chat, mobile, cx));
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
            if let Some(overlay) =
                voice.update(cx, |panel, cx| panel.screen_share_overlay(window, cx))
            {
                root = root.child(overlay);
            }
        }
        if let Some(layer) = self.viewer_layer(window, cx) {
            root = root.child(layer);
        }
        root.into_any_element()
    }
}
