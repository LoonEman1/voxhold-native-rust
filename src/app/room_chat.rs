//! Text chat of the selected voice room. Viewing is gated by the channel's
//! `Read` permission independently of the audio call, and the panel reuses the
//! regular history, composer and draft paths under `ChatKey::channel`.
//!
//! The panel is part of the room surface, not the channel column: it is docked
//! beside the room or stacked under it, never painted on top of the room tiles.

use super::*;

impl Voxhold {
    fn channel_row(&self, channel: i64) -> Option<&Channel> {
        self.channels.iter().find(|row| row.id == channel)
    }

    /// Whether the selected channel's timeline can be shown when its surface
    /// is open. Voice rooms are only readable while their chat panel is open.
    pub(super) fn channel_chat_readable(&self, channel: i64) -> bool {
        if self.access_pending || channel <= 0 {
            return false;
        }
        let Some(row) = self.channel_row(channel) else {
            return false;
        };
        if row.kind == "voice" {
            return self.voice_chat_selected(channel);
        }
        row.has_history()
    }

    pub(super) fn channel_chat_writable(&self, channel: i64) -> bool {
        if !self.channel_chat_readable(channel) {
            return false;
        }
        self.channel_row(channel)
            .is_some_and(|row| row.chat_writable())
    }

    /// Subscribe to the channel only while its timeline is actually visible and
    /// permitted. A closed room chat or a personal conversation keeps the
    /// account socket unsubscribed.
    pub(super) fn refresh_chat_subscription(&self) {
        let enabled = !self.in_conversation() && self.channel_chat_readable(self.channel);
        self.update_channel_subscription(enabled);
    }

    #[cfg(feature = "voxhold-media")]
    pub(super) fn voice_chat_available(&self, channel: i64) -> bool {
        self.channel_row(channel)
            .is_some_and(|row| row.kind == "voice" && row.chat_readable())
    }

    /// Open state of one room, independent of what is currently selected.
    #[cfg(feature = "voxhold-media")]
    pub(super) fn voice_chat_open_for(&self, channel: i64) -> bool {
        self.voice_chat_available(channel)
            && self.voice_chat_rooms.contains(&(self.server, channel))
    }

    #[cfg(not(feature = "voxhold-media"))]
    pub(super) fn voice_chat_open_for(&self, _channel: i64) -> bool {
        false
    }

    /// The room chat is visible only for the selected voice room.
    #[cfg(feature = "voxhold-media")]
    pub(super) fn voice_chat_open(&self) -> bool {
        self.voice_room_visible() && self.voice_chat_open_for(self.channel)
    }

    #[cfg(not(feature = "voxhold-media"))]
    pub(super) fn voice_chat_open(&self) -> bool {
        false
    }

    /// Membership check used by [`Self::channel_chat_readable`] without the
    /// extra "room is selected" condition.
    #[cfg(feature = "voxhold-media")]
    fn voice_chat_selected(&self, channel: i64) -> bool {
        self.voice_chat_open_for(channel)
    }

    #[cfg(not(feature = "voxhold-media"))]
    fn voice_chat_selected(&self, _channel: i64) -> bool {
        false
    }

