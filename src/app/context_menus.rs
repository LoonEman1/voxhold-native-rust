use super::*;
use gpui::{Div, Pixels, Point, Stateful};

#[derive(Clone)]
pub(super) struct ChannelMenu {
    server: i64,
    channel: i64,
    position: Point<Pixels>,
    return_focus: FocusHandle,
    confirm_delete: bool,
}

pub(super) fn context_menu(
    id: &'static str,
    title: &str,
    position: Point<Pixels>,
    viewport: (f32, f32),
    height: f32,
    p: theme::Palette,
) -> Stateful<Div> {
    let width = (viewport.0 - 16.).clamp(1., 264.);
    let x = f32::from(position.x).clamp(8., (viewport.0 - width - 8.).max(8.));
    let y = f32::from(position.y).clamp(8., (viewport.1 - height - 8.).max(8.));
    div()
        .id(id)
        .debug_selector(move || id.into())
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(width))
        .max_h(px((viewport.1 - y - 8.).max(1.)))
        .overflow_y_scroll()
        .occlude()
        .flex()
        .flex_col()
        .p(px(6.))
        .rounded(px(11.))
        .border_1()
        .border_color(ink(p.field_border))
        .bg(ink(if p.light { 0xf4f3ec } else { 0x232320 }))
        .shadow_lg()
        .text_color(ink(p.fg))
        .text_size(px(13.))
        .child(
            div()
                .flex_shrink_0()
                .mx(px(4.))
                .mb(px(5.))
                .px(px(6.))
                .py(px(5.))
                .truncate()
                .border_b_1()
                .border_color(ink(p.line))
                .text_color(ink(p.muted))
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::BOLD)
                .child(title.to_uppercase()),
        )
}

