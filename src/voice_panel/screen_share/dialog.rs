//! Source picker dialog of the screen publication.
//!
//! The state, the gate and the on-air card stay in screen_share.rs; this
//! file only paints the picker and routes its keyboard actions.

use super::*;
use crate::theme;
use voxhold_media::publisher::{Codec, Preset, Resolution};
use voxhold_media::screen_capture::ScreenSource;

impl VoicePanel {
    fn choice(
        &self,
        label: &'static str,
        active: bool,
        focus: FocusHandle,
        key: ShareKey,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.choice_enabled(label, active, true, focus, key, cx)
    }

    fn choice_enabled(
        &self,
        label: &'static str,
        active: bool,
        enabled: bool,
        focus: FocusHandle,
        key: ShareKey,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let mut button = div()
            .id(label)
            .h(px(30.))
            .px(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(if active { c.accent_text } else { c.line }))
            .bg(rgb(if active { c.danger_bg } else { c.surface }))
            .text_color(rgb(if !enabled {
                c.line
            } else if active {
                c.fg
            } else {
                c.muted
            }))
            .text_size(px(12.))
            .flex()
            .items_center()
            .justify_center()
            .track_focus(&focus);
        if enabled {
            button = button
                .cursor_pointer()
                .on_click(cx.listener(move |panel, _, _, cx| panel.activate_share(key, cx)));
        }
        button.child(label).into_any_element()
    }