    #[cfg(feature = "voxhold-media")]
    pub(super) fn open_voice_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let channel = self.channel;
        if !self.voice_room_visible() || !self.voice_chat_available(channel) || self.access_pending
        {
            return;
        }
        let key = (self.server, channel);
        self.voice_chat_rooms.insert(key);
        self.chat_scope_epoch = self.chat_scope_epoch.wrapping_add(1);
        let writable = self.channel_chat_writable(channel);
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |panel, cx| {
                panel.set_scope(
                    Some(draft::ChatKey::channel(self.server, channel)),
                    writable,
                    cx,
                )
            });
        }
        // Opening is the moment the channel becomes a visible timeline: take
        // the subscription and bring the history up to date after a closed
        // period in which no events arrived for it.
        self.refresh_chat_subscription();
        self.refresh_history(window, cx);
        cx.notify();
    }

    #[cfg(feature = "voxhold-media")]
    pub(super) fn close_voice_chat(&mut self, cx: &mut Context<Self>) {
        let key = (self.server, self.channel);
        if !self.voice_chat_rooms.remove(&key) {
            return;
        }
        // A frame already queued by `schedule_visible_read` checks this epoch,
        // so closing cannot mark the hidden timeline as read.
        self.chat_scope_epoch = self.chat_scope_epoch.wrapping_add(1);
        self.read_scheduled = false;
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |panel, cx| {
                panel.set_scope(
                    Some(draft::ChatKey::channel(self.server, self.channel)),
                    false,
                    cx,
                )
            });
        }
        self.refresh_chat_subscription();
        cx.notify();
    }

    #[cfg(feature = "voxhold-media")]
    pub(super) fn toggle_voice_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice_chat_open() {
            self.close_voice_chat(cx);
        } else {
            self.open_voice_chat(window, cx);
        }
    }

    /// Drop room-chat state for channels that no longer allow viewing. Called
    /// after every confirmed channel list, before the room pane is rebuilt.
    #[cfg(feature = "voxhold-media")]
    pub(super) fn prune_voice_chat(&mut self) {
        let server = self.server;
        let channels = &self.channels;
        self.voice_chat_rooms.retain(|(room_server, channel)| {
            if *room_server != server {
                return true;
            }
            channels
                .iter()
                .any(|row| row.id == *channel && row.kind == "voice" && row.chat_readable())
        });
    }

    /// Docked or stacked room chat. `absolute` is the panel rectangle in window
    /// coordinates (the history scrollbar needs it); `local` is the same
    /// rectangle inside the chat column.
    #[cfg(feature = "voxhold-media")]
    pub(super) fn voice_chat_panel(
        &mut self,
        absolute: layout::Rect,
        local: layout::Rect,
        stacked: bool,
        mobile: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let mut aside = div()
            .id("voice-chat-panel")
            .debug_selector(|| "voice-chat-panel".into())
            .absolute()
            .left(px(local.x))
            .top(px(local.y))
            .w(px(local.w.max(1.)))
            .h(px(local.h.max(1.)))
            .flex()
            .flex_col()
            .bg(ink(p.surface))
            .border_color(ink(p.line))
            .shadow_lg()
            .when(stacked, |panel| panel.border_t_1())
            .when(!stacked, |panel| panel.border_l_1());
        let close = div()
            .id("voice-chat-close")
            .debug_selector(|| "voice-chat-close".into())
            .cursor_pointer()
            .text_color(ink(p.muted))
            .child(icon("icons/close.svg", 16., ink(p.muted)));
        aside = aside.child(
            div()
                .h(px(48.))
                .px(px(16.))
                .flex()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(ink(p.line))
                .child(
                    div()
                        .id("voice-chat-tab")
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(7.))
                        .bg(ink(p.selection))
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Чат"),
                )
                .child(close.on_click(cx.listener(|s, _, _, cx| s.close_voice_chat(cx)))),
        );
        if self.access_pending {
            let status = div().p_2().text_size(px(12.)).text_color(ink(p.muted));
            let status = if self.access_reload_in_flight {
                status.child("Проверяем доступ…")
            } else {
                status.child(
                    div()
                        .id("retry-room-chat-access")
                        .cursor_pointer()
                        .child("Повторить проверку доступа")
                        .on_click(cx.listener(|s, _, window, cx| s.invalidate_access(window, cx))),
                )
            };
            return aside.child(status).into_any_element();
        }
        if !self.error.is_empty() {
            aside = aside.child(
                div()
                    .px(px(12.))
                    .py(px(4.))
                    .text_size(px(12.))
                    .text_color(ink(p.danger))
                    .child(self.error.clone()),
            );
        }
        let readable = self.channel_chat_readable(self.channel);
        let writable = self.channel_chat_writable(self.channel);
        if !readable {
            return aside
                .child(
                    div()
                        .p_2()
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .child("Чат комнаты недоступен."),
                )
                .into_any_element();
        }
        aside = aside.child(self.render_history(window, cx, absolute, mobile, true));
        if let Some(attachments) = &self.attachments {
            aside = aside.child(attachments.clone());
        }
        if writable {
            aside = aside.child(self.render_composer(cx, true));
        }
        aside.into_any_element()
    }
}
