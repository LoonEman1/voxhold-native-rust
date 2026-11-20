//! Personal "Сообщения" sidebar and center. The pinned web has a client-side
//! name filter in `CommunitySidebar` but no UI for
//! `GET /me/conversations/search`; W08a requires the confirmed endpoint, so the
//! smallest honest surface is a toggle on the same input that submits the
//! message query and lists its hits. The endpoint and cursor remain the
//! backend contract, not a second channel search.

use super::*;
use crate::messages;

impl Voxhold {
    fn conversation_label(&self, id: i64) -> String {
        self.conversations
            .iter()
            .find(|row| row.id == id)
            .map(|row| inbox::conversation_title(row, self.self_user_id))
            .unwrap_or_else(|| format!("Беседа #{id}"))
    }

    /// The personal sidebar replaces the channel list while the rail and the
    /// server/channel context stay alive in the workspace.
    pub(super) fn messages_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let filter = self.messaging.query.read(cx).value().trim().to_string();
        let rows = messages::filter_names(&self.conversations, self.self_user_id, &filter);
        let mut tiles = Vec::with_capacity(rows.len());
        let mut wanted = Vec::new();
        for row in rows {
            let title = inbox::conversation_title(&row, self.self_user_id);
            let peer = inbox::conversation_peer(&row, self.self_user_id);
            let version = self
                .members
                .iter()
                .find(|member| member.user_id == peer)
                .and_then(|member| member.avatar_version.clone());
            if let Some(version) = version.as_ref().filter(|version| !version.is_empty()) {
                wanted.push((peer, version.clone()));
            }
            tiles.push(PersonalRow {
                id: row.id,
                title,
                direct: row.direct,
                members: row.members.len(),
                unread: row.unread_count,
                muted: row.muted,
                peer,
                avatar: None,
            });
        }
        for (user_id, version) in wanted {
            self.request_avatar(user_id, version, window, cx);
        }
        for tile in &mut tiles {
            let Some(version) = self
                .members
                .iter()
                .find(|member| member.user_id == tile.peer)
                .and_then(|member| member.avatar_version.clone())
            else {
                continue;
            };
            if let Some((image, used)) = self
                .roster
                .avatars
                .get(&(tile.peer, version.clone()))
                .cloned()
            {
                if let Some((_, used)) = self.roster.avatars.get_mut(&(tile.peer, version)) {
                    *used = std::time::Instant::now();
                }
                let _ = used;
                tile.avatar = Some(image);
            }
        }

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
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(ink(p.muted))
                                    .child("ЛИЧНОЕ ОБЩЕНИЕ"),
                            )
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("Сообщения"),
                            ),
                    )
                    .child(
                        div()
                            .id("messages-return-sidebar")
                            .debug_selector(|| "messages-return-sidebar".into())
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|s, _, window, cx| s.return_to_channels(window, cx)),
                            )
                            .child(icon("icons/hash.svg", 17., ink(p.muted))),
                    ),
            )
            .child(
                div()
                    .px(px(16.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(ink(p.muted))
                            .child("РАЗГОВОРЫ"),
                    )
                    .child(
                        div()
                            .id("conversation-create")
                            .debug_selector(|| "conversation-create".into())
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, window, cx| {
                                s.open_conversation_creator(window, cx)
                            }))
                            .child(icon("icons/add.svg", 17., ink(p.muted))),
                    ),
            )
            .child(
                div()
                    .mx(px(12.))
                    .mb(px(6.))
                    .px(px(8.))
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(7.))
                    .bg(ink(p.bg))
                    .child(icon("icons/search.svg", 14., ink(p.muted)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(TextInput::new(&self.messaging.query)),
                    ),
            )
            .child(
                div()
                    .px(px(16.))
                    .pb(px(6.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .child(
                        div()
                            .id("messages-search-mode")
                            .debug_selector(|| "messages-search-mode".into())
                            .cursor_pointer()
                            .child(if self.messaging.global {
                                "Фильтр по имени"
                            } else {
                                "Поиск по сообщениям"
                            })
                            .on_click(cx.listener(|s, _, window, cx| {
                                s.toggle_messages_search_mode(window, cx)
                            })),
                    )
                    .when(self.messaging.global, |row| {
                        row.child(
                            div()
                                .id("messages-search-submit")
                                .debug_selector(|| "messages-search-submit".into())
                                .cursor_pointer()
                                .on_click(cx.listener(|s, _, _, cx| s.submit_messages_search(cx)))
                                .child("Найти"),
                        )
                    }),
            );

        let body = if self.messaging.global {
            self.messages_search_body(cx)
        } else {
            let mut list = div().flex().flex_col().px(px(8.)).gap(px(2.));
            if self.messaging.loading {
                list = list.child(
                    div()
                        .id("messages-index-loading")
                        .debug_selector(|| "messages-index-loading".into())
                        .px(px(8.))
                        .py(px(10.))
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .child("Загрузка…"),
                );
            }
            if tiles.is_empty() {
                if !self.messaging.loading {
                    list = list.child(
                        div()
                            .px(px(8.))
                            .py(px(10.))
                            .text_size(px(12.))
                            .text_color(ink(p.muted))
                            .child(if filter.is_empty() {
                                "Начните разговор через + или профиль участника."
                            } else {
                                "Разговоры не найдены"
                            }),
                    );
                }
            } else {
                for tile in &tiles {
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
                    } else if tile.direct {
                        div()
                            .w(px(30.))
                            .h(px(30.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(ink(crate::members::avatar_color(&tile.title)))
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(ink(0x151510))
                            .child(crate::members::initials(&tile.title))
                            .into_any_element()
                    } else {
                        div()
                            .w(px(30.))
                            .h(px(30.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(9.))
                            .bg(ink(p.selection))
                            .child(icon("icons/people.svg", 17., ink(p.muted)))
                            .into_any_element()
                    };
                    let active = self.conversation.as_ref().map(|row| row.id) == Some(id);
                    list = list.child(
                        div()
                            .id(("personal-conversation", id as u64))
                            .debug_selector({
                                move || format!("personal-conversation-{id}").into()
                            })
                            .h(px(44.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(7.))
                            .cursor_pointer()
                            .when(active, |row| row.bg(ink(p.selection)))
                            .hover(|row| row.bg(ink(p.selection)))
                            .child(face)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(13.))
                                            .text_color(ink(p.fg))
                                            .child(tile.title.clone()),
                                    )
                                    .child(
                                        div().text_size(px(10.)).text_color(ink(p.muted)).child(
                                            if tile.direct {
                                                "Личный диалог".to_string()
                                            } else {
                                                format!("{} участников", tile.members)
                                            },
                                        ),
                                    ),
                            )
                            .when(tile.muted, |row| {
                                row.child(
                                    div()
                                        .text_size(px(9.))
                                        .text_color(ink(p.muted))
                                        .child("тихо"),
                                )
                            })
                            .when(tile.unread > 0, |row| {
                                row.child(
                                    div()
                                        .min_w(px(18.))
                                        .h(px(18.))
                                        .px(px(5.))
                                        .rounded_full()
                                        .bg(ink(p.lime))
                                        .text_size(px(10.))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(ink(0x10100f))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(tile.unread.min(99).to_string()),
                                )
                            })
                            .on_click(cx.listener(move |s, _, window, cx| {
                                if let Some(row) =
                                    s.conversations.iter().find(|row| row.id == id).cloned()
                                {
                                    s.select_conversation(row, window, cx);
                                }
                            })),
                    );
                }
            }
            list.into_any_element()
        };

        panel = panel.child(
            div()
                .id("personal-conversation-list")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(body),
        );
        if !self.messaging.global {
            if self.messaging.older_loading {
                panel = panel.child(
                    div()
                        .px(px(16.))
                        .py(px(6.))
                        .text_size(px(11.))
                        .text_color(ink(p.muted))
                        .child("Загрузка…"),
                );
            } else if !self.messaging.older_error.is_empty() {
                panel = panel.child(
                    div()
                        .px(px(16.))
                        .py(px(6.))
                        .text_size(px(11.))
                        .text_color(ink(p.danger))
                        .child(self.messaging.older_error.clone()),
                );
            } else if self.messaging.has_more {
                panel = panel.child(
                    div()
                        .id("conversation-older")
                        .debug_selector(|| "conversation-older".into())
                        .mx(px(16.))
                        .my(px(6.))
                        .cursor_pointer()
                        .text_size(px(11.))
                        .text_color(ink(p.muted))
                        .on_click(cx.listener(|s, _, _, cx| s.load_older_conversations(cx)))
                        .child("Показать ещё"),
                );
            }
        }
        if !self.messaging.error.is_empty() {
            let error = self.messaging.error.clone();
            panel = panel.child(
                div()
                    .px(px(16.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_color(ink(p.danger))
                            .child(error),
                    )
                    .when(!self.messaging.loading, |row| {
                        row.child(
                            div()
                                .id("messages-index-retry")
                                .debug_selector(|| "messages-index-retry".into())
                                .cursor_pointer()
                                .text_color(ink(p.muted))
                                .on_click(cx.listener(|s, _, _, cx| s.refresh_recents(cx)))
                                .child("Повторить"),
                        )
                    }),
            );
        }
        #[cfg(feature = "voxhold-media")]
        if let Some(dock) = self.voice_dock(cx) {
            panel = panel.child(dock);
        }
        panel
            .child(
                div()
                    .id("messages-return-footer")
                    .debug_selector(|| "messages-return-footer".into())
                    .h(px(40.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(ink(p.line))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .on_click(cx.listener(|s, _, window, cx| s.return_to_channels(window, cx)))
                    .child(icon("icons/hash.svg", 14., ink(p.muted)))
                    .child("Вернуться в сообщество"),
            )
            .child(self.account_footer(cx))
            .into_any_element()
    }

    fn messages_search_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let mut body = div().flex().flex_col().px(px(12.)).gap(px(4.));
        if self.messaging.search.loading {
            body = body.child(
                div()
                    .py(px(8.))
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .child("Поиск…"),
            );
        } else if !self.messaging.search.error.is_empty() {
            body = body.child(
                div()
                    .py(px(8.))
                    .text_size(px(11.))
                    .text_color(ink(p.danger))
                    .child(self.messaging.search.error.clone()),
            );
        } else if self.messaging.search.results.is_empty()
            && !self.messaging.search.submitted.is_empty()
        {
            body = body.child(
                div()
                    .py(px(8.))
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .child("Ничего не найдено"),
            );
        } else if self.messaging.search.submitted.is_empty() {
            body = body.child(
                div()
                    .py(px(8.))
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .child("Введите запрос и нажмите Enter"),
            );
        }
        for hit in &self.messaging.search.results {
            let conversation = hit.conversation_id;
            let message = hit.id;
            let title = self.conversation_label(conversation);
            let author = hit.author_username.clone();
            let preview = messages::search_preview(&hit.content);
            body = body.child(
                div()
                    .id(("conversation-search-hit", hit.id as u64))
                    .debug_selector({
                        let id = hit.id;
                        move || format!("conversation-search-hit-{id}").into()
                    })
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .hover(|row| row.bg(ink(p.selection)))
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.open_conversation_message(conversation, message, window, cx)
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(ink(p.muted))
                                    .child(author),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(ink(p.muted))
                            .child(preview),
                    ),
            );
        }
        if self.messaging.search.loading_more {
            body = body.child(
                div()
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .child("Загрузка…"),
            );
        } else if self.messaging.search.has_more {
            body = body.child(
                div()
                    .id("conversation-search-more")
                    .cursor_pointer()
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .on_click(cx.listener(|s, _, _, cx| s.load_more_messages_search(cx)))
                    .child("Ещё результаты"),
            );
        }
        body.into_any_element()
    }

    /// The center of the personal surface: the creator form, or the honest
    /// empty state when no conversation is selected.
    pub(super) fn personal_center(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let Some(creator) = self.messaging.creator.as_ref() else {
            return div()
                .id("personal-empty")
                .flex_1()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(ink(p.muted))
                .child(icon("icons/message.svg", 34., ink(p.muted)))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(p.fg))
                        .child("Ваши разговоры"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .child("Выберите диалог или беседу слева."),
                )
                .into_any_element();
        };

        let selected = creator.selected.clone();
        let busy = creator.busy;
        let error = creator.error.clone();
        let name_input = creator.name.clone();
        let search_input = creator.search.clone();
        let needle = search_input.read(cx).value().trim().to_lowercase();
        let group = selected.len() > 1;

        let mut people: Vec<(i64, String)> = Vec::new();
        for member in &self.members {
            if member.user_id == self.self_user_id || member.user_id <= 0 {
                continue;
            }
            if !people.iter().any(|(id, _)| *id == member.user_id) {
                people.push((member.user_id, member.username.clone()));
            }
        }
        for row in &self.conversations {
            for member in &row.members {
                if member.user_id == self.self_user_id || member.user_id <= 0 {
                    continue;
                }
                if !people.iter().any(|(id, _)| *id == member.user_id) {
                    people.push((member.user_id, member.username.clone()));
                }
            }
        }
        people.retain(|(_, name)| needle.is_empty() || name.to_lowercase().contains(&needle));

        let mut page = div()
            .id("conversation-creator")
            .flex_1()
            .size_full()
            .overflow_y_scroll()
            .p(px(22.))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(17.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(if group {
                        "Создать беседу"
                    } else {
                        "С кем поговорим?"
                    }),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Выберите одного человека для личного диалога или нескольких для беседы. Всего можно добавить до 15 участников."),
            )
            .child(
                div()
                    .h(px(32.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(7.))
                    .bg(ink(p.surface))
                    .border_1()
                    .border_color(ink(p.line))
                    .child(icon("icons/search.svg", 15., ink(p.muted)))
                    .child(div().flex_1().child(TextInput::new(&search_input))),
            );
        let mut list = div()
            .id("creator-people")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(300.))
            .overflow_y_scroll();
        if people.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Никого не нашли по этому имени."),
            );
        }
        for (id, name) in people {
            let checked = selected.contains(&id);
            let blocked = !checked && selected.len() >= voxhold_api::MAX_USERS;
            list = list.child(
                div()
                    .id(("creator-member", id as u64))
                    .debug_selector(move || format!("creator-member-{id}").into())
                    .h(px(34.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(7.))
                    .cursor_pointer()
                    .when(checked, |row| row.bg(ink(p.selection)))
                    .when(blocked, |row| row.opacity(0.45))
                    .hover(|row| row.bg(ink(p.selection)))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        if !blocked {
                            s.toggle_creator_member(id, cx);
                        }
                    }))
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(16.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(ink(if checked { p.lime } else { p.line }))
                            .when(checked, |box_| box_.bg(ink(p.lime)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(if checked {
                                icon("icons/check.svg", 11., ink(0x10100f))
                            } else {
                                div().into_any_element()
                            }),
                    )
                    .child(div().flex_1().truncate().text_size(px(13.)).child(name)),
            );
        }
        page = page.child(list);
        if group {
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(ink(p.muted))
                            .child("Название беседы · необязательно"),
                    )
                    .child(
                        div()
                            .h(px(32.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(7.))
                            .bg(ink(p.surface))
                            .border_1()
                            .border_color(ink(p.line))
                            .child(TextInput::new(&name_input)),
                    ),
            );
        }
        if !error.is_empty() {
            page = page.child(
                div()
                    .text_size(px(11.))
                    .text_color(ink(p.danger))
                    .child(error),
            );
        }
        page.child(
            div()
                .id("creator-submit")
                .debug_selector(|| "creator-submit".into())
                .h(px(34.))
                .px(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.))
                .bg(ink(if selected.is_empty() || busy {
                    p.selection
                } else {
                    p.lime
                }))
                .text_color(ink(if selected.is_empty() || busy {
                    p.muted
                } else {
                    0x10100f
                }))
                .cursor_pointer()
                .when(busy, |button| button.opacity(0.6))
                .on_click(cx.listener(|s, _, window, cx| s.submit_conversation_creator(window, cx)))
                .child(if busy {
                    "Создаём…"
                } else if group {
                    "Создать беседу"
                } else {
                    "Начать диалог"
                }),
        )
        .child(
            div()
                .id("creator-cancel")
                .debug_selector(|| "creator-cancel".into())
                .text_size(px(11.))
                .text_color(ink(p.muted))
                .cursor_pointer()
                .on_click(cx.listener(|s, _, _, cx| s.close_conversation_creator(cx)))
                .child("Отмена"),
        )
        .into_any_element()
    }
}

struct PersonalRow {
    id: i64,
    title: String,
    direct: bool,
    members: usize,
    unread: i64,
    muted: bool,
    peer: i64,
    avatar: Option<std::sync::Arc<gpui::RenderImage>>,
}