    fn source_row(
        &self,
        source: &ScreenSource,
        index: usize,
        selected: bool,
        enabled: bool,
        focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let description = source.describe();
        let clicked = source.clone();
        let mut row = div()
            .id(("share-source", index))
            .h(px(40.))
            .w_full()
            .px(px(10.))
            .rounded(px(9.))
            .border_1()
            .border_color(rgb(if selected { c.accent_text } else { c.line }))
            .bg(rgb(if selected { c.avatar_bg } else { c.surface }))
            .flex()
            .items_center()
            .gap_2()
            .track_focus(&focus);
        if enabled {
            row =
                row.cursor_pointer()
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        panel.select_screen_source(clicked.clone(), cx)
                    }));
        }
        row.child(img("icons/monitor.svg").w(px(16.)).h(px(16.)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(12.))
                    .truncate()
                    .child(description),
            )
            .when(selected, |row| {
                row.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(c.accent_text))
                        .child("ВЫБРАНО"),
                )
            })
            .into_any_element()
    }

    /// The full-window picker. `None` when it is closed.
    pub fn screen_share_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.screen.open {
            return None;
        }
        let c = self.colors();
        let area = self.player_area;
        if area.is_empty() {
            return None;
        }
        let width = 620.0_f32.min((area.w - 32.0).max(320.0));
        let height = 660.0_f32.min((area.h - 32.0).max(320.0));
        let left = area.x + (area.w - width) / 2.0;
        let top = area.y + (area.h - height) / 2.0;

        let mut ring: Vec<(FocusHandle, ShareKey)> = Vec::new();
        let row = |panel: &Self, ring: &mut Vec<(FocusHandle, ShareKey)>, key: ShareKey| {
            let handle = panel.share_focus(ring.len());
            ring.push((handle.clone(), key));
            handle
        };

        // Tabs and refresh.
        let monitor_focus = row(self, &mut ring, ShareKey::Monitors);
        let window_focus = row(self, &mut ring, ShareKey::Windows);
        let refresh_focus = row(self, &mut ring, ShareKey::Refresh);
        let mut tabs = div().flex().items_center().gap_2();
        tabs = tabs
            .child(self.choice(
                "Мониторы",
                self.screen.kind == SourceKind::Monitor,
                monitor_focus,
                ShareKey::Monitors,
                cx,
            ))
            .child(self.choice(
                "Окна",
                self.screen.kind == SourceKind::Window,
                window_focus,
                ShareKey::Windows,
                cx,
            ))
            .child(div().flex_1())
            .child(self.choice("Обновить", false, refresh_focus, ShareKey::Refresh, cx));

        let sources: Vec<ScreenSource> = self.screen.sources().to_vec();
        let selected = self.screen.selected.clone();
        let mut list = div()
            .id("share-sources")
            .h(px(236.))
            .w_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2();
        if sources.is_empty() {
            let text = if self.screen.loading() {
                "Ищем источники…"
            } else if !self.screen.error().is_empty() {
                self.screen.error()
            } else {
                "Источников нет. Обновите список."
            };
            list = list.child(
                div()
                    .p(px(10.))
                    .text_size(px(12.))
                    .text_color(rgb(c.muted))
                    .child(text.to_owned()),
            );
        }
        for (index, source) in sources.iter().enumerate() {
            let focus = row(self, &mut ring, ShareKey::Source(index));
            let is_selected = selected.as_ref() == Some(source);
            list = list.child(self.source_row(
                source,
                index,
                is_selected,
                !self.screen.restart,
                focus,
                cx,
            ));
        }

        // Quality: only values the SDR publication applies.
        let q = self.screen.quality();
        let codec = Codec::available();
        let bitrate = format!(
            "{},{:01} Мбит/с",
            self.screen.bitrate_kbps / 1000,
            (self.screen.bitrate_kbps % 1000) / 100
        );
        let preset = Preset::detect(q);
        let mut preset_row = div().flex().items_center().gap_2().child(
            div()
                .w(px(92.))
                .text_size(px(12.))
                .text_color(rgb(c.muted))
                .child("Пресеты"),
        );
        for option in Preset::all() {
            let focus = row(self, &mut ring, ShareKey::Preset(option));
            preset_row = preset_row.child(self.choice(
                option.label(),
                preset == option,
                focus,
                ShareKey::Preset(option),
                cx,
            ));
        }

        let res720 = row(self, &mut ring, ShareKey::Resolution(Resolution::P720));
        let res1080 = row(self, &mut ring, ShareKey::Resolution(Resolution::P1080));
        let fps30 = row(self, &mut ring, ShareKey::Fps(30));
        let fps60 = row(self, &mut ring, ShareKey::Fps(60));
        let bitrate_down = row(self, &mut ring, ShareKey::BitrateDown);
        let bitrate_up = row(self, &mut ring, ShareKey::BitrateUp);
        let audio_available = self.screen.audio_available();
        let audio_enabled = self.screen.audio_enabled;
        let audio_controls = audio_enabled && audio_available;
        let audio_down = row(self, &mut ring, ShareKey::AudioBitrateDown);
        let audio_up = row(self, &mut ring, ShareKey::AudioBitrateUp);
        let audio_focus = row(self, &mut ring, ShareKey::Audio);
        let audio_rate = format!("{} кбит/с", self.screen.audio_bitrate_kbps);
        let audio_status = self.screen.audio_status();
        let quality_row = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(92.))
                            .text_size(px(12.))
                            .text_color(rgb(c.muted))
                            .child("Разрешение"),
                    )
                    .child(self.choice(
                        Resolution::P720.label(),
                        q.resolution == Resolution::P720,
                        res720,
                        ShareKey::Resolution(Resolution::P720),
                        cx,
                    ))
                    .child(self.choice(
                        Resolution::P1080.label(),
                        q.resolution == Resolution::P1080,
                        res1080,
                        ShareKey::Resolution(Resolution::P1080),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(92.))
                            .text_size(px(12.))
                            .text_color(rgb(c.muted))
                            .child("Кадры"),
                    )
                    .child(self.choice("30 FPS", q.fps == 30, fps30, ShareKey::Fps(30), cx))
                    .child(self.choice("60 FPS", q.fps == 60, fps60, ShareKey::Fps(60), cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(92.))
                            .text_size(px(12.))
                            .text_color(rgb(c.muted))
                            .child("Битрейт"),
                    )
                    .child(self.choice("− 0,1", false, bitrate_down, ShareKey::BitrateDown, cx))
                    .child(
                        div()
                            .h(px(30.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .child(bitrate),
                    )
                    .child(self.choice("+ 0,1", false, bitrate_up, ShareKey::BitrateUp, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(92.))
                            .text_size(px(12.))
                            .text_color(rgb(c.muted))
                            .child("Аудио"),
                    )
                    .child(self.choice_enabled(
                        "− 64",
                        false,
                        audio_controls,
                        audio_down,
                        ShareKey::AudioBitrateDown,
                        cx,
                    ))
                    .child(
                        div()
                            .h(px(30.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .child(audio_rate),
                    )
                    .child(self.choice_enabled(
                        "+ 64",
                        false,
                        audio_controls,
                        audio_up,
                        ShareKey::AudioBitrateUp,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(92.))
                            .text_size(px(12.))
                            .text_color(rgb(c.muted))
                            .child("Системный звук"),
                    )
                    .child(self.choice_enabled(
                        if audio_enabled {
                            "Включён"
                        } else {
                            "Выключен"
                        },
                        audio_enabled,
                        audio_available,
                        audio_focus,
                        ShareKey::Audio,
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(10.))
                            .text_color(rgb(c.muted))
                            .truncate()
                            .child(audio_status),
                    ),
            )
            .when(audio_enabled, |rows| {
                rows.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(c.muted))
                        .child(SYSTEM_AUDIO_WARNING),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(92.))
                            .text_size(px(12.))
                            .text_color(rgb(c.muted))
                            .child("Кодек"),
                    )
                    .child(
                        div().text_size(px(12.)).child(
                            codec
                                .first()
                                .map(|codec| codec.label())
                                .unwrap_or("нет подтверждённого"),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(c.muted))
                            .child("SDR · серверная передача"),
                    ),
            );

        let cancel_focus = row(self, &mut ring, ShareKey::Cancel);
        let start_focus = row(self, &mut ring, ShareKey::Start);
        let can_start = self.screen.selected.is_some()
            && !self.screen.busy
            && !codec.is_empty()
            && (!self.screen.restart || self.screen.sharing.phase == Phase::Live);
        let start_label = if self.screen.restart {
            "Перезапустить"
        } else if self.screen.busy {
            "Запускаем…"
        } else {
            "Начать трансляцию"
        };
        let error = if self.screen.error.is_empty() {
            None
        } else {
            Some(self.screen.error.clone())
        };
        let actions = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(
                div()
                    .id("share-cancel")
                    .h(px(34.))
                    .px(px(14.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(rgb(c.line))
                    .text_size(px(12.))
                    .text_color(rgb(if self.screen.busy { c.muted } else { c.fg }))
                    .flex()
                    .items_center()
                    .track_focus(&cancel_focus)
                    // Once Start was pressed the attempt is committed: the
                    // explicit Cancel button is unavailable, while Escape and
                    // the backdrop turn into one stop intent.
                    .when(!self.screen.busy, |button| {
                        button
                            .cursor_pointer()
                            .on_click(cx.listener(|panel, _, _, cx| panel.close_screen_share(cx)))
                    })
                    .child("Отмена"),
            )
            .child(
                div()
                    .id("share-start")
                    .h(px(34.))
                    .px(px(14.))
                    .rounded(px(9.))
                    .bg(rgb(if can_start { c.accent } else { c.avatar_bg }))
                    .text_color(rgb(if can_start { c.ink } else { c.muted }))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .flex()
                    .items_center()
                    .when(can_start, |button| button.cursor_pointer())
                    .track_focus(&start_focus)
                    .on_click(cx.listener(|panel, _, _, cx| panel.start_screen_share(cx)))
                    .child(start_label),
            );

        self.screen.ring = ring;
        // The modal must own the keyboard from its first frame; otherwise the
        // user has to click before Tab works. Focus is applied after the frame
        // so the handle is part of the rendered focus path; only the first
        // paint does this, so a later repaint never steals focus.
        if !self.screen.focus_placed {
            if let Some((handle, _)) = self.screen.ring.first() {
                let handle = handle.clone();
                window.defer(cx, move |window, _| window.focus(&handle));
                self.screen.focus_placed = true;
            }
        }
        let mut card = div()
            .id("share-card")
            .debug_selector(|| "screen-share-card".into())
            .key_context("ScreenPicker")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .h(px(height))
            .occlude()
            .rounded(px(20.))
            .border_1()
            .border_color(rgb(c.line))
            .bg(rgb(theme::workspace_color(c.light, 0x1e1e1b)))
            .text_color(rgb(c.fg))
            .flex()
            .flex_col()
            .p(px(18.))
            .gap_3()
            .capture_key_down(
                cx.listener(|panel, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        panel.close_screen_share(cx);
                        cx.stop_propagation();
                        return;
                    }
                    if event.keystroke.key == "tab" {
                        let shift = event.keystroke.modifiers.shift;
                        panel.cycle_share_focus(shift, window, cx);
                        cx.stop_propagation();
                        return;
                    }
                    if event.keystroke.key == "enter" || event.keystroke.key == "space" {
                        panel.activate_focused_share(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Поделиться экраном"),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(c.muted))
                            .child("ТРАНСЛЯЦИЯ ЭКРАНА"),
                    ),
            )
            .child(tabs)
            .child(list)
            .child(preset_row)
            .child(quality_row);
        if let Some(warning) = self.screen.restart_warning() {
            card = card.child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(c.danger))
                    .child(warning),
            );
        }
        if let Some(error) = error {
            card = card.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(c.danger))
                    .child(error),
            );
        }
        card = card.child(actions);

        Some(
            div()
                .id("share-backdrop")
                .absolute()
                .left(px(area.x))
                .top(px(area.y))
                .w(px(area.w))
                .h(px(area.h))
                .bg(gpui::rgba(0x000000b0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|panel, _, _, cx| panel.close_screen_share(cx)),
                )
                .child(card)
                .into_any_element(),
        )
    }
    fn activate_share(&mut self, key: ShareKey, cx: &mut Context<Self>) {
        match key {
            ShareKey::Monitors => {
                self.screen.kind = SourceKind::Monitor;
                cx.notify();
            }
            ShareKey::Windows => {
                self.screen.kind = SourceKind::Window;
                cx.notify();
            }
            ShareKey::Refresh => self.refresh_screen_sources(cx),
            ShareKey::Resolution(resolution) => {
                self.screen.resolution = resolution;
                cx.notify();
            }
            ShareKey::Fps(fps) => {
                self.screen.fps = fps;
                cx.notify();
            }
            ShareKey::BitrateDown => self.change_bitrate(-100, cx),
            ShareKey::BitrateUp => self.change_bitrate(100, cx),
            ShareKey::Preset(preset) => self.apply_preset(preset, cx),
            ShareKey::Audio => self.toggle_system_audio(cx),
            ShareKey::AudioBitrateDown => self.change_audio_bitrate(false, cx),
            ShareKey::AudioBitrateUp => self.change_audio_bitrate(true, cx),
            ShareKey::Cancel => {
                // The committed attempt is stopped by Escape/backdrop; the
                // Cancel button itself is unavailable while busy.
                if !self.screen.busy {
                    self.close_screen_share(cx);
                }
            }
            ShareKey::Start => self.start_screen_share(cx),
            ShareKey::Source(index) => {
                // The click handler carries the source itself; the keyboard
                // path selects by position so Enter works on a focused row.
                if let Some(source) = self.screen.sources().get(index).cloned() {
                    self.select_screen_source(source, cx);
                }
            }
        }
    }

    fn cycle_share_focus(&mut self, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen.ring.is_empty() {
            return;
        }
        let current = self
            .screen
            .ring
            .iter()
            .position(|(handle, _)| handle.is_focused(window));
        let next = match (current, shift) {
            (Some(index), false) => (index + 1) % self.screen.ring.len(),
            (Some(index), true) => (index + self.screen.ring.len() - 1) % self.screen.ring.len(),
            (None, false) => 0,
            (None, true) => self.screen.ring.len() - 1,
        };
        let handle = self.screen.ring[next].0.clone();
        window.focus(&handle);
        cx.notify();
    }

    fn activate_focused_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self
            .screen
            .ring
            .iter()
            .find(|(handle, _)| handle.is_focused(window))
            .map(|(_, key)| *key);
        if let Some(key) = key {
            self.activate_share(key, cx);
        }
    }

    /// Keyboard routing for the open picker. The root shell calls this before
    /// its own Escape handling, so a committed attempt becomes a stop instead
    /// of the dialog silently closing behind a live publication.
    pub(crate) fn screen_share_key(
        &mut self,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match key {
            "escape" => {
                self.close_screen_share(cx);
                true
            }
            "tab" => {
                self.cycle_share_focus(shift, window, cx);
                true
            }
            "enter" | "space" => {
                self.activate_focused_share(window, cx);
                true
            }
            _ => false,
        }
    }
}
