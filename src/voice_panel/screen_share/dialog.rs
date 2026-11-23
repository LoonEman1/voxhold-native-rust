//! Source picker dialog of the screen publication.
//!
//! The state, the gate and the on-air card stay in screen_share.rs; this
//! file only paints the picker and routes its keyboard actions.

use super::picker::*;
use super::*;
use crate::theme;
use voxhold_media::publisher::{Preset, Resolution, SCREEN_FRAME_RATES};
use voxhold_media::screen_capture::ScreenSource;

impl ScreenShareUi {
    pub(in crate::voice_panel) fn quality_for_source(&self, source: &ScreenSource) -> Quality {
        let mut quality = self.quality().clamped();
        if !self.restart {
            quality.resolution = quality
                .resolution
                .for_source(Some((source.width, source.height)));
        }
        quality
    }

    fn resolution_source(&self) -> Option<(u32, u32)> {
        let source = if self.restart {
            self.restart_source()
        } else {
            self.selected.and_then(|id| self.source_for(id))
        };
        source.map(|source| (source.width, source.height))
    }

    fn offers_resolution(&self, resolution: Resolution) -> bool {
        resolution.available_for(self.resolution_source())
            || (self.restart
                && self
                    .sharing
                    .confirmed
                    .as_ref()
                    .is_some_and(|live| live.quality.resolution == resolution))
    }

    /// Bind the restart dialog to the attempt that is already live.
    ///
    /// The capture engine accepted this identity, so it is known available
    /// until the engine itself reports a failure: this is the one exception
    /// to the "must be in the current snapshot" rule that guards a normal
    /// Start. The identity lives only in `sharing.confirmed`; it is never
    /// inserted into the enumeration caches, so a later normal picker cannot
    /// select it.
    pub(super) fn bind_confirmed_live_source(&mut self) {
        self.selected = None;
        self.selected_identity = None;
        let Some(live) = self
            .sharing
            .confirmed
            .as_ref()
            .map(|confirmed| confirmed.source.clone())
        else {
            return;
        };
        self.kind = live.kind;
        self.selected = Some(live.source_id());
        self.selected_identity = live.identity();
        if self.error == SOURCE_GONE_REASON {
            self.error.clear();
        }
    }
}

impl VoicePanel {
    fn choice(
        &self,
        label: impl Into<gpui::SharedString>,
        active: bool,
        focus: FocusHandle,
        key: ShareKey,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.choice_enabled(label, active, true, 30., focus, key, cx)
    }

