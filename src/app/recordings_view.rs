//! «Записи сервера» modal. One container for the header and the call panel,
//! fixed chrome around a clipped card list. Geometry comes from
//! [`crate::recordings`], so the list height, scrollbar, Tab ring and hit tests
//! all use one source.

use super::*;
use crate::{recordings, recordings_dialog, recordings_dialog::Rect};
use gpui::{Div, ElementId, Stateful};
use std::sync::Arc;
use voxhold_api::{Recording, RECORDING_READY};

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecButton {
    Secondary,
    Danger,
    Link,
    Icon,
}

impl Voxhold {
    pub(super) fn recordings_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.recordings_dialog.as_ref()?;
        if self.conversation.is_some()
            || self
                .recordings_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.server_id != self.server)
        {
            self.close_recordings(false, window, cx);
            return None;
        }
        self.sync_recording_dialog_scope(window, cx);
        let (vw, vh) = self.viewport;
        let (vw, vh) = (vw.max(1.), vh.max(1.));
        let now = super::recordings_ops::now_secs();
        {
            let dialog = self.recordings_dialog.as_mut()?;
            dialog.layout(&self.recordings, vw, vh, now, self.self_user_id);
        }
        self.ensure_recording_dialog_focus(window, cx);
        self.request_visible_recording_previews(window, cx);

        let p = self.palette;
        let dialog = self.recordings_dialog.as_ref()?;
        let bounds = dialog.bounds;
        let list = dialog.list;
        let padding = dialog.padding;
        let inner = dialog.inner;
        let footer_top = dialog.footer_top;
        let hint_lines = dialog.hint_lines;
        let max_scroll = dialog.max_scroll;
        let scroll = dialog.scroll;
        let busy = dialog.busy.clone();
        let confirm = dialog.confirm.clone();
        let error = if dialog.error.is_empty() {
            self.recordings.error.clone()
        } else {
            dialog.error.clone()
        };
        let rows = dialog.rows.clone();
        let loading = self.recordings.loading;
        let hint = self.recordings.hint();
        let previews_visible = self.recordings_visible_for_preview();
        let mut previews = std::collections::HashMap::new();
        if let Some(dialog) = self.recordings_dialog.as_mut() {
            for row in &rows {
                if !previews_visible || !row.ready {
                    continue;
                }
                let Some(version) = row
                    .record
                    .preview_version
                    .as_ref()
                    .filter(|version| !version.is_empty())
                else {
                    continue;
                };
                let key = format!("{}@{version}", row.record.id);
                if let Some(image) = dialog.previews.get(&key) {
                    previews.insert(row.record.id.clone(), image);
                }
            }
        }

        let mut ring: Vec<(FocusHandle, String)> = Vec::new();
        let mut card = div()
            .id("recordings-card")
            .absolute()
            .left(px(bounds.x))
            .top(px(bounds.y))
            .w(px(bounds.w))
            .h(px(bounds.h))
            .occlude()
            .rounded(px(17.))
            .border_1()
            .border_color(ink(p.line))
            .bg(ink(if p.light { 0xffffff } else { 0x1e1e1b }))
            .text_color(ink(p.fg))
            .overflow_hidden();

        card = card
            .child(rec_text(
                "ЗАПИСИ ЗВОНКОВ",
                padding,
                18.,
                inner - 40.,
                14.,
                10.,
                p.lime,
                true,
            ))
            .child(rec_text(
                "Записи сервера",
                padding,
                33.,
                inner - 40.,
                30.,
                23.,
                p.fg,
                true,
            ));
        let close = self.rec_button(
            "rec-close",
            "",
            Rect::new(bounds.w - padding - 32., 17., 32., 32.),
            RecButton::Icon,
            false,
            &mut ring,
            window,
            cx,
        );
        card = card.child(close);

        let hint_width = inner;
        let lines = recordings_dialog::wrap_lines(&hint, hint_width, 12.);
        for (index, line) in lines.iter().enumerate() {
            let y = list.y - bounds.y - 18. - hint_lines as f32 * 18. + index as f32 * 18.;
            card = card.child(rec_text(line, padding, y, inner, 18., 12., p.muted, false));
        }

        let mut list_children: Vec<AnyElement> = Vec::new();
        if rows.is_empty() {
            if loading {
                list_children.push(rec_text(
                    "Загружаем записи…",
                    0.,
                    40.,
                    list.w,
                    24.,
                    13.,
                    p.muted,
                    false,
                ));
            } else {
                list_children.push(
                    div()
                        .absolute()
                        .left(px((list.w - 28.) / 2.))
                        .top(px(22.))
                        .child(icon("icons/inbox.svg", 28., ink(p.muted)))
                        .into_any_element(),
                );
                list_children.push(rec_text(
                    "Записей пока нет",
                    0.,
                    58.,
                    list.w,
                    24.,
                    16.,
                    p.fg,
                    true,
                ));
                list_children.push(rec_text(
                    "Они появятся здесь после записи звонка.",
                    10.,
                    84.,
                    (list.w - 20.).max(40.),
                    20.,
                    12.,
                    p.muted,
                    false,
                ));
            }
        }
        for row in &rows {
            let image = previews.get(&row.record.id).cloned();
            list_children.push(
                self.recording_card(row, image, list, &busy, &confirm, &mut ring, window, cx),
            );
        }
        let wheel = cx.listener(|s, event: &gpui::ScrollWheelEvent, _window, cx| {
            let delta: f32 = event.delta.pixel_delta(px(42.)).y.into();
            if let Some(dialog) = s.recordings_dialog.as_mut() {
                dialog.wheel(-delta);
            }
            cx.notify();
            cx.stop_propagation();
        });
        card = card.child(
            div()
                .id("recordings-list")
                .absolute()
                .left(px(list.x - bounds.x))
                .top(px(list.y - bounds.y))
                .w(px(list.w))
                .h(px(list.h))
                .overflow_hidden()
                .on_scroll_wheel(wheel)
                .children(list_children),
        );
        if max_scroll > 0. {
            let track = list.h - 16.;
            let thumb = 25_f32.max(track * track / (track + max_scroll));
            let top = 8. + (track - thumb) * (scroll / max_scroll).clamp(0., 1.);
            card = card.child(
                div()
                    .absolute()
                    .left(px(list.x - bounds.x + list.w - 7.))
                    .top(px(list.y - bounds.y + top))
                    .w(px(4.))
                    .h(px(thumb))
                    .rounded(px(2.))
                    .bg(ink(p.line)),
            );
        }

        card = card.child(
            div()
                .absolute()
                .left(px(padding))
                .top(px(footer_top - bounds.y))
                .w(px(inner))
                .h(px(1.))
                .bg(ink(p.line)),
        );
        if !error.is_empty() {
            card = card.child(rec_text(
                &error,
                padding,
                footer_top - bounds.y + 24.,
                (inner - 200.).max(40.),
                22.,
                12.,
                p.danger,
                false,
            ));
        }
        let dismiss = self.rec_button(
            "rec-dismiss",
            "Закрыть",
            Rect::new(
                bounds.w - padding - recordings::DISMISS_WIDTH,
                footer_top - bounds.y + 15.,
                recordings::DISMISS_WIDTH,
                recordings::DISMISS_HEIGHT,
            ),
            RecButton::Secondary,
            false,
            &mut ring,
            window,
            cx,
        );
        card = card.child(dismiss);
        if !self.recordings.error.is_empty() && !self.recordings.loading {
            let retry = self.rec_button(
                "rec-retry",
                "Повторить",
                Rect::new(
                    bounds.w - padding - recordings::DISMISS_WIDTH - 96.,
                    footer_top - bounds.y + 18.,
                    recordings::RETRY_WIDTH,
                    recordings::RETRY_HEIGHT,
                ),
                RecButton::Secondary,
                false,
                &mut ring,
                window,
                cx,
            );
            card = card.child(retry);
        }

        let focus_request = self
            .recordings_dialog
            .as_mut()
            .and_then(|dialog| dialog.focus_request.take());
        if let Some(key) = focus_request {
            if let Some((handle, _)) = ring.iter().find(|(_, action)| action == &key) {
                if let Some(dialog) = self.recordings_dialog.as_mut() {
                    dialog.focused_key = Some(key);
                }
                window.focus(handle);
            } else {
                if let Some(dialog) = self.recordings_dialog.as_mut() {
                    dialog.focused_key = None;
                }
                window.focus(&self.recordings_close_focus);
            }
        }

        card = card.on_mouse_up(
            MouseButton::Left,
            cx.listener(|s, _: &gpui::MouseUpEvent, _, cx| {
                if let Some(dialog) = s.recordings_dialog.as_mut() {
                    let pressed = dialog.pressed.take().is_some();
                    let backdrop = std::mem::replace(&mut dialog.backdrop_pressed, false);
                    if pressed || backdrop {
                        cx.notify();
                    }
                }
            }),
        );

        let close_ring = ring.clone();
        let weak = cx.entity().downgrade();
        card = card.capture_key_down(cx.listener(
            move |s, event: &gpui::KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if key == "escape" {
                    s.close_recordings(true, window, cx);
                    cx.stop_propagation();
                    return;
                }
                if key == "tab" {
                    if close_ring.is_empty() {
                        return;
                    }
                    let current = close_ring
                        .iter()
                        .position(|(handle, _)| handle.is_focused(window));
                    let next = match (current, event.keystroke.modifiers.shift) {
                        (Some(index), false) => (index + 1) % close_ring.len(),
                        (Some(index), true) => (index + close_ring.len() - 1) % close_ring.len(),
                        (None, false) => 0,
                        (None, true) => close_ring.len() - 1,
                    };
                    if let Some(dialog) = s.recordings_dialog.as_mut() {
                        dialog.focus_action(&close_ring[next].1);
                        dialog.focused_key = Some(close_ring[next].1.clone());
                    }
                    window.focus(&close_ring[next].0);
                    cx.stop_propagation();
                    return;
                }
                if (key == "enter" || key == "space") && !event.keystroke.modifiers.control {
                    let focused = close_ring
                        .iter()
                        .find(|(handle, _)| handle.is_focused(window))
                        .map(|(_, key)| key.clone());
                    let Some(key) = focused else { return };
                    let activate = s
                        .recordings_dialog
                        .as_mut()
                        .is_some_and(|dialog| dialog.activate_key(&key));
                    if !activate {
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                    let weak = weak.clone();
                    window.defer(cx, move |window, cx| {
                        if let Some(view) = weak.upgrade() {
                            view.update(cx, |view, cx| view.run_recording_action(&key, window, cx));
                        }
                    });
                    cx.stop_propagation();
                }
            },
        ));

        Some(
            div()
                .id("recordings-backdrop")
                .absolute()
                .size_full()
                .occlude()
                .bg(gpui::rgba(0x000000ad))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|s, event: &gpui::MouseDownEvent, _window, cx| {
                        let inside = s.recordings_dialog.as_ref().is_some_and(|dialog| {
                            dialog
                                .bounds
                                .contains((event.position.x.into(), event.position.y.into()))
                        });
                        if let Some(dialog) = s.recordings_dialog.as_mut() {
                            dialog.backdrop_pressed = !inside;
                        }
                        // The backdrop is a full input barrier: the search and
                        // every other control below the modal never see this
                        // press, even when the click lands outside the card.
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, event: &gpui::MouseUpEvent, window, cx| {
                        let inside = s.recordings_dialog.as_ref().is_some_and(|dialog| {
                            dialog
                                .bounds
                                .contains((event.position.x.into(), event.position.y.into()))
                        });
                        let completed = s.recordings_dialog.as_mut().is_some_and(|dialog| {
                            let completed = dialog.backdrop_pressed && !inside;
                            dialog.backdrop_pressed = false;
                            // Any release that did not activate a card action
                            // clears the press, including a release outside the
                            // button or in the clipped part of the list.
                            dialog.pressed = None;
                            completed
                        });
                        if completed {
                            s.close_recordings(true, window, cx);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(card)
                .into_any_element(),
        )
    }

    fn recording_card(
        &mut self,
        row: &recordings_dialog::RowView,
        image: Option<Arc<gpui::RenderImage>>,
        list: Rect,
        busy: &str,
        confirm: &str,
        ring: &mut Vec<(FocusHandle, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let card = row.card;
        let ox = card.x - list.x;
        let oy = card.y - list.y;
        let stacked = recordings_dialog::stacked(card.w - 2. * recordings::CARD_PADDING);
        let details_x = if stacked {
            recordings::CARD_PADDING
        } else {
            recordings::CARD_PADDING + recordings::PREVIEW_WIDTH + 14.
        };
        let details_w = card.w
            - 2. * recordings::CARD_PADDING
            - if stacked {
                0.
            } else {
                recordings::PREVIEW_WIDTH + 14.
            };
        let mut element = div()
            .absolute()
            .left(px(ox))
            .top(px(oy))
            .w(px(card.w))
            .h(px(card.h))
            .rounded(px(12.))
            .border_1()
            .border_color(ink(p.line))
            .bg(ink(p.surface))
            .overflow_hidden();

        let preview = row.preview;
        let mut picture = div()
            .absolute()
            .left(px(preview.x - card.x))
            .top(px(preview.y - card.y))
            .w(px(preview.w))
            .h(px(preview.h))
            .rounded(px(8.))
            .border_1()
            .border_color(ink(p.line))
            .bg(ink(p.bg))
            .overflow_hidden();
        if let Some(image) = image {
            picture = picture.child(
                gpui::img(image)
                    .size_full()
                    .object_fit(gpui::ObjectFit::Contain),
            );
        } else {
            let label = if row.live {
                "Идёт запись"
            } else {
                "Без превью"
            };
            picture = picture
                .child(
                    div()
                        .absolute()
                        .left(px((preview.w - 24.) / 2.))
                        .top(px(preview.h / 2. - 22.))
                        .child(icon("icons/monitor.svg", 24., ink(p.muted))),
                )
                .child(rec_text(
                    label,
                    0.,
                    preview.h / 2. + 3.,
                    preview.w,
                    16.,
                    11.,
                    p.muted,
                    false,
                ));
        }
        element = element.child(picture);

        let top = row.details_top - card.y;
        element = element
            .child(
                div()
                    .absolute()
                    .left(px(details_x))
                    .top(px(top))
                    .w(px(38.))
                    .h(px(38.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(ink(p.line))
                    .bg(ink(p.bg))
                    .child(div().absolute().left(px(9.)).top(px(9.)).child(icon(
                        "icons/inbox.svg",
                        20.,
                        ink(p.muted),
                    ))),
            )
            .child(rec_text(
                &self.recording_channel_name(row.record.channel_id),
                details_x + 49.,
                top + 1.,
                (details_w - 49.).max(40.),
                19.,
                13.,
                p.fg,
                true,
            ))
            .child(rec_text(
                &recordings::date(row.record.created_at),
                details_x + 49.,
                top + 21.,
                (details_w - 49.).max(40.),
                16.,
                11.,
                p.muted,
                false,
            ));
        let meta = format!(
            "{} · {} · {}",
            row.record.owner_username,
            recordings::duration(row.record.duration_ms),
            recordings::size(row.record.size_bytes)
        );
        let meta_y = top + 38. + 8.;
        element = element.child(rec_text(
            &meta, details_x, meta_y, details_w, 15., 11., p.muted, false,
        ));
        let status = recordings::status_text(&row.record, super::recordings_ops::now_secs());
        let status_y = meta_y + 15. + 8.;
        let status_x = if row.live {
            element = element.child(
                div()
                    .absolute()
                    .left(px(details_x))
                    .top(px(status_y + 4.))
                    .w(px(8.))
                    .h(px(8.))
                    .rounded_full()
                    .bg(ink(p.danger)),
            );
            details_x + 14.
        } else {
            details_x
        };
        for (index, line) in recordings_dialog::wrap_lines(&status, details_w - 14., 11.)
            .iter()
            .enumerate()
        {
            element = element.child(rec_text(
                line,
                status_x,
                status_y + index as f32 * 16.,
                details_w - 14.,
                16.,
                11.,
                if row.live { p.danger } else { p.muted },
                false,
            ));
        }
        if row.confirm {
            let question =
                "Удалить видео и превью с сервера для всех участников? Скачанные копии останутся.";
            let lines = recordings_dialog::wrap_lines(question, details_w, 11.);
            let top = card.h - recordings::CARD_PADDING - 30. - 8. - 32.;
            for (index, line) in lines.iter().take(2).enumerate() {
                element = element.child(rec_text(
                    line,
                    details_x,
                    top + index as f32 * 16.,
                    details_w,
                    16.,
                    11.,
                    p.fg,
                    false,
                ));
            }
        }

        let id = row.record.id.clone();
        let download_key = format!("rec-download-{id}");
        let download = row.download;
        if download.w > 0. {
            let label = if busy == id {
                "Подождите…".to_string()
            } else {
                "Скачать".to_string()
            };
            let disabled = !row.ready || !busy.is_empty();
            let button = self.rec_button(
                &download_key,
                &label,
                Rect::new(download.x - card.x, download.y - card.y, download.w, 30.),
                RecButton::Secondary,
                disabled,
                ring,
                window,
                cx,
            );
            element = element.child(button);
        }
        if let Some(delete) = row.delete.filter(|_| confirm != id) {
            let key = format!("rec-delete-{id}");
            let button = self.rec_button(
                &key,
                "Удалить с сервера",
                Rect::new(delete.x - card.x, delete.y - card.y, delete.w, 24.),
                RecButton::Link,
                !busy.is_empty(),
                ring,
                window,
                cx,
            );
            element = element.child(button);
        }
        if let Some(yes) = row.confirm_yes {
            let key = format!("rec-confirm-{id}");
            let button = self.rec_button(
                &key,
                "Удалить с сервера",
                Rect::new(yes.x - card.x, yes.y - card.y, yes.w, 30.),
                RecButton::Danger,
                !busy.is_empty(),
                ring,
                window,
                cx,
            );
            element = element.child(button);
        }
        if let Some(no) = row.confirm_no {
            let key = format!("rec-cancel-{id}");
            let button = self.rec_button(
                &key,
                "Отмена",
                Rect::new(no.x - card.x, no.y - card.y, no.w, 30.),
                RecButton::Secondary,
                !busy.is_empty(),
                ring,
                window,
                cx,
            );
            element = element.child(button);
        }
        element.into_any_element()
    }

    fn rec_button(
        &mut self,
        id: &str,
        label: &str,
        rect: Rect,
        kind: RecButton,
        disabled: bool,
        ring: &mut Vec<(FocusHandle, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = self.palette;
        let focus = if id == "rec-close" {
            self.recordings_close_focus.clone()
        } else {
            self.focus_id(ElementId::Name(id.to_string().into()), window, cx)
        };
        if !disabled {
            ring.push((focus.clone(), id.to_string()));
        }
        let key = id.to_string();
        let card_action = key.starts_with("rec-download-")
            || key.starts_with("rec-delete-")
            || key.starts_with("rec-confirm-")
            || key.starts_with("rec-cancel-");
        let down_key = key.clone();
        let down_focus = focus.clone();
        let up_key = key.clone();
        let mut button = div()
            .id(ElementId::Name(id.to_string().into()))
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(9.))
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .text_size(px(12.))
            .font_weight(gpui::FontWeight::BOLD)
            .track_focus(&focus)
            .tab_index(0)
            .when(kind == RecButton::Secondary, |button| {
                button.bg(ink(p.surface)).border_1().border_color(ink(
                    if focus.is_focused(window) {
                        p.lime
                    } else {
                        p.line
                    },
                ))
            })
            .when(kind == RecButton::Danger, |button| {
                button
                    .bg(super::ui::ink_alpha(p.danger, 20))
                    .border_1()
                    .border_color(super::ui::ink_alpha(p.danger, 140))
                    .text_color(ink(p.danger))
            })
            .when(kind == RecButton::Link, |button| {
                button
                    .justify_start()
                    .text_color(ink(p.muted))
                    .hover(|button| button.text_color(ink(p.danger)))
            })
            .when(kind == RecButton::Icon, |button| {
                button
                    .text_color(ink(p.muted))
                    .hover(|button| button.text_color(ink(p.fg)).bg(ink(p.surface)))
            })
            .when(disabled, |button| button.opacity(0.55))
            .when(!disabled, |button| {
                button
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |s, event: &gpui::MouseDownEvent, window, cx| {
                            let point = (event.position.x.into(), event.position.y.into());
                            if let Some(dialog) = s.recordings_dialog.as_mut() {
                                let hit = if card_action {
                                    dialog.action_hit(&down_key, point)
                                } else {
                                    let bounds = Rect::new(
                                        dialog.bounds.x + rect.x,
                                        dialog.bounds.y + rect.y,
                                        rect.w,
                                        rect.h,
                                    );
                                    bounds.contains(point)
                                };
                                if hit {
                                    dialog.focus_action(&down_key);
                                    dialog.focused_key = Some(down_key.clone());
                                    dialog.pressed = Some(down_key.clone());
                                    window.focus(&down_focus);
                                    cx.notify();
                                }
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |s, event: &gpui::MouseUpEvent, window, cx| {
                            let point = (event.position.x.into(), event.position.y.into());
                            let fire = s.recordings_dialog.as_mut().is_some_and(|dialog| {
                                let pressed = dialog.pressed.take();
                                if pressed.as_deref() != Some(up_key.as_str()) {
                                    return false;
                                }
                                if card_action {
                                    dialog.action_hit(&up_key, point)
                                } else {
                                    let bounds = Rect::new(
                                        dialog.bounds.x + rect.x,
                                        dialog.bounds.y + rect.y,
                                        rect.w,
                                        rect.h,
                                    );
                                    bounds.contains(point)
                                }
                            });
                            if fire {
                                s.run_recording_action(&up_key, window, cx);
                            } else {
                                cx.notify();
                            }
                        }),
                    )
            });
        if kind == RecButton::Icon {
            button = button.child(icon("icons/close.svg", 16., ink(p.muted)));
        } else if kind == RecButton::Link {
            button = button
                .child(icon("icons/trash.svg", 13., ink(p.muted)))
                .child(label.to_string());
        } else {
            button = button.child(label.to_string());
        }
        button
    }

    fn recording_channel_name(&self, id: i64) -> String {
        self.channels
            .iter()
            .find(|channel| channel.id == id)
            .map(|channel| channel.name.clone())
            .unwrap_or_else(|| format!("Канал #{id}"))
    }

    pub(super) fn run_recording_action(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key == "rec-close" || key == "rec-dismiss" {
            self.close_recordings(true, window, cx);
            return;
        }
        if key == "rec-retry" {
            self.refresh_recordings(0, window, cx);
            return;
        }
        if let Some(id) = key.strip_prefix("rec-download-") {
            if let Some(record) = self.record_by_id(id) {
                self.download_recording(record, window, cx);
            }
            return;
        }
        if let Some(id) = key.strip_prefix("rec-delete-") {
            if self.record_by_id(id).is_some() {
                let cancel = format!("rec-cancel-{id}");
                if let Some(dialog) = self.recordings_dialog.as_mut() {
                    dialog.confirm = id.to_string();
                    dialog.error.clear();
                    dialog.focused_key = Some(cancel.clone());
                    dialog.focus_request = Some(cancel);
                }
                cx.notify();
            }
            return;
        }
        if let Some(id) = key.strip_prefix("rec-confirm-") {
            if let Some(record) = self.record_by_id(id) {
                self.delete_recording(record, window, cx);
            }
            return;
        }
        if let Some(id) = key.strip_prefix("rec-cancel-") {
            let delete = format!("rec-delete-{id}");
            if let Some(dialog) = self.recordings_dialog.as_mut() {
                dialog.confirm.clear();
                dialog.focused_key = Some(delete.clone());
                dialog.focus_request = Some(delete);
            }
            cx.notify();
        }
    }

    fn record_by_id(&self, id: &str) -> Option<Recording> {
        self.recordings
            .records
            .iter()
            .find(|record| record.id == id)
            .cloned()
    }

    /// Start at most two private preview requests for the rows that are
    /// actually painted, and only after the list is confirmed for this access
    /// generation.
    pub(super) fn request_visible_recording_previews(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.recordings_visible_for_preview() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(list) = self.recordings_dialog.as_ref().map(|dialog| dialog.list) else {
            return;
        };
        let requests: Vec<(String, String)> = {
            let Some(dialog) = self.recordings_dialog.as_ref() else {
                return;
            };
            dialog
                .rows
                .iter()
                .filter(|row| {
                    row.card.bottom() >= list.y
                        && row.card.y <= list.bottom()
                        && row.record.status == RECORDING_READY
                })
                .filter_map(|row| {
                    let version = row.record.preview_version.clone()?;
                    if version.is_empty()
                        || !self.recording_preview_allowed(&row.record.id, &version)
                    {
                        return None;
                    }
                    let key = format!("{}@{version}", row.record.id);
                    if dialog.previews.contains(&key)
                        || dialog.preview_pending.contains(&key)
                        || dialog
                            .preview_retry
                            .get(&key)
                            .is_some_and(|at| *at > std::time::Instant::now())
                    {
                        return None;
                    }
                    Some((row.record.id.clone(), version))
                })
                .take(recordings::PREVIEW_PARALLEL)
                .collect()
        };
        for (id, version) in requests {
            if self
                .recordings_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.preview_pending.len() >= recordings::PREVIEW_PARALLEL)
            {
                break;
            }
            let key = format!("{id}@{version}");
            let (generation, preview_generation) = match self.recordings_dialog.as_ref() {
                Some(dialog) => (dialog.generation, dialog.preview_generation),
                None => break,
            };
            let epoch = self.recording_access_epoch;
            if let Some(dialog) = self.recordings_dialog.as_mut() {
                dialog.preview_pending.insert(key.clone());
            }
            let runtime = self.runtime.clone();
            let client = client.clone();
            let request_id = id.clone();
            let request_version = version.clone();
            let handle = runtime.spawn(async move {
                client
                    .recording_preview(&request_id, &request_version)
                    .await
            });
            self.recordings_preview_aborts.retain(|request| {
                !(request.key == key
                    && request.dialog == generation
                    && request.generation == preview_generation)
            });
            self.recordings_preview_aborts.push(super::PreviewAbort {
                key: key.clone(),
                dialog: generation,
                generation: preview_generation,
                abort: handle.abort_handle(),
            });
            cx.spawn_in(window, async move |this, window| {
                let result = handle.await;
                let _ = this.update_in(window, |s, window, cx| {
                    s.recordings_preview_aborts.retain(|request| {
                        !(request.key == key
                            && request.dialog == generation
                            && request.generation == preview_generation)
                    });
                    s.finish_recording_preview(
                        key,
                        generation,
                        preview_generation,
                        epoch,
                        id,
                        version,
                        result.ok(),
                        window,
                        cx,
                    );
                });
            })
            .detach();
        }
    }

    /// Apply one preview answer only while the request still owns the current
    /// dialog preview scope. A late image from the previous generation must not
    /// clear the new request's pending entry or replace its cache.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_recording_preview(
        &mut self,
        key: String,
        generation: u64,
        preview_generation: u64,
        epoch: u64,
        id: String,
        version: String,
        result: Option<std::result::Result<image::DynamicImage, voxhold_api::Error>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let allowed = self.recording_preview_allowed(&id, &version);
        let owns = self.recordings_dialog.as_ref().is_some_and(|dialog| {
            dialog.generation == generation
                && dialog.preview_generation == preview_generation
                && dialog.epoch == epoch
                && self.recording_access_epoch == epoch
                && dialog.preview_pending.contains(&key)
        });
        if !owns {
            return;
        }
        let Some(dialog) = self.recordings_dialog.as_mut() else {
            return;
        };
        dialog.preview_pending.remove(&key);
        if !allowed {
            cx.notify();
            return;
        }
        match result {
            Some(Ok(image)) => {
                let image = if image.width() > recordings::PREVIEW_WIDTH as u32
                    || image.height() > recordings::PREVIEW_HEIGHT as u32
                {
                    image.resize(
                        recordings::PREVIEW_WIDTH as u32,
                        recordings::PREVIEW_HEIGHT as u32,
                        image::imageops::FilterType::CatmullRom,
                    )
                } else {
                    image
                };
                let bytes = (image.width() * image.height() * 4) as usize;
                let frame = crate::members::avatar_frame(image);
                let image = Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
                for old in dialog.previews.put(key.clone(), image, bytes) {
                    let _ = window.drop_image(old);
                }
                dialog.preview_retry.remove(&key);
            }
            Some(Err(error)) => {
                let delay = if error.status() == Some(429) {
                    std::time::Duration::from_secs(15)
                } else {
                    std::time::Duration::from_secs(5)
                };
                let delay = if dialog.preview_retry.contains_key(&key) {
                    delay * 2
                } else {
                    delay
                };
                dialog
                    .preview_retry
                    .insert(key.clone(), std::time::Instant::now() + delay);
            }
            // Aborted by close or a newer preview scope: pending and cache
            // already belong to that newer scope.
            None => return,
        }
        cx.notify();
    }
}

fn rec_text(
    text: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    size: f32,
    color: u32,
    bold: bool,
) -> AnyElement {
    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .text_size(px(size))
        .text_color(ink(color))
        .when(bold, |text| text.font_weight(gpui::FontWeight::BOLD))
        .overflow_hidden()
        .child(text.to_string())
        .into_any_element()
}
