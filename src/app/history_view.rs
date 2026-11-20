use super::ui::{history_divider, icon, ink, ink_alpha};
use super::*;
use crate::history;
use crate::message_tools::HoverAction;

/// Byte offset of a rune index, for text runs and highlights.
fn char_byte_index(content: &str, index: usize) -> usize {
    content
        .char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(content.len())
}

impl Voxhold {
    pub(super) fn render_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        chat: layout::Rect,
        mobile: bool,
        docked: bool,
    ) -> AnyElement {
        let p = self.palette; // Chat column: fixed header, history, composer, panels.
        let channel_kind = if self.conversation.is_some() {
            "text"
        } else {
            self.channels
                .iter()
                .find(|channel| channel.id == self.channel)
                .map(|channel| channel.kind.as_str())
                .unwrap_or("")
        };
        let meta = history::row_meta(&self.messages, channel_kind, self.unread_after);
        let keys: Vec<HistoryKey> = self
            .messages
            .iter()
            .zip(&meta)
            .map(|(message, meta)| {
                (
                    message.id,
                    message.revision,
                    message.reactions_revision,
                    meta.date.clone().unwrap_or_default(),
                    meta.grouped,
                    meta.unread,
                    self.attachment_epoch,
                )
            })
            .collect();
        if keys != self.history_keys {
            let prefix = self
                .history_keys
                .iter()
                .zip(&keys)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = self.history_keys[prefix..]
                .iter()
                .rev()
                .zip(keys[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            self.history_list.splice(
                prefix..self.history_keys.len() - suffix,
                keys.len() - prefix - suffix,
            );
            self.history_keys = keys;
        }
        // A prepend keeps `(message_id, offset_in_row)` under the pointer. The
        // list count was just spliced, so the target index is in range.
        if let Some((id, offset_in_item)) = self.pending_anchor.take() {
            if let Some(item_ix) = self.messages.iter().position(|m| m.id == id) {
                self.history_list.scroll_to(gpui::ListOffset {
                    item_ix,
                    offset_in_item,
                });
            }
        }
        let selected = self.channels.iter().find(|c| c.id == self.channel).cloned();
        let conversation = self.conversation.is_some();
        let readable = conversation || self.channel_chat_readable(self.channel);
        let role = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .map(|s| s.role.clone())
            .unwrap_or_default();
        // Avatars are requested only for the neighbourhood of the viewport. The
        // member list carries the version; the cache is shared with the rail.
        let start = self
            .history_list
            .logical_scroll_top()
            .item_ix
            .saturating_sub(3);
        let end = (start + 40).min(self.messages.len());
        let mut wanted: Vec<(i64, String)> = self.messages[start..end]
            .iter()
            .filter_map(|message| {
                self.members
                    .iter()
                    .find(|member| member.user_id == message.author.user_id)
                    .and_then(|member| {
                        member
                            .avatar_version
                            .as_ref()
                            .filter(|version| !version.is_empty())
                            .map(|version| (member.user_id, version.clone()))
                    })
            })
            .collect();
        wanted.sort();
        wanted.dedup();
        for (user_id, version) in wanted {
            self.request_avatar(user_id, version, window, cx);
        }
        // Thumbnails and link previews are requested for the same viewport
        // window as the avatars, then read from the bounded caches.
        let mut thumbs: std::collections::HashMap<
            media::ThumbKey,
            std::sync::Arc<gpui::RenderImage>,
        > = std::collections::HashMap::new();
        let mut previews: std::collections::HashMap<
            String,
            (
                media::PreviewEntry,
                Option<std::sync::Arc<gpui::RenderImage>>,
            ),
        > = std::collections::HashMap::new();
        let mut wanted_thumbs = Vec::new();
        let mut wanted_previews = Vec::new();
        for message in &self.messages[start..end] {
            for file in &message.attachments {
                if !history::image_attachment(file) {
                    continue;
                }
                let (width, height) = history::attachment_size(
                    file.width,
                    file.height,
                    (chat.w - history::HISTORY_TEXT_INSET).max(1.),
                );
                wanted_thumbs.push(media::ThumbKey::new(&file.id, width, height - 25.));
            }
            let (_, hrefs) = links::scan(&message.content);
            wanted_previews.extend(hrefs);
        }
        for key in wanted_thumbs {
            self.request_thumbnail(key.clone(), window, cx);
            if let Some(image) = self.media.thumbs.get(&key) {
                thumbs.insert(key, image);
            }
        }
        let visible_previews: std::collections::HashSet<String> = wanted_previews
            .iter()
            .map(|preview| preview.cache_url.clone())
            .collect();
        for preview in wanted_previews {
            self.request_preview(preview.cache_url.clone(), window, cx);
            if let Some(entry) = self.media.preview_entry(&preview.cache_url) {
                let image = self.media.preview_images.get(&preview.cache_url);
                previews.insert(preview.cache_url, (entry, image));
            }
        }
        self.ensure_preview_timer(&visible_previews, window, cx);
        let mut avatar_by_user = std::collections::HashMap::new();
        for ((user_id, version), (image, used)) in self.roster.avatars.iter_mut() {
            avatar_by_user.insert((*user_id, version.clone()), image.clone());
            *used = std::time::Instant::now();
        }
        let member_info: std::collections::HashMap<i64, (String, Option<String>)> = self
            .members
            .iter()
            .map(|member| {
                (
                    member.user_id,
                    (member.role.clone(), member.avatar_version.clone()),
                )
            })
            .collect();
        let rows: Vec<_> =
            self.messages
                .iter()
                .zip(meta)
                .map(|(message, meta)| {
                    let own = self.self_user_id > 0 && self.self_user_id == message.author.user_id;
                    let permissions = message_tools::permissions_for(
                        self.self_user_id,
                        &role,
                        selected.as_ref(),
                        message,
                        conversation,
                    );
                    let read_by_other = self
                        .reads
                        .iter()
                        .any(|(user, last)| *user != self.self_user_id && *last >= message.id);
                    let (role_label, avatar) = member_info
                        .get(&message.author.user_id)
                        .cloned()
                        .map(|(role, version)| {
                            let avatar = version.as_ref().and_then(|version| {
                                avatar_by_user
                                    .get(&(message.author.user_id, version.clone()))
                                    .cloned()
                            });
                            (role, avatar)
                        })
                        .unwrap_or_default();
                    let selected_row = self.selected_messages.contains(&message.id);
                    let pinned = self.pinned_ids.contains(&message.id);
                    let highlighted = self
                        .highlighted_message
                        .is_some_and(|(id, _)| id == message.id);
                    let editing = self.editing == Some(message.id);
                    let delete_confirm = self.confirm_delete == Some(message.id)
                        || self.message_menu.as_ref().is_some_and(|menu| {
                            menu.message_id == message.id && menu.confirm_delete
                        });
                    let error = {
                        let edit_error = self
                            .edit_error
                            .as_ref()
                            .filter(|(row, open, _)| {
                                *row == message.id && *open == self.edit_open_seq && editing
                            })
                            .map(|(_, _, error)| error.clone())
                            .unwrap_or_default();
                        if edit_error.is_empty() {
                            self.action_error
                                .as_ref()
                                .filter(|(id, _)| *id == message.id)
                                .map(|(_, error)| error.clone())
                                .unwrap_or_default()
                        } else {
                            edit_error
                        }
                    };
                    (
                        message.clone(),
                        permissions,
                        meta,
                        read_by_other,
                        own,
                        role_label,
                        avatar,
                        selected_row,
                        pinned,
                        highlighted,
                        editing,
                        delete_confirm,
                        error,
                    )
                })
                .collect();
        let weak = cx.entity().downgrade();
        self.painted_read = Default::default();
        let painted_read = self.painted_read.clone();
        let hovered_message = self.hovered_message;
        let scroll_epoch = self.scroll_epoch;
        let menu_closed = self.message_menu.is_none();
        let edit_input = self.edit_input.clone();
        let thumbs_for_rows = thumbs;
        let previews_for_rows = previews;
        let history_list = list(self.history_list.clone(), move |index, _, _cx| {
            let (
                message,
                permissions,
                meta,
                read_by_other,
                own,
                role,
                avatar,
                selected_row,
                pinned,
                highlighted,
                editing,
                delete_confirm,
                action_error,
            ) = rows[index].clone();
            let id = message.id;
            let painted_read = painted_read.clone();
            let weak = weak.clone();
            let weak_hover = weak.clone();
            let weak_menu = weak.clone();
            let weak_select = weak.clone();
            let weak_quote = weak.clone();
            let weak_reaction = weak.clone();
            let weak_media = weak.clone();
            let role_color = history::role_color(&role, p.light);
            let time = history::format_time(message.created_at);
            let quote = history::quote_labels(&message);
            let quote_target = message
                .reply
                .as_ref()
                .filter(|reply| !reply.deleted && reply.message_id > 0)
                .map(|reply| reply.message_id);
            let face = if let Some(image) = avatar {
                div()
                    .w(px(38.))
                    .h(px(38.))
                    .flex_shrink_0()
                    .rounded_full()
                    .overflow_hidden()
                    .child(
                        gpui::img(image)
                            .size_full()
                            .object_fit(gpui::ObjectFit::Cover),
                    )
            } else {
                div()
                    .w(px(38.))
                    .h(px(38.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(ink(members::avatar_color(&message.author.username)))
                    .text_size(px(14.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(0x151510))
                    .child(members::initials(&message.author.username))
            };
            let mut row = div()
                .id(("message", message.id as u64))
                .relative()
                .w_full()
                .cursor_pointer()
                .when(selected_row, |row| {
                    row.bg(ink(if p.light { 0xe4e8cf } else { 0x2b2f1f }))
                })
                .when(highlighted && !selected_row, |row| {
                    row.bg(ink(if p.light { 0xf1f3d8 } else { 0x30341f }))
                })
                .on_click(move |event, _, cx| {
                    if let Some(view) = weak_select.upgrade() {
                        let additive = event.modifiers().control || event.modifiers().shift;
                        view.update(cx, |s, cx| s.toggle_selected(id, additive, cx));
                    }
                })
                .on_mouse_down(
                    MouseButton::Right,
                    move |event: &gpui::MouseDownEvent, _, cx| {
                        if let Some(view) = weak_menu.upgrade() {
                            let x: f32 = event.position.x.into();
                            let y: f32 = event.position.y.into();
                            view.update(cx, |s, cx| s.open_message_menu(id, x, y, false, cx));
                        }
                        cx.stop_propagation();
                    },
                )
                .on_hover(move |hovered, _, cx| {
                    if let Some(view) = weak_hover.upgrade() {
                        view.update(cx, |s, cx| {
                            let next = if *hovered {
                                id
                            } else if s.hovered_message == id {
                                0
                            } else {
                                s.hovered_message
                            };
                            if s.hovered_message != next {
                                s.hovered_message = next;
                                if next == 0 {
                                    s.hovered_action = None;
                                }
                                cx.notify();
                            }
                        });
                    }
                });
            if let Some(date) = &meta.date {
                row = row.child(history_divider(date, p.muted, p.line, p.bg, false));
            }
            if meta.unread {
                row = row.child(history_divider(
                    "Новые сообщения",
                    p.lime,
                    p.line,
                    p.bg,
                    true,
                ));
            }
            if hovered_message == id && menu_closed {
                let buttons = message_tools::hover_actions(permissions, conversation, false);
                let mut bar = div()
                    .id(gpui::ElementId::Name(format!("hover-actions-{id}").into()))
                    .absolute()
                    .right(px(12.))
                    .top(px(-10.))
                    .h(px(29.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(ink(0x3b3b36))
                    .bg(ink(0x252522))
                    .occlude();
                for (index, action) in buttons.into_iter().enumerate() {
                    let key = action.key();
                    let hovered_key = (id, key.to_string());
                    let active = match action {
                        HoverAction::Quick(emoji) => message
                            .reactions
                            .iter()
                            .any(|reaction| reaction.emoji == emoji && reaction.me),
                        _ => false,
                    };
                    let weak_button = weak.clone();
                    let mut button = div()
                        .id(gpui::ElementId::Name(format!("hover-{id}-{index}").into()))
                        .w(px(29.))
                        .h(px(29.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .when(active, |button| button.bg(ink(0x393a2e)))
                        .on_hover(move |hovered, _, cx| {
                            if let Some(view) = weak_button.upgrade() {
                                view.update(cx, |s, cx| {
                                    let next = if *hovered {
                                        Some(hovered_key.clone())
                                    } else if s.hovered_action.as_ref() == Some(&hovered_key) {
                                        None
                                    } else {
                                        s.hovered_action.clone()
                                    };
                                    if s.hovered_action != next {
                                        s.hovered_action = next;
                                        cx.notify();
                                    }
                                });
                            }
                        });
                    button = match action {
                        HoverAction::Quick(emoji) => button.child(
                            div()
                                .font_family(crate::reactions::EMOJI_FONT)
                                .text_size(px(16.))
                                .child(emoji),
                        ),
                        HoverAction::Picker => {
                            button.child(icon("icons/emoji.svg", 15., ink(p.muted)))
                        }
                        HoverAction::Reply => {
                            button.child(icon("icons/reply.svg", 15., ink(p.muted)))
                        }
                        HoverAction::Forward => {
                            button.child(icon("icons/forward.svg", 15., ink(p.muted)))
                        }
                        HoverAction::More => {
                            button.child(icon("icons/more.svg", 15., ink(p.muted)))
                        }
                    };
                    let weak_click = weak.clone();
                    button = button.on_click(move |event, window, cx| {
                        let Some(view) = weak_click.upgrade() else {
                            return;
                        };
                        let x: f32 = event.position().x.into();
                        let y: f32 = event.position().y.into();
                        view.update(cx, |s, cx| {
                            s.dispatch_hover_action(id, action, scroll_epoch, x, y, window, cx)
                        });
                        cx.stop_propagation();
                    });
                    bar = bar.child(button);
                }
                row = row.child(bar);
            }
            let mut head = div()
                .flex()
                .items_center()
                .gap(px(7.))
                .h(px(20.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(role_color))
                        .child(message.author.username.clone()),
                )
                .child(
                    div()
                        .px(px(6.))
                        .h(px(18.))
                        .rounded(px(5.))
                        .bg(ink_alpha(role_color, 48))
                        .border_1()
                        .border_color(ink_alpha(role_color, 87))
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(role_color))
                        .child(history::role_label(&role, true)),
                );
            if own {
                head = head.child(
                    div()
                        .px(px(5.))
                        .h(px(16.))
                        .rounded(px(4.))
                        .bg(ink(p.lime))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(0x17170f))
                        .child("Вы"),
                );
            }
            head = head.child(
                div()
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .child(time.clone()),
            );
            let mut body = div().flex_1().min_w(px(0.)).flex().flex_col();
            if !meta.grouped {
                body = body.child(head);
            }
            if let Some(target) = quote_target {
                let name = quote.0.clone();
                let excerpt = quote.1.clone();
                body = body.child(
                    div()
                        .id(("quote", id as u64))
                        .w_full()
                        .max_w(px(580.))
                        .h(px(history::QUOTE_HEIGHT - 7.))
                        .mb(px(4.))
                        .rounded(px(9.))
                        .bg(ink(if p.light { 0xf0f2d9 } else { 0x26291b }))
                        .border_l_2()
                        .border_color(ink_alpha(if p.light { 0xb5cf16 } else { 0xe7ff5b }, 163))
                        .pl(px(12.))
                        .pr(px(12.))
                        .pt(px(6.))
                        .cursor_pointer()
                        .on_click(move |_, window, cx| {
                            if let Some(view) = weak_quote.upgrade() {
                                view.update(cx, |s, cx| s.jump_to_message(target, window, cx));
                            }
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(icon(
                                    "icons/reply.svg",
                                    15.,
                                    ink(if p.light { 0xb5cf16 } else { 0xe7ff5b }),
                                ))
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(ink(if p.light { 0xb5cf16 } else { 0xe7ff5b }))
                                        .truncate()
                                        .child(name),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(ink(p.muted))
                                .truncate()
                                .child(excerpt),
                        ),
                );
            } else if message.forward.is_some() {
                body = body.child(
                    div()
                        .h(px(history::QUOTE_HEIGHT - 7.))
                        .w_full()
                        .max_w(px(580.))
                        .mb(px(4.))
                        .rounded(px(9.))
                        .bg(ink(if p.light { 0xf0f2d9 } else { 0x26291b }))
                        .pl(px(12.))
                        .pt(px(6.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(icon("icons/forward.svg", 15., ink(p.lime)))
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(ink(p.lime))
                                        .child(quote.0.clone()),
                                ),
                        ),
                );
            }
            let (link_list, hrefs) = links::scan(&message.content);
            let mut ranges = Vec::new();
            let mut link_targets = Vec::new();
            for link in &link_list {
                let start = char_byte_index(&message.content, link.start);
                let end = char_byte_index(&message.content, link.end);
                if start < end {
                    ranges.push(start..end);
                    link_targets.push(link.href.clone());
                }
            }
            let highlights: Vec<_> = ranges
                .iter()
                .map(|range| {
                    (
                        range.clone(),
                        gpui::HighlightStyle {
                            color: Some(ink(p.lime).into()),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            let text = div()
                .w_full()
                .text_size(px(14.))
                .line_height(px(20.))
                .text_color(ink(if p.light { 0x3b3b36 } else { 0xbab8b0 }));
            body = body.child(if ranges.is_empty() {
                text.child(message.content.clone())
            } else {
                let weak_text = weak_media.clone();
                text.child(
                    gpui::InteractiveText::new(
                        ("message-text", id as u64),
                        gpui::StyledText::new(message.content.clone()).with_highlights(highlights),
                    )
                    .on_click(ranges, move |index, _window, cx| {
                        let Some(href) = link_targets.get(index).cloned() else {
                            return;
                        };
                        if let Some(view) = weak_text.upgrade() {
                            view.update(cx, |s, cx| s.open_link(&href, cx));
                        }
                        cx.stop_propagation();
                    }),
                )
            });
            // A card is reserved as soon as the message contains a link, so a
            // late metadata answer never grows the row.
            for preview in hrefs.iter().take(2) {
                let cache_url = &preview.cache_url;
                let (meta, image) = match previews_for_rows.get(cache_url) {
                    Some((entry, image)) => (entry.meta.clone(), image.clone()),
                    None => (voxhold_api::LinkPreview::default(), None),
                };
                let labels = media::Media::preview_labels(cache_url, &meta);
                let mut labels_column = div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .py(px(6.));
                for (line, value) in labels.into_iter().enumerate() {
                    if value.is_empty() {
                        continue;
                    }
                    labels_column = labels_column.child(
                        div()
                            .h(px(19.))
                            .truncate()
                            .text_size(px(if line == 1 { 14. } else { 11. }))
                            .font_weight(if line == 1 {
                                gpui::FontWeight::BOLD
                            } else {
                                gpui::FontWeight::NORMAL
                            })
                            .text_color(ink(if line == 1 { p.fg } else { p.muted }))
                            .child(value),
                    );
                }
                let weak_link = weak_media.clone();
                let target = preview.open_href.clone();
                let card = div()
                    .id(gpui::ElementId::Name(
                        format!("link-card-{}-{}", id, cache_url).into(),
                    ))
                    .w_full()
                    .max_w(px(580.))
                    .h(px(96.))
                    .mb(px(4.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(ink(if p.light { 0xdcdcd2 } else { 0x34342e }))
                    .bg(ink(if p.light { 0xf4f4ec } else { 0x1b1b18 }))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(13.))
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        if let Some(view) = weak_link.upgrade() {
                            view.update(cx, |s, cx| s.open_link(&target, cx));
                        }
                        cx.stop_propagation();
                    })
                    .child(div().flex_1().min_w(px(0.)).child(labels_column));
                body = body.child(if let Some(image) = image {
                    card.child(
                        div()
                            .w(px(98.))
                            .h(px(76.))
                            .flex_shrink_0()
                            .overflow_hidden()
                            .child(
                                gpui::img(image)
                                    .w(px(98.))
                                    .h(px(76.))
                                    .object_fit(gpui::ObjectFit::Cover),
                            ),
                    )
                } else {
                    card
                });
            }
            for (file_index, file) in message.attachments.iter().enumerate() {
                let (width, height) = history::attachment_size(
                    file.width,
                    file.height,
                    (chat.w - history::HISTORY_TEXT_INSET).max(1.),
                );
                let label = history::attachment_label(file);
                if history::image_attachment(file) {
                    let key = media::ThumbKey::new(&file.id, width, height - 25.);
                    let thumbnail = thumbs_for_rows.get(&key).cloned();
                    let file_for_click = file.clone();
                    let weak_image = weak_media.clone();
                    let message_id = message.id;
                    let slot = div()
                        .id(("attachment-image", file_index))
                        .w(px(width))
                        .h(px(height))
                        .flex_shrink_0()
                        .rounded(px(7.))
                        .overflow_hidden()
                        .bg(ink(if p.light { 0xe8e8e0 } else { 0x20201d }))
                        .cursor_pointer()
                        .when_some(thumbnail, |slot, image| {
                            slot.child(
                                gpui::img(image)
                                    .size_full()
                                    .object_fit(gpui::ObjectFit::Cover),
                            )
                        })
                        .on_click(move |_, window, cx| {
                            if let Some(view) = weak_image.upgrade() {
                                view.update(cx, |s, cx| {
                                    s.open_viewer(message_id, &file_for_click, window, cx)
                                });
                            }
                            cx.stop_propagation();
                        });
                    body = body.child(
                        div()
                            .w(px(width))
                            .mb(px(4.))
                            .flex()
                            .flex_col()
                            .child(slot)
                            .child(
                                div()
                                    .h(px(25.))
                                    .text_size(px(11.))
                                    .text_color(ink(p.muted))
                                    .truncate()
                                    .child(label),
                            ),
                    );
                } else {
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(29.))
                            .child(icon("icons/paperclip.svg", 14., ink(p.muted)))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(ink(p.muted))
                                    .truncate()
                                    .child(label),
                            ),
                    );
                }
            }
            if !message.reactions.is_empty() {
                let mut reactions = div().flex().flex_wrap().gap(px(4.)).mt(px(4.)).w_full();
                for (reaction_index, reaction) in message.reactions.clone().into_iter().enumerate()
                {
                    let emoji = reaction.emoji.clone();
                    let active = reaction.me;
                    let count = history::reaction_count(&reaction);
                    let weak_reaction = weak_reaction.clone();
                    reactions = reactions.child(
                        div()
                            .id((
                                gpui::ElementId::from(("reaction", id as u64)),
                                reaction_index.to_string(),
                            ))
                            .h(px(22.))
                            .min_w(px(history::REACTION_PITCH - 4.))
                            .px_1()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .rounded(px(6.))
                            .bg(ink(if active {
                                if p.light {
                                    0xe9edbd
                                } else {
                                    0x353c23
                                }
                            } else if p.light {
                                0xecece3
                            } else {
                                0x292925
                            }))
                            .text_size(px(15.))
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                if let Some(view) = weak_reaction.upgrade() {
                                    view.update(cx, |s, cx| {
                                        s.toggle_reaction(id, emoji.clone(), active, cx);
                                    });
                                }
                                cx.stop_propagation();
                            })
                            .child(
                                div()
                                    .font_family(crate::reactions::EMOJI_FONT)
                                    .text_size(px(15.))
                                    .child(reaction.emoji.clone()),
                            )
                            .child(div().text_size(px(12.)).text_color(ink(p.fg)).child(count)),
                    );
                }
                body = body.child(reactions);
            }
            row = row.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .px(px(19.))
                    .pr(px(21.))
                    .pt(px(4.))
                    .pb(px(10.))
                    .when(!meta.grouped, |line| line.child(face))
                    .when(meta.grouped, |line| {
                        line.child(
                            div()
                                .w(px(38.))
                                .h(px(20.))
                                .flex_shrink_0()
                                .text_size(px(10.))
                                .text_color(ink(p.muted))
                                .child(if hovered_message == id {
                                    time.clone()
                                } else {
                                    String::new()
                                }),
                        )
                    })
                    .child(body),
            );
            if pinned {
                row = row.child(
                    div()
                        .px(px(19.))
                        .text_size(px(10.))
                        .text_color(ink(p.lime))
                        .child("Закреплено"),
                );
            }
            if editing {
                let weak_save = weak.clone();
                let weak_cancel = weak.clone();
                row = row.child(
                    div()
                        .px(px(19.))
                        .pb(px(6.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(TextInput::new(&edit_input))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .id(("edit-save", id as u64))
                                        .cursor_pointer()
                                        .text_size(px(12.))
                                        .text_color(ink(p.lime))
                                        .on_click(move |_, window, cx| {
                                            if let Some(view) = weak_save.upgrade() {
                                                view.update(cx, |s, cx| s.save_edit(window, cx));
                                            }
                                            cx.stop_propagation();
                                        })
                                        .child("Сохранить"),
                                )
                                .child(
                                    div()
                                        .id(("edit-cancel", id as u64))
                                        .cursor_pointer()
                                        .text_size(px(12.))
                                        .text_color(ink(p.muted))
                                        .on_click(move |_, _, cx| {
                                            if let Some(view) = weak_cancel.upgrade() {
                                                view.update(cx, |s, cx| s.cancel_edit(cx));
                                            }
                                            cx.stop_propagation();
                                        })
                                        .child("Отмена"),
                                ),
                        ),
                );
            }
            if delete_confirm {
                let weak_yes = weak.clone();
                let weak_no = weak.clone();
                row = row.child(
                    div()
                        .px(px(19.))
                        .pb(px(6.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(12.))
                        .child("Удалить сообщение?")
                        .child(
                            div()
                                .id(("delete-no", id as u64))
                                .cursor_pointer()
                                .text_color(ink(p.muted))
                                .on_click(move |_, _, cx| {
                                    if let Some(view) = weak_no.upgrade() {
                                        view.update(cx, |s, cx| {
                                            s.cancel_delete(cx);
                                            s.message_menu = None;
                                            cx.notify();
                                        });
                                    }
                                    cx.stop_propagation();
                                })
                                .child("Нет"),
                        )
                        .child(
                            div()
                                .id(("delete-yes", id as u64))
                                .cursor_pointer()
                                .text_color(ink(p.danger))
                                .on_click(move |_, window, cx| {
                                    if let Some(view) = weak_yes.upgrade() {
                                        view.update(cx, |s, cx| {
                                            s.confirm_delete_now(id, window, cx)
                                        });
                                    }
                                    cx.stop_propagation();
                                })
                                .child("Да"),
                        ),
                );
            }
            if !action_error.is_empty() {
                row = row.child(
                    div()
                        .px(px(19.))
                        .pb(px(4.))
                        .text_size(px(11.))
                        .text_color(ink(p.danger))
                        .child(action_error),
                );
            }
            if own {
                let mut checks = div()
                    .absolute()
                    .right(px(14.))
                    .bottom(px(2.))
                    .flex()
                    .items_center();
                checks = checks.child(icon("icons/check.svg", 12., ink(0x63d8e6)));
                if read_by_other {
                    checks = checks.child(div().ml(px(-7.)).child(icon(
                        "icons/check.svg",
                        12.,
                        ink(0x63d8e6),
                    )));
                }
                row = row.child(checks);
            }
            row.child(
                gpui::canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        if bounds.intersects(&window.content_mask().bounds) {
                            painted_read.set(painted_read.get().max(id));
                        }
                    },
                )
                .absolute()
                .size_full()
                .top_0()
                .left_0(),
            )
            .into_any_element()
        })
        .flex_1();
        if let Some(id) = self.pending_jump.take() {
            if let Some(item_ix) = self.messages.iter().position(|message| message.id == id) {
                self.history_list.scroll_to(gpui::ListOffset {
                    item_ix,
                    offset_in_item: px(0.),
                });
            }
        }
        let mut history = div()
            .relative()
            .flex_1()
            .flex()
            .flex_col()
            .min_h(px(0.))
            .debug_selector(|| "chat-history".into());
        if self.has_more {
            let retry_in = self
                .older_retry_at
                .map(|at| {
                    let remaining = at.saturating_duration_since(std::time::Instant::now());
                    remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)
                })
                .unwrap_or(0);
            let label = if self.loading_older {
                "Загрузка…".to_string()
            } else if retry_in > 0 {
                format!("Повторить через {retry_in} с")
            } else if !self.older_error.is_empty() {
                "Повторить".to_string()
            } else {
                "Загрузить предыдущие сообщения".to_string()
            };
            history = history.child(
                div()
                    .id("older")
                    .p_2()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .on_click(cx.listener(|s, _, window, cx| s.load_older(window, cx)))
                    .child(label)
                    .when(!self.older_error.is_empty(), |row| {
                        row.child(
                            div()
                                .text_size(px(11.))
                                .text_color(ink(p.danger))
                                .child(self.older_error.clone()),
                        )
                    }),
            );
        }
        history = history.child(history_list);
        if self.history_has_newer {
            history = history.child(
                div()
                    .id("history-latest")
                    .absolute()
                    .right(px(24.))
                    .bottom(px(8.))
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .bg(ink(p.selection))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(ink(p.fg))
                    .child("К последним")
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.reset_for_latest();
                        s.refresh_history(window, cx);
                    })),
            );
        }
        let scroll_max: f32 = self.history_list.max_offset_for_scrollbar().height.into();
        let scroll_offset: f32 = (-self.history_list.scroll_px_offset_for_scrollbar().y).into();
        let viewport_h: f32 = self.history_list.viewport_bounds().size.height.into();
        if scroll_max > 0.5 && viewport_h > 40. {
            let track_h = viewport_h - 10.;
            let content = viewport_h + scroll_max;
            let thumb_h = (viewport_h * viewport_h / content).max(28.).min(track_h);
            let progress = (scroll_offset / scroll_max).clamp(0., 1.);
            let thumb_top = (track_h - thumb_h) * progress;
            let track_top: f32 = self.history_list.viewport_bounds().origin.y.into();
            let header_height = if docked {
                48.
            } else if mobile {
                58.
            } else {
                67.
            };
            let history_top = chat.y + header_height;
            history = history.child(
                div()
                    .id("history-scrollbar")
                    .absolute()
                    .right(px(2.))
                    .top(px(track_top + 5. - history_top))
                    .w(px(15.))
                    .h(px(track_h))
                    .cursor(gpui::CursorStyle::default())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |s, event: &gpui::MouseDownEvent, _, cx| {
                            let y: f32 = event.position.y.into();
                            let top = track_top + 5.;
                            let thumb_y = top + thumb_top;
                            let grab = (y - thumb_y).clamp(0., thumb_h);
                            s.scrollbar_drag = Some(ScrollbarDrag {
                                top,
                                height: track_h,
                                thumb: thumb_h,
                                grab,
                            });
                            s.history_list.scrollbar_drag_started();
                            let ratio =
                                ((y - grab - top) / (track_h - thumb_h).max(1.)).clamp(0., 1.);
                            s.history_list.set_offset_from_scrollbar(gpui::point(
                                px(0.),
                                px(ratio * scroll_max),
                            ));
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(4.))
                            .top(px(thumb_top))
                            .w(px(7.))
                            .h(px(thumb_h))
                            .rounded(px(4.))
                            .bg(ink(if self.scrollbar_drag.is_some() {
                                0x96968e
                            } else {
                                0x50504a
                            })),
                    ),
            );
        }
        if readable && !self.messages.is_empty() && !self.read_pending && !self.read_scheduled {
            self.schedule_visible_read(window, cx);
        }
        history.into_any_element()
    }
}
