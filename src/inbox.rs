//! Recent personal conversations. The list is owned by the workspace (the
//! same data feeds the rail/inbox); this entity only paints the popup and
//! forwards a click. Titles follow Go `recents.go::conversationTile`.

use gpui::{div, prelude::*, px, App, Context, Render, Window};
use std::sync::Arc;
use voxhold_api::Conversation;

#[derive(Clone)]
pub struct ConversationTile {
    pub id: i64,
    pub title: String,
    pub direct: bool,
    pub unread: i64,
    pub muted: bool,
    pub peer_id: i64,
    pub avatar: Option<Arc<gpui::RenderImage>>,
}

pub struct Inbox {
    tiles: Vec<ConversationTile>,
    loading: bool,
    error: String,
    on_select: Option<Box<dyn Fn(i64, &mut Window, &mut App) + 'static>>,
    on_refresh: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl Inbox {
    pub fn new() -> Self {
        Self {
            tiles: Vec::new(),
            loading: false,
            error: String::new(),
            on_select: None,
            on_refresh: None,
        }
    }

    pub fn set_select_handler(
        &mut self,
        handler: Box<dyn Fn(i64, &mut Window, &mut App) + 'static>,
    ) {
        self.on_select = Some(handler);
    }

    pub fn set_refresh_handler(&mut self, handler: Box<dyn Fn(&mut Window, &mut App) + 'static>) {
        self.on_refresh = Some(handler);
    }

    pub fn set_loading(&mut self, loading: bool, cx: &mut Context<Self>) {
        self.loading = loading;
        cx.notify();
    }

    pub fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = error;
        cx.notify();
    }

    pub fn set_tiles(&mut self, tiles: Vec<ConversationTile>, cx: &mut Context<Self>) {
        self.tiles = tiles;
        self.loading = false;
        self.error.clear();
        cx.notify();
    }
}

/// Go `conversationTile`: an explicit group name wins, a direct chat uses the
/// other member, and a nameless multi-person chat falls back to "Беседа".
pub fn conversation_title(conversation: &Conversation, self_user_id: i64) -> String {
    let mut names = Vec::new();
    for person in &conversation.members {
        if self_user_id > 0 && person.user_id == self_user_id {
            continue;
        }
        names.push(person.username.clone());
    }
    if !conversation.name.is_empty() {
        return conversation.name.clone();
    }
    if names.is_empty() {
        return "Беседа".into();
    }
    names.join(", ")
}

pub fn conversation_peer(conversation: &Conversation, self_user_id: i64) -> i64 {
    if !conversation.direct {
        return 0;
    }
    conversation
        .members
        .iter()
        .find(|person| person.user_id != self_user_id)
        .map(|person| person.user_id)
        .unwrap_or(0)
}

impl Render for Inbox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::theme::dark_palette();
        let mut list = div().flex().flex_col().gap_1();
        if self.loading {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(crate::ink(p.muted))
                    .child("Загрузка…"),
            );
        } else if !self.error.is_empty() {
            list = list.child(
                div()
                    .id("inbox-retry")
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(crate::ink(p.danger))
                    .on_click(cx.listener(|s, _, window, cx| {
                        if let Some(handler) = &s.on_refresh {
                            handler(window, cx);
                        }
                    }))
                    .child(format!("{} · Повторить", self.error)),
            );
        } else if self.tiles.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(crate::ink(p.muted))
                    .child("Личных диалогов пока нет"),
            );
        }
        for tile in &self.tiles {
            let id = tile.id;
            let face = if let Some(image) = tile.avatar.clone() {
                div()
                    .w(px(30.))
                    .h(px(30.))
                    .flex_shrink_0()
                    .rounded_full()
                    .overflow_hidden()
                    .child(
                        gpui::img(image)
                            .size_full()
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                    .into_any_element()
            } else {
                div()
                    .w(px(30.))
                    .h(px(30.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(crate::ink(crate::members::avatar_color(&tile.title)))
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(crate::ink(0x151510))
                    .child(crate::members::initials(&tile.title))
                    .into_any_element()
            };
            list = list.child(
                div()
                    .id(("conversation", id as u64))
                    .h(px(38.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(7.))
                    .cursor_pointer()
                    .hover(|row| row.bg(crate::ink(p.selection)))
                    .child(face)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(13.))
                            .child(tile.title.clone()),
                    )
                    .when(tile.muted, |row| {
                        row.child(
                            div()
                                .text_size(px(10.))
                                .text_color(crate::ink(p.muted))
                                .child("тихо"),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(crate::ink(p.muted))
                            .child(if tile.direct { "ЛС" } else { "Группа" }),
                    )
                    .when(tile.unread > 0, |row| {
                        row.child(
                            div()
                                .min_w(px(18.))
                                .h(px(18.))
                                .px(px(5.))
                                .rounded_full()
                                .bg(crate::ink(p.lime))
                                .text_size(px(10.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(crate::ink(0x10100f))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(tile.unread.min(99).to_string()),
                        )
                    })
                    .on_click(cx.listener(move |s, _, window, cx| {
                        if let Some(handler) = &s.on_select {
                            handler(id, window, cx);
                        }
                    })),
            );
        }
        div()
            .id("inbox-popup")
            .w(px(320.))
            .max_h(px(440.))
            .occlude()
            .rounded(px(10.))
            .shadow_lg()
            .border_1()
            .border_color(crate::ink(p.line))
            .bg(crate::ink(p.surface))
            .text_color(crate::ink(p.fg))
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
                            .child("Личные диалоги"),
                    )
                    .child(
                        div()
                            .id("inbox-refresh")
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(crate::ink(p.muted))
                            .on_click(cx.listener(|s, _, window, cx| {
                                if let Some(handler) = &s.on_refresh {
                                    handler(window, cx);
                                }
                            }))
                            .child("Обновить"),
                    ),
            )
            .child(
                div()
                    .id("inbox-list")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(list),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxhold_api::Author;

    fn conversation(name: &str, direct: bool, members: Vec<(i64, &str)>) -> Conversation {
        Conversation {
            id: 7,
            name: name.into(),
            direct,
            members: members
                .into_iter()
                .map(|(user_id, username)| Author {
                    user_id,
                    username: username.into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn direct_and_group_titles_follow_go_recents() {
        let direct = conversation("", true, vec![(1, "me"), (2, "neva")]);
        assert_eq!(conversation_title(&direct, 1), "neva");
        assert_eq!(conversation_peer(&direct, 1), 2);

        let group = conversation("", false, vec![(1, "me"), (2, "neva"), (3, "alex")]);
        assert_eq!(conversation_title(&group, 1), "neva, alex");
        assert_eq!(conversation_peer(&group, 1), 0);

        let named = conversation("Проект", true, vec![(1, "me"), (2, "neva")]);
        assert_eq!(conversation_title(&named, 1), "Проект");

        let empty = conversation("", false, vec![(1, "me")]);
        assert_eq!(conversation_title(&empty, 1), "Беседа");
    }
}
