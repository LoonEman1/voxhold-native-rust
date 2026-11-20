//! Source picker dialog of the screen publication.
//!
//! The state, the gate and the on-air card stay in screen_share.rs; this
//! file only paints the picker and routes its keyboard actions.

use super::*;
use crate::theme;
use voxhold_media::publisher::{Codec, Preset, Resolution};
use voxhold_media::screen_capture::ScreenSource;

impl ScreenShareUi {
    /// Bind the restart dialog to the attempt that is already live.
    ///
    /// The capture engine accepted this identity, so it is known available
    /// until the engine itself reports a failure: this is the one exception
    /// to the "must be in the current snapshot" rule that guards a normal
    /// Start. The identity lives only in `last_request`; it is never inserted
    /// into the enumeration caches, so a later normal picker cannot select it.
    pub(super) fn bind_confirmed_live_source(&mut self) {
        self.selected = None;
        let Some(live) = self
            .last_request
            .as_ref()
            .map(|request| request.source.source_id())
        else {
            return;
        };
        self.kind = live.kind;
        self.selected = Some(live);
        if self.error == SOURCE_GONE_REASON {
            self.error.clear();
        }
    }
}

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

    /// One picker card. The root id, the click and the focus handle all use the
    /// stable `kind + id`, never a grid position, so a reorder keeps both the
    /// selection and the keyboard focus on the same source.
    fn source_card(
        &self,
        source: &ScreenSource,
        id: SourceId,
        selected: bool,
        enabled: bool,
        focus: Option<FocusHandle>,
        card_width: f32,
        locked: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let key = ScreenShareUi::source_kind_name(id);
        let thumb = self.screen.previews.get(&id);
        let failure = self.screen.preview_failures.get(&id);
        let size = if source.width > 0 && source.height > 0 {
            format!("{}×{}", source.width, source.height)
        } else {
            "Размер неизвестен".into()
        };
        let card_selector = format!("share-source-card-{key}-{}", id.id);
        let thumb_selector = format!("share-thumb-{key}-{}", id.id);
        let placeholder_selector = format!("share-thumb-placeholder-{key}-{}", id.id);
        let error_selector = format!("share-thumb-error-{key}-{}", id.id);
        let selected_selector = format!("share-selected-{key}-{}", id.id);

        let mut picture = div()
            .w_full()
            .h(px(70.))
            .flex_shrink_0()
            .overflow_hidden()
            .bg(rgb(0x080808))
            .flex()
            .items_center()
            .justify_center();
        if let Some(thumb) = thumb {
            picture = picture.child(
                div()
                    .debug_selector(move || thumb_selector.clone())
                    .w_full()
                    .h_full()
                    .child(
                        img(thumb.image.clone())
                            .w_full()
                            .h_full()
                            .object_fit(gpui::ObjectFit::Contain),
                    ),
            );
        } else if failure.is_some() {
            picture = picture.child(
                div()
                    .debug_selector(move || error_selector.clone())
                    .text_size(px(10.))
                    .text_color(rgb(c.danger))
                    .child("Превью недоступно"),
            );
        } else {
            picture = picture.child(
                div()
                    .debug_selector(move || placeholder_selector.clone())
                    .text_size(px(10.))
                    .text_color(rgb(c.muted))
                    .child(if locked {
                        "Текущий источник"
                    } else {
                        "Превью…"
                    }),
            );
        }

        let mut footer = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p(px(6.))
            .min_w(px(0.))
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .truncate()
                    .child(source.title.clone()),
            )
            .child(
                div()
                    .text_size(px(9.))
                    .text_color(rgb(c.muted))
                    .truncate()
                    .child(size),
            );
        if let Some(message) = failure {
            footer = footer.child(
                div()
                    .text_size(px(9.))
                    .text_color(rgb(c.danger))
                    .truncate()
                    .child(message.clone()),
            );
        }

        let mut card = div()
            .id(Self::source_element_id(id))
            .debug_selector(move || card_selector.clone())
            .w(px(card_width))
            .h(px(124.))
            .flex_shrink_0()
            .relative()
            .overflow_hidden()
            .rounded(px(9.))
            .border_1()
            .border_color(rgb(if selected { c.accent_text } else { c.line }))
            .bg(rgb(if selected { c.avatar_bg } else { c.surface }))
            .flex()
            .flex_col();
        if let Some(focus) = focus {
            card = card.track_focus(&focus);
        }
        if enabled {
            card = card
                .cursor_pointer()
                .on_click(cx.listener(move |panel, _, _, cx| panel.select_screen_source(id, cx)));
        }
        if selected {
            card = card.child(
                div()
                    .debug_selector(move || selected_selector.clone())
                    .absolute()
                    .top(px(5.))
                    .right(px(5.))
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(rgb(c.accent))
                    .text_color(rgb(c.ink))
                    .text_size(px(8.))
                    .child("ВЫБРАНО"),
            );
        }
        card.child(picture).child(footer).into_any_element()
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

        // Tabs and refresh. A restart is source-locked: the category and the
        // enumeration are not part of its decision, so they are neither
        // painted nor reachable by keyboard.
        let mut tabs = div().flex().items_center().gap_2();
        if !self.screen.restart {
            let monitor_focus = row(self, &mut ring, ShareKey::Monitors);
            let window_focus = row(self, &mut ring, ShareKey::Windows);
            let refresh_focus = row(self, &mut ring, ShareKey::Refresh);
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
        }

        // Two cards per row at the dialog's normal width, one when it has to
        // shrink; the grid scrolls inside the same picker height as before.
        let columns = if width >= 560.0 { 2 } else { 1 };
        let inner_width = (width - 36.0).max(120.0);
        let gap = 8.0_f32;
        let card_width = if columns == 2 {
            (inner_width - gap) / 2.0
        } else {
            inner_width
        };
        let mut list = div()
            .id("share-sources")
            .debug_selector(|| "share-sources".into())
            .h(px(236.))
            .w_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(gap));
        if self.screen.restart {
            // Locked context: only the confirmed live source is painted, and
            // it is inert. It is not part of the focus ring, so neither Tab
            // nor a click can move the restart onto another cached source.
            if let Some(live) = self.screen.restart_source().cloned() {
                let id = live.source_id();
                self.screen.preview_cards = 1;
                list = list
                    .child(self.source_card(&live, id, true, false, None, card_width, true, cx));
            } else {
                self.screen.preview_cards = 0;
            }
        } else {
            let sources: Vec<ScreenSource> = self.screen.sources().to_vec();
            let selected = self.screen.selected;
            self.screen.preview_cards = sources.len();
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
            let mut rows: Vec<Vec<gpui::AnyElement>> = Vec::new();
            let mut current: Vec<gpui::AnyElement> = Vec::new();
            for source in sources.iter() {
                let id = source.source_id();
                // Focus belongs to the source, not to the grid position: the
                // keyed handle survives a reorder and follows the same id.
                let focus = window
                    .use_keyed_state(Self::source_element_id(id), cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone();
                ring.push((focus.clone(), ShareKey::Source(id)));
                let is_selected = selected == Some(id);
                current.push(self.source_card(
                    source,
                    id,
                    is_selected,
                    true,
                    Some(focus),
                    card_width,
                    false,
                    cx,
                ));
                if current.len() == columns {
                    rows.push(std::mem::take(&mut current));
                }
            }
            if !current.is_empty() {
                rows.push(current);
            }
            for cards in rows {
                let mut grid_row = div().flex().items_start().gap(px(gap));
                for card in cards {
                    grid_row = grid_row.child(card);
                }
                list = list.child(grid_row);
            }
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
        let audio_requested = self.screen.audio_requested();
        let audio_toggle = self.screen.audio_toggle_label();
        let audio_controls = audio_requested;
        let audio_down = row(self, &mut ring, ShareKey::AudioBitrateDown);
        let audio_up = row(self, &mut ring, ShareKey::AudioBitrateUp);
        let audio_focus = row(self, &mut ring, ShareKey::Audio);
        let audio_rate = format!("{} кбит/с", self.screen.audio_bitrate_kbps);
        let audio_status = self.screen.audio_status();
        let scope_sentence = self.screen.audio_scope_sentence();
        let scope_choices = audio_scope::audio_scope_choices(self.screen.kind);
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
                        audio_toggle,
                        audio_requested,
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
            .when(audio_requested, |rows| {
                let mut scope_row = div().flex().items_center().gap_2().child(
                    div()
                        .w(px(92.))
                        .text_size(px(12.))
                        .text_color(rgb(c.muted))
                        .child("Что звучит"),
                );
                for (scope, label) in scope_choices {
                    let focus = row(self, &mut ring, ShareKey::AudioScope(scope));
                    scope_row = scope_row.child(self.choice_enabled(
                        label,
                        self.screen.audio_scope == scope,
                        audio_available,
                        focus,
                        ShareKey::AudioScope(scope),
                        cx,
                    ));
                }
                rows.child(scope_row).child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(c.muted))
                        .child(scope_sentence),
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
        let can_start = self.screen_can_start();
        let live_bitrate = self.screen.restart_change() == Some(QualityChange::LiveBitrate);
        let start_label = if self.screen.sharing.preflighting {
            "Проверяем…"
        } else if self.screen.restart {
            if live_bitrate {
                "Применить"
            } else {
                "Перезапустить"
            }
        } else if self.screen.busy {
            "Запускаем…"
        } else {
            "Начать трансляцию"
        };
        // A restart dialog may be closed while its preflight runs: that
        // cancels the preflight and leaves the live stream alone.
        let cancel_active =
            !self.screen.busy || (self.screen.restart && !self.screen.sharing.restarting);
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
                    .text_color(rgb(if self.screen.busy && !cancel_active {
                        c.muted
                    } else {
                        c.fg
                    }))
                    .flex()
                    .items_center()
                    .track_focus(&cancel_focus)
                    // Once Start was pressed the attempt is committed: the
                    // explicit Cancel button is unavailable, while Escape and
                    // the backdrop turn into one stop intent. A restart
                    // preflight is the exception: closing it only cancels the
                    // preflight and leaves the live stream running.
                    .when(cancel_active, |button| {
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
            ShareKey::Monitors => self.change_share_kind(SourceKind::Monitor, cx),
            ShareKey::Windows => self.change_share_kind(SourceKind::Window, cx),
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
            ShareKey::AudioScope(scope) => self.select_audio_scope(scope, cx),
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
            ShareKey::Source(id) => {
                // The row is selected by its stable identity, so Enter on a
                // focused row still selects it after the list is reordered.
                self.select_screen_source(id, cx);
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
