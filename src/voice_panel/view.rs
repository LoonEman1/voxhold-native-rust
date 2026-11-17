//! Painting of the voice room and the sidebar dock. The geometry comes from
//! `voice_room`, which mirrors the Go layout numbers.

use super::*;

impl VoicePanel {
    fn room_phase(&self) -> CallPhase {
        if self.call_channel != 0 && self.call_channel == self.room {
            self.phase
        } else {
            CallPhase::Idle
        }
    }

    fn action_icon(&self, action: &VoiceAction) -> &'static str {
        match action.id {
            "voice-mic" if self.self_muted => "icons/mic-off.svg",
            "voice-mic" => "icons/mic.svg",
            "voice-deaf" if self.controls.deafened => "icons/headphones-off.svg",
            "voice-deaf" => "icons/headphones.svg",
            "voice-records" => "icons/recordings.svg",
            _ => "icons/logout.svg",
        }
    }

    fn action_active(&self, action: &VoiceAction) -> bool {
        match action.id {
            "voice-mic" => self.self_muted,
            "voice-deaf" => self.controls.deafened,
            "voice-leave" => true,
            _ => false,
        }
    }

    fn action_button(
        &self,
        index: usize,
        action: &VoiceAction,
        labels: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = self.colors();
        let active = self.action_active(action);
        let color = if active { c.danger } else { c.muted };
        let action_id = action.id;
        div()
            .id(("voice-action", index))
            .debug_selector(move || format!("voice-action-{index}").into())
            .h_full()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .rounded(px(10.))
            .cursor_pointer()
            .bg(rgb(if active { c.danger_bg } else { c.surface }))
            .border_1()
            .border_color(rgb(if active { c.danger } else { c.line }))
            .text_color(rgb(color))
            .track_focus(&self.action_focus[index])
            .child(img(self.action_icon(action)).w(px(16.)).h(px(16.)))
            .when(labels, |button| {
                button.child(div().text_size(px(11.)).child(action.label))
            })
            .on_click(cx.listener(move |panel, _, window, cx| match action_id {
                "voice-mic" => panel.mute(cx),
                "voice-deaf" => panel.deafen(cx),
                "voice-records" => panel.trigger_recordings(window, cx),
                _ => panel.leave_in_window(window, cx),
            }))
    }

    pub(super) fn visible_streams(&self) -> Vec<Publication> {
        self.room_streams()
            .iter()
            .filter(|publication| publication.channel_id == self.room)
            .cloned()
            .collect()
    }

    fn stream_tile(
        &mut self,
        index: usize,
        rect: Rect,
        publication: &Publication,
        active: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let name = self.member_name(publication.publisher_user_id);
        let watching = self.watching.as_ref().is_some_and(|current| {
            current.publisher_connection_id == publication.publisher_connection_id
        });
        let status = if active {
            format!(
                "{} · {} смотрят",
                publication.codec.to_uppercase(),
                publication.viewer_count
            )
        } else {
            "Подключитесь к каналу для просмотра".into()
        };
        let publication = publication.clone();
        let focus = &self.stream_focus[index.min(self.stream_focus.len() - 1)];
        let mut card = div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(12.))
            .bg(rgb(c.surface))
            .border_1()
            .border_color(rgb(c.line))
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top(px(12.))
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(5.))
                    .bg(rgb(c.live_bg))
                    .text_size(px(10.))
                    .text_color(rgb(c.danger))
                    .child("LIVE"),
            )
            .child(
                img("icons/monitor.svg")
                    .absolute()
                    .left(px((rect.w - 50.) / 2.))
                    .top(px(rect.h * 0.27))
                    .w(px(50.))
                    .h(px(50.)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(11.))
                    .top(px(rect.h * 0.27 + 60.))
                    .w(px(rect.w - 22.))
                    .text_center()
                    .text_size(px(14.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .truncate()
                    .child(name),
            )
            .child(
                div()
                    .absolute()
                    .left(px(14.))
                    .top(px(rect.h - 72.))
                    .w(px(rect.w - 28.))
                    .text_size(px(11.))
                    .text_color(rgb(c.muted))
                    .child(status),
            );
        if watching {
            card = card.child(
                div()
                    .id(("stream-full", index))
                    .absolute()
                    .left(px(14.))
                    .top(px(rect.h - 46.))
                    .w(px(rect.w - 28.))
                    .h(px(34.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(rgb(c.accent))
                    .text_color(rgb(c.accent_text))
                    .text_size(px(13.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(focus)
                    .on_click(
                        cx.listener(|panel, _, window, _| panel.player_toggle_fullscreen(window)),
                    )
                    .child("На весь экран"),
            );
        } else if active {
            card = card.child(
                div()
                    .id(("stream-watch", index))
                    .absolute()
                    .left(px(14.))
                    .top(px(rect.h - 46.))
                    .w(px(rect.w - 28.))
                    .h(px(34.))
                    .rounded(px(9.))
                    .bg(rgb(c.accent))
                    .text_color(rgb(c.ink))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(focus)
                    .on_click(
                        cx.listener(move |panel, _, _, cx| panel.watch(publication.clone(), cx)),
                    )
                    .child("Смотреть"),
            );
        }
        card.into_any_element()
    }

    fn member_card(
        &mut self,
        slot: usize,
        rect: Rect,
        participant: &Participant,
        active: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let speaking = active
            && !participant.reconnecting
            && !participant.self_mute
            && !participant.server_mute
            && self
                .speaking
                .get(&participant.connection_id)
                .copied()
                .unwrap_or(false);
        let muted = participant.self_mute
            || participant.server_mute
            || self.local_mutes.contains(&participant.connection_id);
        let name = self.member_name(participant.user_id);
        let self_user = self.self_user_id != 0 && participant.user_id == self.self_user_id;
        let icon =
            voice_room::voice_state_icon(participant.reconnecting, participant.self_deaf, muted);
        let avatar = (rect.w * 0.42).min(84.0);
        let has_audio = self.audio_for(&participant.connection_id);
        let local_muted = self.local_mutes.contains(&participant.connection_id);
        let connection = participant.connection_id.clone();
        let dot = if muted || participant.self_deaf {
            c.danger
        } else {
            c.accent
        };
        let mut card = div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(12.))
            .bg(rgb(c.surface))
            .when(c.light, |card| card.border_1().border_color(rgb(c.line)))
            .when(speaking, |card| card.border_3().border_color(rgb(c.accent)))
            .child(
                div()
                    .absolute()
                    .left(px((rect.w - avatar) / 2.))
                    .top(px((rect.h - avatar) / 2. - 14.))
                    .w(px(avatar))
                    .h(px(avatar))
                    .rounded_full()
                    .bg(rgb(c.avatar_bg))
                    .when(speaking, |avatar| {
                        avatar.border_3().border_color(rgb(c.accent))
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(avatar * 0.34))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(c.fg))
                    .child(initials(&name)),
            )
            .child(
                div()
                    .absolute()
                    .left(px((rect.w + avatar) / 2. - 12.))
                    .top(px((rect.h - avatar) / 2. - 26.))
                    .w(px(12.))
                    .h(px(12.))
                    .rounded_full()
                    .bg(rgb(dot)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(20.))
                    .top(px((rect.h - avatar) / 2. + avatar - 6.))
                    .w(px((rect.w - 40.).max(0.)))
                    .flex()
                    .justify_center()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .max_w(px(rect.w - 70.))
                            .truncate()
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(if self_user { c.accent_text } else { c.fg }))
                            .child(name),
                    )
                    .when(self_user, |row| {
                        row.child(
                            div()
                                .px(px(4.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .bg(rgb(c.accent))
                                .text_color(rgb(c.ink))
                                .text_size(px(10.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("ВЫ"),
                        )
                    }),
            );
        if let Some(label) = icon {
            card = card.child(
                div()
                    .absolute()
                    .left(px(13.))
                    .top(px(rect.h - 36.))
                    .px(px(6.))
                    .py(px(3.))
                    .rounded(px(6.))
                    .bg(rgb(c.danger_bg))
                    .text_size(px(10.))
                    .text_color(rgb(c.danger))
                    .child(label),
            );
        }
        if has_audio {
            let focus = &self.member_focus[slot.min(self.member_focus.len() - 1)];
            card =
                card.child(
                    div()
                        .id(("voice-local", slot))
                        .absolute()
                        .left(px(rect.w - 38.))
                        .top(px(rect.h - 40.))
                        .w(px(28.))
                        .h(px(30.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .bg(rgb(if local_muted {
                            c.danger_bg
                        } else {
                            c.avatar_bg
                        }))
                        .track_focus(focus)
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.local_mute(connection.clone(), cx)
                        }))
                        .child(
                            img(if local_muted {
                                "icons/headphones-off.svg"
                            } else {
                                "icons/volume.svg"
                            })
                            .w(px(14.))
                            .h(px(14.)),
                        ),
                );
        }
        card.into_any_element()
    }

    fn room_cards(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let (width, height) = self.room_size;
        let active = self.call_channel != 0 && self.call_channel == self.room;
        let roster = self.visible_roster();
        let streams = self.visible_streams();
        let count = roster.len() + streams.len();
        let geometry = voice_room::room_geometry(
            Rect::new(0.0, 0.0, width, height),
            active,
            count,
            self.room_scroll,
        );
        let max_scroll = (geometry.content_height - geometry.viewport.h).max(0.0);
        self.room_scroll = self.room_scroll.clamp(0.0, max_scroll);
        let mut children = Vec::new();
        for (index, publication) in streams.iter().enumerate() {
            let Some(rect) = geometry.cards.get(index) else {
                break;
            };
            children.push(self.stream_tile(index, *rect, publication, active, cx));
        }
        let mut slot = 0;
        for (index, participant) in roster.iter().enumerate() {
            let Some(rect) = geometry.cards.get(streams.len() + index) else {
                break;
            };
            let audio = self.audio_for(&participant.connection_id);
            children.push(self.member_card(slot, *rect, participant, active, cx));
            if audio {
                slot += 1;
            }
        }
        children
    }

    pub(super) fn room_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = self.call_channel != 0 && self.call_channel == self.room;
        let phase = self.room_phase();
        let c = self.colors();
        let (title, tone) = voice_room::room_status(phase);
        let color = match tone {
            Tone::Lime => c.accent_text,
            Tone::Danger => c.danger,
            Tone::Muted => c.muted,
        };
        let cards = self.room_cards(cx);
        let (width, height) = self.room_size;
        let geometry = voice_room::room_geometry(
            Rect::new(0.0, 0.0, width, height),
            active,
            cards.len(),
            self.room_scroll,
        );
        let wheel = cx.listener(|panel, event: &gpui::ScrollWheelEvent, _, cx| {
            let delta: f32 = event.delta.pixel_delta(px(42.)).y.into();
            panel.room_scroll = (panel.room_scroll - delta).max(0.0);
            let (width, height) = panel.room_size;
            let active = panel.call_channel != 0 && panel.call_channel == panel.room;
            let count = panel.visible_roster().len() + panel.visible_streams().len();
            let geometry = voice_room::room_geometry(
                Rect::new(0.0, 0.0, width, height),
                active,
                count,
                panel.room_scroll,
            );
            panel.room_scroll = panel.room_scroll.clamp(
                0.0,
                (geometry.content_height - geometry.viewport.h).max(0.0),
            );
            cx.notify();
            cx.stop_propagation();
        });
        let live = self
            .recordings_live
            .as_ref()
            .and_then(|slot| slot.borrow().clone());
        let mut body = div().relative().size_full().text_color(rgb(c.fg)).child(
            div()
                .absolute()
                .left(px(24.))
                .top(px(9.))
                .h(px(24.))
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(rgb(color)))
                .child(div().text_size(px(12.)).text_color(rgb(color)).child(title)),
        );
        if !active && !self.error.is_empty() {
            body = body.child(
                div()
                    .absolute()
                    .left(px(24.))
                    .top(px(34.))
                    .text_size(px(12.))
                    .text_color(rgb(c.danger))
                    .child(self.error.clone()),
            );
        }
        if let Some(live) = live.filter(|_| width > 240.) {
            body = body.child(
                div()
                    .absolute()
                    .right(px(24.))
                    .top(px(9.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(c.danger))
                    .child(format!("Идёт запись · {live}")),
            );
        }
        // Pre-gate level of the real capture track while the call is active.
        if active {
            let level = (self.local_level as f32).clamp(0., 1.);
            body = body.child(
                div()
                    .absolute()
                    .right(px(24.))
                    .top(px(34.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(10.))
                    .text_color(rgb(c.muted))
                    .child("Микрофон")
                    .child(
                        div()
                            .w(px(90.))
                            .h(px(5.))
                            .rounded(px(3.))
                            .bg(rgb(c.avatar_bg))
                            .child(div().h(px(5.)).w(px(90. * level)).rounded(px(3.)).bg(rgb(
                                if self.controls.muted {
                                    c.muted
                                } else {
                                    c.accent
                                },
                            ))),
                    ),
            );
        }
        if !active && self.visible_roster().is_empty() && self.visible_streams().is_empty() {
            body = body.child(
                div()
                    .absolute()
                    .left(px(24.))
                    .top(px(64.))
                    .text_size(px(14.))
                    .text_color(rgb(c.muted))
                    .child("В канале пока никого нет"),
            );
        }
        body = body.child(
            div()
                .id("voice-room-cards")
                .debug_selector(|| "voice-room-viewport".into())
                .absolute()
                .left(px(geometry.viewport.x))
                .top(px(geometry.viewport.y))
                .w(px(geometry.viewport.w.max(1.)))
                .h(px(geometry.viewport.h.max(1.)))
                .overflow_hidden()
                .on_scroll_wheel(wheel)
                .children(cards),
        );
        if active {
            let toolbar = geometry.toolbar;
            let mut bar = div()
                .absolute()
                .left(px(toolbar.x))
                .top(px(toolbar.y))
                .w(px(toolbar.w))
                .h(px(toolbar.h))
                .rounded(px(14.))
                .bg(rgb(c.surface))
                .border_1()
                .border_color(rgb(c.line))
                .flex()
                .gap(px(5.))
                .p(px(7.));
            for (index, action) in VOICE_ACTIONS.iter().enumerate() {
                bar = bar.child(self.action_button(index, action, true, cx));
            }
            body = body.child(bar);
        } else {
            let join = geometry.toolbar;
            body = body.child(
                div()
                    .id("voice-join")
                    .debug_selector(|| "voice-join".into())
                    .absolute()
                    .left(px(24.))
                    .top(px(join.y))
                    .w(px(join.w))
                    .h(px(40.))
                    .rounded(px(11.))
                    .bg(rgb(c.accent))
                    .text_color(rgb(c.ink))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&self.join_focus)
                    .on_click(cx.listener(|panel, _, _, cx| panel.join(cx)))
                    .child("Подключиться"),
            );
        }
        if active && !self.device_error.is_empty() {
            body = body.child(
                div()
                    .absolute()
                    .left(px(24.))
                    .top(px(52.))
                    .text_size(px(11.))
                    .text_color(rgb(c.danger))
                    .child(self.device_error.clone()),
            );
        }
        let _ = height;
        body.into_any_element()
    }
}

fn initials(name: &str) -> String {
    let letters: String = name
        .trim()
        .chars()
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        "VX".into()
    } else {
        letters
    }
}

impl Render for VoicePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("voice-panel")
            .track_focus(&self.focus)
            .key_context("VoiceRoom")
            .on_action(cx.listener(|panel, _: &VoiceTab, window, cx| {
                panel.cycle_focus(false, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|panel, _: &VoiceTabPrev, window, cx| {
                panel.cycle_focus(true, window, cx);
                cx.stop_propagation();
            }))
            .size_full()
            .child(self.room_body(cx))
    }
}
