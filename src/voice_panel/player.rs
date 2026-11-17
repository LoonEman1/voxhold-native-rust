//! Floating and fullscreen stream player. All positions are local to the
//! window, matching Go `layoutStreamPlayer` and `stream_player.go`.

use super::*;

impl VoicePanel {
    pub fn player_box(&self) -> Option<Rect> {
        if !self.player_open || self.player_area.is_empty() {
            return None;
        }
        let width = if self.player_width <= 0.0 {
            PLAYER_DEFAULT_WIDTH
        } else {
            self.player_width
        };
        let position = if self.player_placed {
            self.player_pos
        } else {
            (
                self.player_area.right() - width - 22.0,
                self.player_area.bottom() - (width * 9.0 / 16.0 + 76.0) - 22.0,
            )
        };
        Some(voice_room::floating_stream_box(
            self.player_area,
            position,
            width,
        ))
    }

    pub fn set_player_area_and_place(&mut self, area: Rect) {
        self.player_area = area;
        if let Some(player) = self.player_box() {
            self.player_pos = (player.x, player.y);
            self.player_placed = true;
        }
    }

    pub fn player_toggle_fullscreen(&mut self, window: &mut Window) {
        if !self.player_open {
            return;
        }
        if self.player_fullscreen {
            self.exit_fullscreen(window);
            window.focus(&self.player_actions[1]);
            return;
        }
        self.player_fullscreen = true;
        self.player_drag = None;
        if !window.is_fullscreen() {
            window.toggle_fullscreen();
            self.owns_fullscreen = true;
        }
        window.focus(&self.player_actions[1]);
    }

    pub fn exit_fullscreen(&mut self, window: &mut Window) {
        if self.owns_fullscreen && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.owns_fullscreen = false;
        self.player_fullscreen = false;
        self.player_drag = None;
        // Keep the local width and position: returning from fullscreen restores
        // the same floating box.
        if let Some(player) = self.player_box() {
            self.player_pos = (player.x, player.y);
        }
        window.focus(&self.player_actions[1]);
    }

    /// Returns whether the key belonged to the player.
    pub fn player_escape(&mut self, window: &mut Window) -> bool {
        if !self.player_open || !self.player_fullscreen {
            return false;
        }
        self.exit_fullscreen(window);
        true
    }

    pub fn player_key_f(&mut self, window: &mut Window) -> bool {
        if !self.player_open {
            return false;
        }
        let focused = self
            .player_actions
            .get(1)
            .is_some_and(|h| h.is_focused(window));
        if self.player_fullscreen || focused {
            self.player_toggle_fullscreen(window);
            return true;
        }
        false
    }

    pub fn player_fullscreen_focused(&self, window: &Window) -> bool {
        self.player_actions
            .get(1)
            .is_some_and(|handle| handle.is_focused(window))
    }

    pub fn player_pointer_down(&mut self, x: f32, y: f32) -> bool {
        let Some(player) = self.player_box() else {
            return false;
        };
        if self.player_fullscreen {
            return false;
        }
        let geometry = voice_room::player_geometry(self.player_area, player, self.has_audio());
        if geometry.corner.contains(x, y) {
            self.player_drag = Some(PlayerDrag {
                kind: DragKind::Resize,
                offset_x: 0.0,
                offset_y: 0.0,
            });
            return true;
        }
        if geometry.header.contains(x, y) {
            self.player_drag = Some(PlayerDrag {
                kind: DragKind::Move,
                offset_x: x - player.x,
                offset_y: y - player.y,
            });
            return true;
        }
        false
    }

    pub fn player_pointer_moved(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let (Some(drag), Some(player)) = (self.player_drag, self.player_box()) else {
            return;
        };
        match drag.kind {
            DragKind::Move => {
                self.player_pos = (x - drag.offset_x, y - drag.offset_y);
            }
            DragKind::Resize => {
                self.player_width = (x - player.x + 3.0).max(voice_room::PLAYER_MIN_WIDTH);
            }
        }
        if let Some(player) = self.player_box() {
            self.player_pos = (player.x, player.y);
        }
        cx.notify();
    }

    pub fn player_pointer_up(&mut self) {
        self.player_drag = None;
    }

    pub fn player_dragging(&self) -> bool {
        self.player_drag.is_some()
    }