pub(super) fn context_item(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    danger: bool,
    p: theme::Palette,
) -> Stateful<Div> {
    let color = if danger { p.danger } else { p.fg };
    div()
        .id(id)
        .debug_selector(move || id.into())
        .h(px(32.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(9.))
        .px(px(9.))
        .rounded(px(7.))
        .cursor_pointer()
        .text_color(ink(color))
        .hover(|row| {
            row.bg(ink(if danger && p.light {
                0xfce3e0
            } else if danger {
                p.error_bg
            } else {
                p.selection
            }))
        })
        .child(icon(glyph, 16., ink(color)))
        .child(div().min_w(px(0.)).truncate().child(label))
}

pub(super) fn context_separator(p: theme::Palette) -> Div {
    div()
        .flex_shrink_0()
        .mx(px(6.))
        .my(px(5.))
        .h(px(1.))
        .bg(ink(p.line))
}

impl Voxhold {
    pub(super) fn open_channel_menu(
        &mut self,
        channel: i64,
        position: Point<Pixels>,
        return_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) {
        if self.access_pending || self.channels.iter().all(|row| row.id != channel) {
            return;
        }
        self.member_menu = None;
        self.message_menu = None;
        self.channel_menu = Some(ChannelMenu {
            server: self.server,
            channel,
            position,
            return_focus,
            confirm_delete: false,
        });
        cx.notify();
    }

    fn channel_menu_target(&self) -> Option<(&ChannelMenu, &Channel)> {
        let menu = self.channel_menu.as_ref()?;
        if menu.server != self.server || self.access_pending {
            return None;
        }
        let row = self.channels.iter().find(|row| row.id == menu.channel)?;
        Some((menu, row))
    }

    pub(super) fn open_channel_settings_menu_action(
        &mut self,
        channel: i64,
        entry: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.access_pending && self.can_manage_channel_now() {
            self.open_settings_for_channel(SettingsKind::Channel, channel, entry, window, cx);
        }
    }

    pub(super) fn channel_menu_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (menu, channel) = self.channel_menu_target()?;
        let p = self.palette;
        let can_manage = self.can_manage_channel_now();
        let voice = channel.kind == "voice";
        let chat = cfg!(feature = "voxhold-media") && voice && channel.chat_readable();
        let notifications = channel.has_history();
        let action_count = 1 + usize::from(chat || notifications) + 2 * usize::from(can_manage);
        let height = if menu.confirm_delete {
            145.
        } else {
            48. + action_count as f32 * 32. + if can_manage { 11. } else { 0. }
        };
        let mut panel = context_menu(
            "channel-menu",
            &channel.name,
            menu.position,
            self.viewport,
            height,
            p,
        );
        if menu.confirm_delete && can_manage {
            return Some(
                panel
                    .child(
                        div()
                            .px(px(9.))
                            .py(px(4.))
                            .child("Канал и его история исчезнут для всех участников."),
                    )
                    .child(
                        context_item(
                            "channel-menu-confirm-delete",
                            "Удалить канал",
                            "icons/trash.svg",
                            true,
                            p,
                        )
                        .on_click(cx.listener(|s, _, window, cx| {
                            let Some((menu, _)) = s.channel_menu_target() else {
                                return;
                            };
                            if !menu.confirm_delete || !s.can_manage_channel_now() {
                                return;
                            }
                            let menu = menu.clone();
                            s.channel_menu = None;
                            s.open_channel_settings_menu_action(
                                menu.channel,
                                menu.return_focus,
                                window,
                                cx,
                            );
                            s.channel_action(channel_settings::Action::Delete, window, cx);
                            s.channel_action(channel_settings::Action::ConfirmDelete, window, cx);
                        })),
                    )
                    .child(
                        context_item("channel-menu-cancel", "Отмена", "icons/close.svg", false, p)
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(menu) = &mut s.channel_menu {
                                    menu.confirm_delete = false;
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        let glyph = match channel.kind.as_str() {
            "voice" => "icons/volume.svg",
            "news" => "icons/news.svg",
            _ => "icons/hash.svg",
        };
        panel = panel.child(
            context_item("channel-menu-open", "Открыть канал", glyph, false, p).on_click(
                cx.listener(|s, _, window, cx| {
                    let Some((menu, _)) = s.channel_menu_target() else {
                        return;
                    };
                    let id = menu.channel;
                    s.channel_menu = None;
                    s.select_channel(id, window, cx);
                }),
            ),
        );
        #[cfg(feature = "voxhold-media")]
        if chat {
            panel = panel.child(
                context_item(
                    "channel-menu-chat",
                    "Открыть чат",
                    "icons/chat.svg",
                    false,
                    p,
                )
                .on_click(cx.listener(|s, _, window, cx| {
                    let Some((menu, row)) = s.channel_menu_target() else {
                        return;
                    };
                    let id = menu.channel;
                    if !row.chat_readable() {
                        return;
                    }
                    s.channel_menu = None;
                    if s.channel != id {
                        s.select_channel(id, window, cx);
                    }
                    if !s.voice_chat_open_for(id) {
                        s.open_voice_chat(window, cx);
                    }
                    cx.notify();
                })),
            );
        }
        if notifications {
            panel = panel.child(
                context_item(
                    "channel-menu-notifications",
                    "Уведомления",
                    "icons/bell.svg",
                    false,
                    p,
                )
                .on_click(cx.listener(|s, _, window, cx| {
                    let Some((menu, row)) = s.channel_menu_target() else {
                        return;
                    };
                    if !row.has_history() {
                        return;
                    }
                    let menu = menu.clone();
                    s.channel_menu = None;
                    s.open_settings_for_channel(
                        SettingsKind::ChannelNotifications,
                        menu.channel,
                        menu.return_focus,
                        window,
                        cx,
                    );
                })),
            );
        }
        if can_manage {
            panel = panel
                .child(context_separator(p))
                .child(
                    context_item(
                        "channel-menu-edit",
                        "Изменить канал",
                        "icons/edit.svg",
                        false,
                        p,
                    )
                    .on_click(cx.listener(|s, _, window, cx| {
                        let Some((menu, _)) = s.channel_menu_target() else {
                            return;
                        };
                        let menu = menu.clone();
                        s.channel_menu = None;
                        s.open_channel_settings_menu_action(
                            menu.channel,
                            menu.return_focus,
                            window,
                            cx,
                        );
                    })),
                )
                .child(
                    context_item(
                        "channel-menu-delete",
                        "Удалить канал",
                        "icons/trash.svg",
                        true,
                        p,
                    )
                    .on_click(cx.listener(|s, _, _, cx| {
                        if let Some(menu) = &mut s.channel_menu {
                            menu.confirm_delete = true;
                        }
                        cx.notify();
                    })),
                );
        }
        Some(panel.into_any_element())
    }
}