    pub(super) fn choice_enabled(
        &self,
        label: impl Into<gpui::SharedString>,
        active: bool,
        enabled: bool,
        height: f32,
        focus: FocusHandle,
        key: ShareKey,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let label = label.into();
        #[cfg(test)]
        let selector = format!("share-option-{label}");
        let button = div().id(label.clone());
        #[cfg(test)]
        let button = button.debug_selector(move || selector);
        let mut button = button
            .h(px(height))
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
            button = button.cursor_pointer().on_click(
                cx.listener(move |panel, _, window, cx| panel.activate_share(key, window, cx)),
            );
        }
        button.child(label).into_any_element()
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
        let mut control_index = 0;
        let mut row = |panel: &Self, ring: &mut Vec<(FocusHandle, ShareKey)>, key: ShareKey| {
            let handle = panel.share_focus(control_index);
            control_index += 1;
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

        // Columns follow the measured inner width minus the scrollbar reserve:
        // GPUI only reserves layout space for a scrollbar, the bar itself is
        // painted below, so the cards must not extend under it. The modal
        // border is part of its box and is subtracted as well.
        let inner_width = (width - PICKER_PADDING * 2. - CARD_BORDER * 2.).max(120.0);
        let content_width = (inner_width - LIST_SCROLLBAR).max(1.0);
        let columns = picker_columns(content_width);
        let card_width = card_width(columns, content_width);
        // The physical card width decides the thumbnail request. Moving the
        // picker to a 125/150% monitor starts a new preview generation.
        let requested = thumbnail_request_width(card_width, window.scale_factor());
        self.set_thumbnail_width(requested, window, cx);

        let restart_live = self.screen.restart_source().cloned();
        let sources: Vec<ScreenSource> = if self.screen.restart {
            restart_live.iter().cloned().collect()
        } else {
            self.screen.sources().to_vec()
        };
        // The rows of the last painted grid decide which thumbnails the batch
        // claims and may evict. A restart card or an empty category has no
        // rows to measure, so the next picker starts from the bootstrap set.
        let mut rows_changed = false;
        if sources.is_empty() {
            self.screen.visible_rows = None;
            self.screen.preview_columns = columns;
        } else {
            let top = self.screen.list_scroll.top_item();
            let bottom = self.screen.list_scroll.bottom_item();
            rows_changed = self
                .screen
                .set_visible_rows(columns, top.min(bottom), bottom);
        }

        // The second row on the base screenshot was clipped by the list
        // viewport, which was fixed at 236 px; the cards themselves were
        // complete but small. The viewport height is now derived from the
        // dialog, rows past it scroll inside this separate element, and the
        // bar is painted from the real scroll state.
        let empty = sources.is_empty();
        let mut list = div()
            .id("share-sources")
            .debug_selector(|| "share-sources".into())
            .w_full()
            .flex_shrink_0()
            .relative();
        if empty {
            // An empty grid must not reserve the height of a row it does not
            // have. The message is its own small block.
            let text = if self.screen.restart {
                "Текущий источник недоступен.".to_owned()
            } else if self.screen.loading() {
                "Ищем источники…".to_owned()
            } else if !self.screen.error().is_empty() {
                self.screen.error().to_owned()
            } else {
                "Источников нет. Обновите список.".to_owned()
            };
            self.screen.preview_cards = 0;
            list = list.child(
                div()
                    .debug_selector(|| "share-sources-empty".into())
                    .p(px(10.))
                    .text_size(px(12.))
                    .text_color(rgb(c.muted))
                    .child(text),
            );
        } else {
            self.screen.preview_cards = sources.len();
            let viewport_height = list_height(height);
            let mut grid = div()
                .id("share-sources-scroll")
                .w_full()
                .h_full()
                .flex()
                .flex_col()
                .gap(px(PICKER_GAP))
                .overflow_y_scroll()
                .scrollbar_width(px(LIST_SCROLLBAR))
                .track_scroll(&self.screen.list_scroll.clone());
            let selected = self.screen.selected;
            let mut rows: Vec<Vec<gpui::AnyElement>> = Vec::new();
            let mut current: Vec<gpui::AnyElement> = Vec::new();
            for source in sources.iter() {
                let id = source.source_id();
                // Locked context: only the confirmed live source is painted,
                // and it is inert. A restart is not part of the focus ring, so
                // neither Tab nor a click can move it onto another cached
                // source.
                let (focus, is_selected, enabled, locked) = if self.screen.restart {
                    (None, true, false, true)
                } else {
                    // Focus belongs to the surface, not to the grid position
                    // or the capturer number: the keyed handle survives a
                    // reorder.
                    let focus = window
                        .use_keyed_state(Self::source_element_id(source), cx, |_, cx| {
                            cx.focus_handle()
                        })
                        .read(cx)
                        .clone();
                    ring.push((focus.clone(), ShareKey::Source(id)));
                    (Some(focus), selected == Some(id), true, false)
                };
                current.push(self.source_card(
                    source,
                    id,
                    is_selected,
                    enabled,
                    focus,
                    card_width,
                    locked,
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
                let mut grid_row = div().flex().items_start().gap(px(PICKER_GAP)).w_full();
                for card in cards {
                    grid_row = grid_row.child(card);
                }
                grid = grid.child(grid_row);
            }
            list = list.h(px(viewport_height)).child(grid);
            // The reserved 8 px is only layout space in GPUI. Paint the
            // visible bar over the right edge of the viewport from the same
            // handle that scrolls the grid: the thumb follows `offset()` and
            // disappears when there is nothing to scroll.
            let max_scroll: f32 = self.screen.list_scroll.max_offset().height.into();
            if max_scroll > 0.5 {
                let offset: f32 = (-self.screen.list_scroll.offset().y).into();
                let content = viewport_height + max_scroll;
                let thumb_h = (viewport_height * viewport_height / content)
                    .max(28.)
                    .min(viewport_height);
                let progress = (offset / max_scroll).clamp(0., 1.);
                let thumb_top = (viewport_height - thumb_h) * progress;
                list = list.child(
                    div()
                        .debug_selector(|| "share-sources-scrollbar".into())
                        .absolute()
                        .top(px(0.))
                        .right(px(0.))
                        .w(px(LIST_SCROLLBAR))
                        .h(px(viewport_height))
                        .child(
                            div()
                                .debug_selector(|| "share-sources-scrollbar-thumb".into())
                                .absolute()
                                .right(px(1.))
                                .top(px(thumb_top))
                                .w(px(LIST_SCROLLBAR - 2.))
                                .h(px(thumb_h))
                                .rounded(px(3.))
                                .bg(rgb(c.muted)),
                        ),
                );
            }
        }
        // A wheel or scrollbar move happens outside the request points of the
        // picker, so the frame asks once for the rows that just became
        // visible. A plain repaint changes neither the rows nor the scroll and
        // asks nothing. The request itself is guarded: a running batch keeps
        // its single capture and picks the new rows up at its next claim.
        if rows_changed {
            #[cfg(test)]
            {
                self.screen.preview_deferrals = self.screen.preview_deferrals.saturating_add(1);
            }
            let entity = cx.entity();
            window.defer(cx, move |window, cx| {
                let _ = entity.update(cx, |panel, cx| {
                    panel.request_screen_previews(window, cx);
                });
            });
        }

        // Quality: only values the SDR publication applies.
        let q = self.screen.quality();
        // Codec: the website's «Автоматически» plus its four SDR codecs. The
        // selector and its reasons live in `screen_share/codec.rs`.
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

        let option_row = |label| {
            div().flex().items_start().gap_2().child(
                div()
                    .w(px(92.))
                    .flex_shrink_0()
                    .pt(px(7.))
                    .text_size(px(12.))
                    .text_color(rgb(c.muted))
                    .child(label),
            )
        };
        let active_resolution = if self.screen.restart {
            q.resolution
        } else {
            q.resolution.for_source(self.screen.resolution_source())
        };
        let mut resolutions = div().flex().flex_wrap().gap_2().min_w_0();
        for resolution in Resolution::OPTIONS {
            if self.screen.offers_resolution(resolution) {
                let key = ShareKey::Resolution(resolution);
                let focus = row(self, &mut ring, key);
                resolutions = resolutions.child(self.choice(
                    resolution.label(),
                    active_resolution == resolution,
                    focus,
                    key,
                    cx,
                ));
            }
        }
        let mut frame_rates = div().flex().flex_wrap().gap_2().min_w_0();
        for fps in SCREEN_FRAME_RATES {
            let key = ShareKey::Fps(fps);
            let focus = row(self, &mut ring, key);
            frame_rates =
                frame_rates.child(self.choice(format!("{fps} FPS"), q.fps == fps, focus, key, cx));
        }
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
            .gap_1()
            .child(option_row("Разрешение").child(resolutions))
            .child(option_row("Кадры").child(frame_rates))
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
                        30.,
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
                        30.,
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
                        30.,
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
                        30.,
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
            .child(self.codec_row(&mut ring, &mut row, cx));

        let cancel_focus = row(self, &mut ring, ShareKey::Cancel);
        let start_focus = row(self, &mut ring, ShareKey::Start);
        let can_start = self.screen_can_start();
        let restart_change = self.screen.restart_change();
        let live_bitrate = restart_change == Some(QualityChange::LiveBitrate);
        let unchanged = restart_change == Some(QualityChange::Unchanged);
        let applying = self
            .screen
            .sharing
            .quality_status
            .as_ref()
            .is_some_and(|status| status.outcome == QualityOutcome::Applying);
        let start_label = if applying {
            "Применяем…"
        } else if self.screen.sharing.preflighting {
            "Проверяем…"
        } else if self.screen.restart {
            if unchanged {
                // Apply would be a verified no-op: the dialog only closes.
                "Готово"
            } else if live_bitrate {
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
            .debug_selector(|| "share-actions".into())
            .flex()
            .flex_shrink_0()
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
                        button.cursor_pointer().on_click(
                            cx.listener(|panel, _, window, cx| {
                                panel.close_screen_share(window, cx)
                            }),
                        )
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
                    .on_click(
                        cx.listener(|panel, _, window, cx| panel.start_screen_share(window, cx)),
                    )
                    .child(start_label),
            );

        // The settings keep their own scroll region: a compact window shrinks
        // this block instead of pushing the Cancel/Start row out of the modal.
        let settings = div()
            .id("share-settings")
            .debug_selector(|| "share-settings".into())
            .w_full()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .scrollbar_width(px(6.))
            .flex()
            .flex_col()
            .gap_2()
            .child(preset_row)
            .child(quality_row);

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
                        panel.close_screen_share(window, cx);
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
            .child(settings);
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
                    cx.listener(|panel, _, window, cx| panel.close_screen_share(window, cx)),
                )
                .child(card)
                .into_any_element(),
        )
    }
    fn activate_share(&mut self, key: ShareKey, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            ShareKey::Monitors => self.change_share_kind(SourceKind::Monitor, window, cx),
            ShareKey::Windows => self.change_share_kind(SourceKind::Window, window, cx),
            ShareKey::Refresh => self.refresh_screen_sources(window, cx),
            ShareKey::Resolution(resolution) => {
                if !self.screen.offers_resolution(resolution) {
                    return;
                }
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
            ShareKey::Codec(intent) => self.select_screen_codec(intent, cx),
            ShareKey::Audio => self.toggle_system_audio(cx),
            ShareKey::AudioScope(scope) => self.select_audio_scope(scope, cx),
            ShareKey::AudioBitrateDown => self.change_audio_bitrate(false, cx),
            ShareKey::AudioBitrateUp => self.change_audio_bitrate(true, cx),
            ShareKey::Cancel => {
                // The committed attempt is stopped by Escape/backdrop; the
                // Cancel button itself is unavailable while busy.
                if !self.screen.busy {
                    self.close_screen_share(window, cx);
                }
            }
            ShareKey::Start => self.start_screen_share(window, cx),
            ShareKey::Source(id) => {
                // The row is selected by its stable identity, so Enter on a
                // focused row still selects it after the list is reordered.
                self.select_screen_source(id, cx);
            }
        }
        if !self.screen.restart
            && matches!(
                key,
                ShareKey::Resolution(_)
                    | ShareKey::Fps(_)
                    | ShareKey::Preset(_)
                    | ShareKey::BitrateDown
                    | ShareKey::BitrateUp
                    | ShareKey::Codec(_)
                    | ShareKey::Audio
                    | ShareKey::AudioScope(_)
                    | ShareKey::AudioBitrateDown
                    | ShareKey::AudioBitrateUp
            )
        {
            self.remember_stream_preferences(cx);
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
        let (handle, key) = self.screen.ring[next].clone();
        // Focus must be visible, not merely set: a keyboard walk to the last
        // source scrolls its whole grid row into the separate list viewport.
        if let ShareKey::Source(id) = key {
            if let Some(index) = self
                .screen
                .sources()
                .iter()
                .position(|source| source.source_id() == id)
            {
                let columns = self.screen.preview_columns.max(1);
                self.screen.list_scroll.scroll_to_item(index / columns);
            }
        }
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
            self.activate_share(key, window, cx);
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
                self.close_screen_share(window, cx);
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