    pub fn player_double_click(&mut self, window: &mut Window) {
        self.player_toggle_fullscreen(window);
    }

    pub(super) fn has_audio(&self) -> bool {
        self.watching
            .as_ref()
            .is_some_and(|publication| publication.has_audio)
    }
}

impl VoicePanel {
    fn player_labels(&self) -> (String, String, String) {
        if let Some(watching) = &self.watching {
            (
                self.rooms
                    .get(&watching.channel_id)
                    .map(|room| room.name.clone())
                    .unwrap_or_else(|| "Трансляция экрана".into()),
                watching.codec.to_uppercase(),
                self.member_name(watching.publisher_user_id),
            )
        } else {
            ("Трансляция экрана".into(), String::new(), String::new())
        }
    }

    fn player_status_line(&self) -> String {
        let error = if self.stream_audio_error.is_empty() {
            &self.stream_error
        } else {
            &self.stream_audio_error
        };
        voice_room::player_status(
            &self.stream_status,
            error,
            self.frame.is_some(),
            self.stream_connected,
        )
    }

    fn audio_button(
        &self,
        area: Rect,
        origin: (f32, f32),
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id("stream-audio")
            .absolute()
            .left(px(area.x - origin.0))
            .top(px(area.y - origin.1))
            .w(px(area.w))
            .h(px(area.h))
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .track_focus(&self.player_actions[0])
            .child(
                img(if self.stream_muted {
                    "icons/headphones-off.svg"
                } else {
                    "icons/volume.svg"
                })
                .w(px(16.))
                .h(px(16.)),
            )
            .on_click(cx.listener(|panel, _, _, cx| panel.toggle_stream_mute(cx)))
            .into_any_element()
    }

