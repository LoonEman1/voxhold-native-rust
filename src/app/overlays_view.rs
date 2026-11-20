use super::*;
use crate::message_tools::{self, Action};

impl Voxhold {
    pub(super) fn dismiss_overlays(&mut self, cx: &mut Context<Self>) {
        self.message_menu = None;
        self.member_menu = None;
        // A pending forward keeps its modal and its per-recipient keys.
        if !self.forward_is_sending() {
            self.forward = None;
        }
        self.show_search = false;
        self.close_edit();
        self.confirm_delete = None;
        if self.update.panel_open {
            self.close_update_panel(cx);
        }
        cx.notify();
    }

    pub(super) fn search_overlay(&mut self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.show_search {
            return None;
        }
        let panel = self.search.clone()?;
        let x = (self.viewport.0 - 380.).max(crate::layout::RAIL_WIDTH + 8.);
        Some(
            div()
                .absolute()
                .left(px(x))
                .top(px(72.))
                .child(panel)
                .into_any_element(),
        )
    }

    pub(super) fn message_menu_overlay(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.message_menu.clone()?;
        let message = self.message_snapshot(menu.message_id)?;
        let permissions = self.permissions_for_row(&message);
        let pinned = self.pinned_ids.contains(&message.id);
        let actions = message_tools::menu_actions(
            permissions,
            pinned,
            self.in_conversation(),
            self.self_user_id == message.author.user_id,
        );
        let p = self.palette;
        let x = menu.x.clamp(12., (self.viewport.0 - 300.).max(12.));
        let y = menu.y.clamp(12., (self.viewport.1 - 340.).max(12.));
        let mut panel = div()
            .id("message-menu")
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(286.))
            .occlude()
            .rounded(px(10.))
            .shadow_lg()
            .border_1()
            .border_color(ink(p.line))
            .bg(ink(p.surface))
            .text_color(ink(p.fg))
            .p_2()
            .flex()
            .flex_col()
            .gap_1();
        let mut quick = div().flex().items_center().gap_1().pb(px(4.));
        for (index, emoji) in reactions::QUICK_REACTIONS.into_iter().enumerate() {
            let active = message
                .reactions
                .iter()
                .any(|reaction| reaction.emoji == emoji && reaction.me);
            let value = emoji.to_string();
            quick = quick.child(
                div()
                    .id(("menu-reaction", index as u64))
                    .w(px(30.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .when(active, |button| button.bg(ink(p.selection)))
                    .cursor_pointer()
                    .font_family(reactions::EMOJI_FONT)
                    .on_click(cx.listener(move |s, _, _, cx| {
                        let active = s.message_snapshot(menu.message_id).is_some_and(|message| {
                            message
                                .reactions
                                .iter()
                                .any(|reaction| reaction.emoji == value && reaction.me)
                        });
                        s.message_menu = None;
                        s.toggle_reaction(menu.message_id, value.clone(), active, cx);
                    }))
                    .child(emoji),
            );
        }
        quick = quick.child(
            div()
                .id("menu-more-reactions")
                .flex_1()
                .h(px(28.))
                .flex()
                .items_center()
                .justify_end()
                .cursor_pointer()
                .text_size(px(11.))
                .text_color(ink(p.muted))
                .on_click(cx.listener(|s, _, _, cx| {
                    if let Some(menu) = s.message_menu.as_mut() {
                        menu.expanded = !menu.expanded;
                        cx.notify();
                    }
                }))
                .child(if menu.expanded {
                    "Скрыть"
                } else {
                    "Все"
                }),
        );
        panel = panel.child(quick);
        if menu.expanded {
            panel = panel.child(
                div()
                    .id("menu-reaction-picker")
                    .max_h(px(230.))
                    .overflow_y_scroll()
                    .child(self.reaction_picker.clone()),
            );
        }
        if menu.confirm_delete {
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_1()
                    .text_size(px(12.))
                    .child("Удалить сообщение?")
                    .child(
                        div()
                            .id("menu-delete-no")
                            .cursor_pointer()
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(menu) = s.message_menu.as_mut() {
                                    menu.confirm_delete = false;
                                    cx.notify();
                                }
                            }))
                            .child("Нет"),
                    )
                    .child(
                        div()
                            .id("menu-delete-yes")
                            .cursor_pointer()
                            .text_color(ink(p.danger))
                            .on_click(cx.listener(move |s, _, window, cx| {
                                s.confirm_delete_now(menu.message_id, window, cx);
                            }))
                            .child("Да"),
                    ),
            );
        } else {
            for action in actions {
                let label = match action {
                    Action::Discuss if message.comment_count > 0 => {
                        format!("Обсуждение ({})", message.comment_count)
                    }
                    other => other.label().to_string(),
                };
                let danger = action == Action::Delete || action == Action::Block;
                let pending_kind = if matches!(action, Action::Pin | Action::Unpin) {
                    "pin"
                } else {
                    action.id()
                };
                panel = panel.child(
                    div()
                        .id(gpui::ElementId::Name(
                            format!("menu-action-{}", action.id()).into(),
                        ))
                        .h(px(30.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .when(danger, |row| row.text_color(ink(p.danger)))
                        .when(self.action_pending(menu.message_id, pending_kind), |row| {
                            row.opacity(0.5)
                        })
                        .hover(|row| row.bg(ink(p.selection)))
                        .on_click(cx.listener(move |s, _, window, cx| {
                            s.dispatch_message_action(menu.message_id, action, window, cx);
                            if !matches!(action, Action::Delete) {
                                s.message_menu = None;
                            }
                            cx.notify();
                        }))
                        .child(label),
                );
            }
        }
        if let Some((_, error)) = self
            .action_error
            .as_ref()
            .filter(|(id, _)| *id == menu.message_id)
        {
            panel = panel.child(
                div()
                    .text_size(px(11.))
                    .text_color(ink(p.danger))
                    .child(error.clone()),
            );
        }
        Some(panel.into_any_element())
    }

    pub(super) fn forward_overlay(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.forward.clone()?;
        let p = self.palette;
        let x = (self.viewport.0 / 2. - 180.).max(12.);
        let y = (self.viewport.1 / 2. - 180.).max(12.);
        let mut panel = div()
            .id("forward-panel")
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(360.))
            .max_h(px(360.))
            .occlude()
            .rounded(px(10.))
            .shadow_lg()
            .border_1()
            .border_color(ink(p.line))
            .bg(ink(p.surface))
            .text_color(ink(p.fg))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Переслать сообщение"),
                    )
                    .child(
                        div()
                            .id("forward-close")
                            .cursor_pointer()
                            .text_size(px(12.))
                            .on_click(cx.listener(|s, _, _, cx| s.close_forward(cx)))
                            .child("Закрыть"),
                    ),
            );
        if state.loading {
            panel = panel.child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Загрузка…"),
            );
        } else if !state.error.is_empty() {
            panel = panel.child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.danger))
                    .child(state.error.clone()),
            );
        } else if state.destinations.is_empty() {
            panel = panel.child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Нет доступных мест для пересылки"),
            );
        }
        let mut list = div()
            .id("forward-list")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for (index, destination) in state.destinations.iter().enumerate() {
            let label = format!(
                "{}: {}",
                if destination.kind == "channel" {
                    "Канал"
                } else {
                    "Диалог"
                },
                destination.name
            );
            list = list.child(
                div()
                    .id(("forward-destination", index as u64))
                    .h(px(30.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .hover(|row| row.bg(ink(p.selection)))
                    .on_click(cx.listener(move |s, _, window, cx| {
                        if !s.forward.as_ref().is_some_and(|state| state.sending) {
                            s.forward_to(index, window, cx);
                        }
                    }))
                    .child(label),
            );
        }
        panel = panel.child(list);
        if state.sending {
            panel = panel.child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Пересылка…"),
            );
        }
        Some(panel.into_any_element())
    }

    pub(super) fn notice_overlay(&self) -> Option<AnyElement> {
        if self.action_notice.is_empty() && self.remember_error.is_empty() {
            return None;
        }
        let p = self.palette;
        let mut column = div()
            .absolute()
            .right(px(20.))
            .bottom(px(90.))
            .flex()
            .flex_col()
            .items_end()
            .gap(px(6.));
        if !self.remember_error.is_empty() {
            column = column.child(
                div()
                    .debug_selector(|| "remember-error".into())
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(7.))
                    .bg(ink(p.selection))
                    .text_size(px(12.))
                    .text_color(ink(p.fg))
                    .child(self.remember_error.clone()),
            );
        }
        if !self.action_notice.is_empty() {
            column = column.child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(7.))
                    .bg(ink(p.selection))
                    .text_size(px(12.))
                    .text_color(ink(p.fg))
                    .child(self.action_notice.clone()),
            );
        }
        Some(column.into_any_element())
    }
}