    /// Floating/fullscreen player overlay in window coordinates. `None` when
    /// the player is closed; the voice session is unaffected either way.
    pub fn player_overlay(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.player_open {
            return None;
        }
        if self.player_fullscreen {
            return Some(self.fullscreen_overlay(cx));
        }
        let player = self.player_box()?;
        let c = self.colors();
        let (title, codec, publisher) = self.player_labels();
        let status = self.player_status_line();
        let geometry = voice_room::player_geometry(self.player_area, player, self.has_audio());
        let local = |rect: Rect| Rect::new(rect.x - player.x, rect.y - player.y, rect.w, rect.h);
        let header = local(geometry.header);
        let picture = local(geometry.picture);
        let corner = local(geometry.corner);
        let fullscreen_button = local(geometry.fullscreen);
        let close = local(geometry.close);
        let audio = geometry.audio.map(local);

        let mut picture_div = div()
            .id("stream-picture")
            .debug_selector(|| "voice-player-picture".into())
            .absolute()
            .left(px(picture.x))
            .top(px(picture.y))
            .w(px(picture.w))
            .h(px(picture.h))
            .overflow_hidden()
            .bg(rgb(0x080808))
            .on_click(cx.listener(|panel, event: &gpui::ClickEvent, window, _| {
                if event.click_count() == 2 {
                    panel.player_double_click(window);
                }
            }));
        if let Some(image) = self.frame.clone() {
            picture_div = picture_div.child(
                img(image)
                    .w_full()
                    .h_full()
                    .object_fit(gpui::ObjectFit::Contain),
            );
        }
        if !status.is_empty() {
            picture_div = picture_div.child(
                div()
                    .absolute()
                    .left(px(8.))
                    .top(px((picture.h - 34.) / 2.))
                    .w(px((picture.w - 16.).max(0.)))
                    .h(px(34.))
                    .rounded(px(8.))
                    .bg(gpui::rgba(0x000000b3))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.))
                    .truncate()
                    .child(status),
            );
        }
        let mut card = div()
            .id("stream-player")
            .debug_selector(|| "voice-player".into())
            .absolute()
            .left(px(player.x))
            .top(px(player.y))
            .w(px(player.w))
            .h(px(player.h))
            .rounded(px(12.))
            .bg(rgb(c.surface))
            .border_1()
            .border_color(rgb(c.line))
            .child(picture_div)
            .child(
                div()
                    .id("stream-player-header")
                    .absolute()
                    .left(px(header.x))
                    .top(px(header.y))
                    .w(px(header.w))
                    .h(px(header.h))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_move()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|panel, event: &gpui::MouseDownEvent, _, _| {
                            let point = event.position;
                            panel.player_pointer_down(point.x.into(), point.y.into());
                        }),
                    )
                    .child(img("icons/grip.svg").w(px(12.)).h(px(14.)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .truncate()
                            .child(title),
                    ),
            )
            .child(
                div()
                    .id("stream-fullscreen")
                    .absolute()
                    .left(px(fullscreen_button.x))
                    .top(px(fullscreen_button.y))
                    .w(px(fullscreen_button.w))
                    .h(px(fullscreen_button.h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&self.player_actions[1])
                    .on_click(
                        cx.listener(|panel, _, window, _| panel.player_toggle_fullscreen(window)),
                    )
                    .child(img("icons/maximize.svg").w(px(14.)).h(px(14.))),
            )
            .child(
                div()
                    .id("stream-close")
                    .absolute()
                    .left(px(close.x))
                    .top(px(close.y))
                    .w(px(close.w))
                    .h(px(close.h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&self.player_actions[2])
                    .on_click(cx.listener(|panel, _, _, cx| panel.leave_stream(cx)))
                    .child(img("icons/close.svg").w(px(13.)).h(px(13.))),
            )
            .child(
                div()
                    .absolute()
                    .left(px(13.))
                    .top(px(player.h - 34.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .truncate()
                    .child(publisher),
            )
            .child(
                div()
                    .absolute()
                    .left(px(13.))
                    .top(px(player.h - 17.))
                    .text_size(px(10.))
                    .text_color(rgb(c.muted))
                    .child(codec),
            )
            .child(
                div()
                    .absolute()
                    .left(px(corner.x))
                    .top(px(corner.y))
                    .w(px(corner.w))
                    .h(px(corner.h))
                    .cursor_nwse_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|panel, event: &gpui::MouseDownEvent, _, _| {
                            let point = event.position;
                            panel.player_pointer_down(point.x.into(), point.y.into());
                        }),
                    ),
            );
        if let Some(audio) = audio {
            card = card.child(self.audio_button(audio, (0.0, 0.0), cx));
        }
        Some(card.into_any_element())
    }

    fn fullscreen_overlay(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let c = self.colors();
        let area = self.player_area;
        let status = self.player_status_line();
        let geometry = voice_room::player_geometry(area, area, self.has_audio());
        let picture = geometry.fullscreen_picture;
        let exit = geometry.fullscreen_exit;
        let audio = geometry.fullscreen_audio;
        let mut root = div()
            .id("stream-fullscreen-player")
            .absolute()
            .left(px(area.x))
            .top(px(area.y))
            .w(px(area.w))
            .h(px(area.h))
            .bg(rgb(0x060606));
        let mut frame = div()
            .id("stream-fullscreen-picture")
            .absolute()
            .left(px(0.))
            .top(px(0.))
            .w(px(picture.w))
            .h(px(picture.h))
            .on_click(cx.listener(|panel, event: &gpui::ClickEvent, window, _| {
                if event.click_count() == 2 {
                    panel.exit_fullscreen(window);
                }
            }));
        if let Some(image) = self.frame.clone() {
            frame = frame.child(
                img(image)
                    .w_full()
                    .h_full()
                    .object_fit(gpui::ObjectFit::Contain),
            );
        }
        if !status.is_empty() {
            frame = frame.child(
                div()
                    .absolute()
                    .left(px(8.))
                    .top(px((picture.h - 34.) / 2.))
                    .w(px((picture.w - 16.).max(0.)))
                    .h(px(34.))
                    .rounded(px(8.))
                    .bg(gpui::rgba(0x000000b3))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.))
                    .child(status),
            );
        }
        root = root.child(frame).child(
            div()
                .id("stream-exit-full")
                .absolute()
                .left(px(exit.x - area.x))
                .top(px(exit.y - area.y))
                .w(px(exit.w))
                .h(px(exit.h))
                .rounded(px(10.))
                .bg(rgb(c.surface))
                .border_1()
                .border_color(rgb(c.line))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_size(px(13.))
                .track_focus(&self.player_actions[1])
                .on_click(cx.listener(|panel, _, window, _| panel.exit_fullscreen(window)))
                .child("Выйти из полного экрана"),
        );
        if let Some(audio) = audio {
            root = root.child(self.audio_button(audio, (area.x, area.y), cx));
        }
        root.into_any_element()
    }
}
