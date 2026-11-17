//! The shared settings modal frame and the personal-settings tabs.

use super::*;
use gpui::{Div, ElementId, Focusable, Stateful};
use gpui_component::input::TextInput;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ButtonKind {
    Primary,
    Secondary,
    Danger,
    Tab,
    Icon,
}

impl Voxhold {
    pub(super) fn focus_id(
        &mut self,
        id: ElementId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        window
            .use_keyed_state(id, cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone()
    }

    pub(super) fn settings_button(
        &mut self,
        id: ElementId,
        label: impl Into<String>,
        kind: ButtonKind,
        selected: bool,
        disabled: bool,
        action: SettingsAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = self.palette;
        let focus = self.focus_id(id.clone(), window, cx);
        let focused = focus.is_focused(window);
        if !disabled {
            self.settings_ring
                .push((focus.clone(), Some(action.clone())));
        }
        let mut button = div()
            .id(id)
            .h(px(40.))
            .min_w(px(0.))
            .px(px(14.))
            .rounded(px(9.))
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .text_size(px(13.))
            .font_weight(gpui::FontWeight::BOLD)
            .when(kind == ButtonKind::Icon, |button| {
                button.w(px(34.)).h(px(34.)).px(px(0.))
            })
            .when(kind == ButtonKind::Primary, |button| {
                button
                    .bg(ink(if disabled { p.surface } else { p.lime }))
                    .text_color(ink(if disabled { p.muted } else { 0x15150f }))
            })
            .when(kind == ButtonKind::Secondary, |button| {
                button
                    .bg(ink(p.surface))
                    .border_1()
                    .border_color(ink(if focused { p.lime } else { p.line }))
            })
            .when(kind == ButtonKind::Danger, |button| {
                button
                    .bg(super::ui::ink_alpha(p.danger, 15))
                    .border_1()
                    .border_color(super::ui::ink_alpha(p.danger, 110))
                    .text_color(ink(p.danger))
            })
            .when(kind == ButtonKind::Tab, |button| {
                button
                    .bg(ink(if selected { p.surface } else { p.bg }))
                    .text_color(ink(if selected { p.fg } else { p.muted }))
                    .border_1()
                    .border_color(ink(if focused {
                        p.lime
                    } else if selected {
                        p.line
                    } else {
                        p.bg
                    }))
            })
            .when(kind == ButtonKind::Icon, |button| {
                button.text_color(ink(p.muted))
            })
            .when(disabled, |button| button.opacity(0.55))
            .when(!disabled, |button| {
                button
                    .cursor_pointer()
                    .track_focus(&focus)
                    .tab_index(0)
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.settings_action(action.clone(), window, cx)
                    }))
            });
        if kind == ButtonKind::Icon {
            button = button.child(icon("icons/close.svg", 16., ink(self.palette.muted)));
        }
        if kind != ButtonKind::Icon {
            button = button.child(label.into());
        }
        button
    }

    pub(super) fn settings_toggle(
        &mut self,
        id: ElementId,
        title: &str,
        description: &str,
        on: bool,
        disabled: bool,
        action: SettingsAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let focus = self.focus_id(id.clone(), window, cx);
        let focused = focus.is_focused(window);
        if !disabled {
            self.settings_ring
                .push((focus.clone(), Some(action.clone())));
        }
        let track = div()
            .w(px(43.))
            .h(px(21.))
            .rounded(px(11.))
            .bg(ink(if on { p.lime } else { p.selection }))
            .relative()
            .child(
                div()
                    .absolute()
                    .top(px(3.))
                    .left(px(if on { 24. } else { 3. }))
                    .w(px(15.))
                    .h(px(15.))
                    .rounded_full()
                    .bg(ink(if on { 0x15150f } else { p.muted })),
            );
        let _ = focused;
        div()
            .id(id)
            .w_full()
            .py(px(6.))
            .flex()
            .flex_col()
            .gap_1()
            .when(!disabled, |row| {
                row.cursor_pointer()
                    .track_focus(&focus)
                    .tab_index(0)
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.settings_action(action.clone(), window, cx)
                    }))
            })
            .when(disabled, |row| row.opacity(0.55))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(13.))
                            .child(title.to_owned()),
                    )
                    .child(track),
            )
            .when(!description.is_empty(), |row| {
                row.child(
                    div()
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .child(description.to_owned()),
                )
            })
            .into_any_element()
    }

    pub(super) fn settings_option(
        &mut self,
        id: ElementId,
        title: &str,
        description: &str,
        selected: bool,
        disabled: bool,
        action: SettingsAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let focus = self.focus_id(id.clone(), window, cx);
        if !disabled {
            self.settings_ring
                .push((focus.clone(), Some(action.clone())));
        }
        div()
            .id(id)
            .w_full()
            .py(px(8.))
            .px(px(2.))
            .rounded(px(8.))
            .flex()
            .items_start()
            .gap_3()
            .when(!disabled, |row| {
                row.cursor_pointer()
                    .track_focus(&focus)
                    .tab_index(0)
                    .hover(|row| row.bg(ink(p.selection)))
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.settings_action(action.clone(), window, cx)
                    }))
            })
            .child(
                div()
                    .w(px(18.))
                    .h(px(18.))
                    .mt(px(1.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(ink(if selected { p.lime } else { p.line }))
                    .bg(ink(if selected { p.lime } else { p.bg }))
                    .when(selected, |dot| {
                        dot.child(icon("icons/check.svg", 12., ink(0x15150f)))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_size(px(13.)).child(title.to_owned()))
                    .when(!description.is_empty(), |text| {
                        text.child(
                            div()
                                .text_size(px(12.))
                                .text_color(ink(p.muted))
                                .child(description.to_owned()),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn settings_value_slider(
        &mut self,
        id: &'static str,
        label: &str,
        value: i64,
        max: i64,
        step: i64,
        action: impl Fn(i64) -> SettingsAct + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let ratio = (value.clamp(0, max) as f32 / max.max(1) as f32).clamp(0., 1.);
        let lower = action((value - step).clamp(0, max));
        let upper = action((value + step).clamp(0, max));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("{label} · {value}")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.settings_button(
                        ElementId::Name(format!("{id}-down").into()),
                        "−",
                        ButtonKind::Secondary,
                        false,
                        value <= 0,
                        lower,
                        window,
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .h(px(6.))
                            .rounded(px(3.))
                            .bg(ink(p.selection))
                            .relative()
                            .child(
                                div()
                                    .absolute()
                                    .left(px(0.))
                                    .top(px(0.))
                                    .h(px(6.))
                                    .w(px(220. * ratio))
                                    .rounded(px(3.))
                                    .bg(ink(p.lime)),
                            ),
                    )
                    .child(self.settings_button(
                        ElementId::Name(format!("{id}-up").into()),
                        "+",
                        ButtonKind::Secondary,
                        false,
                        value >= max,
                        upper,
                        window,
                        cx,
                    )),
            )
            .into_any_element()
    }

    pub(super) fn settings_input(
        &mut self,
        state: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = state.read(cx).focus_handle(cx);
        self.settings_ring.push((focus, None));
        TextInput::new(state)
            .font_family("Inter")
            .text_size(px(14.))
            .into_any_element()
    }

    pub(super) fn heading(&self, title: &str, description: &str) -> AnyElement {
        let p = self.palette;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title.to_owned()),
            )
            .when(!description.is_empty(), |heading| {
                heading.child(
                    div()
                        .text_size(px(13.))
                        .text_color(ink(p.muted))
                        .child(description.to_owned()),
                )
            })
            .into_any_element()
    }

    pub(super) fn separator(&self) -> AnyElement {
        div()
            .h(px(1.))
            .my(px(9.))
            .bg(ink(self.palette.line))
            .into_any_element()
    }

    pub(super) fn form_text(&self, text: &str, danger: bool) -> AnyElement {
        let p = self.palette;
        div()
            .text_size(px(13.))
            .text_color(ink(if danger { p.danger } else { p.muted }))
            .child(text.to_owned())
            .into_any_element()
    }

    pub(super) fn settings_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let kind = self.active_settings?;
        let (title, tabs, active, pending, error) = match kind {
            SettingsKind::Personal => {
                let panel = self.personal.as_ref()?;
                (
                    panel.dialog.title.clone(),
                    panel
                        .dialog
                        .tabs
                        .iter()
                        .map(|tab| tab.label)
                        .collect::<Vec<_>>(),
                    panel.dialog.active,
                    panel.dialog.pending,
                    panel.dialog.error.clone(),
                )
            }
            SettingsKind::Server => {
                let panel = self.server_settings.as_ref()?;
                (
                    panel.dialog.title.clone(),
                    panel
                        .dialog
                        .tabs
                        .iter()
                        .map(|tab| tab.label)
                        .collect::<Vec<_>>(),
                    panel.dialog.active,
                    panel.dialog.pending,
                    panel.dialog.error.clone(),
                )
            }
            SettingsKind::Channel => {
                let panel = self.channel_settings.as_ref()?;
                (
                    panel.dialog.title.clone(),
                    panel
                        .dialog
                        .tabs
                        .iter()
                        .map(|tab| tab.label)
                        .collect::<Vec<_>>(),
                    panel.dialog.active,
                    panel.dialog.pending,
                    panel.dialog.error.clone(),
                )
            }
            SettingsKind::ChannelNotifications => {
                let panel = self.channel_notifications.as_ref()?;
                (
                    panel.dialog.title.clone(),
                    panel
                        .dialog
                        .tabs
                        .iter()
                        .map(|tab| tab.label)
                        .collect::<Vec<_>>(),
                    panel.dialog.active,
                    panel.dialog.pending,
                    panel.error.clone(),
                )
            }
        };
        let p = self.palette;
        let (vw, vh) = self.viewport;
        let width = 680_f32.min(vw - 16.).max(260.);
        let height = 780_f32.min(vh - 24.).max(240.);
        let pad = if width < 440. { 16. } else { 24. };
        let left = (vw - width) / 2.;
        let top = (vh - height) / 2.;
        self.settings_ring.clear();

        let header = div()
            .h(px(67.))
            .px(px(pad))
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(23.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(pending > 0 && error.is_empty(), |row| {
                        row.child(
                            div()
                                .text_size(px(12.))
                                .text_color(ink(p.muted))
                                .child("Загрузка…"),
                        )
                    })
                    .child(self.settings_button(
                        ElementId::Name("settings-close".into()),
                        "",
                        ButtonKind::Icon,
                        false,
                        false,
                        SettingsAct::Close,
                        window,
                        cx,
                    )),
            );
        let tab_strip = if tabs.len() > 1 {
            let gap = 5.;
            let cell = (width - 2. * pad - gap * (tabs.len() - 1) as f32) / tabs.len() as f32;
            let mut strip = div()
                .h(px(56.))
                .px(px(pad))
                .flex()
                .items_start()
                .gap(px(gap));
            for (index, label) in tabs.iter().enumerate() {
                let button = self.settings_button(
                    ElementId::Name(format!("settings-tab-{index}").into()),
                    *label,
                    ButtonKind::Tab,
                    index == active,
                    false,
                    SettingsAct::Tab(index),
                    window,
                    cx,
                );
                strip = strip.child(div().w(px(cell)).h(px(42.)).child(button.w_full()));
            }
            strip
        } else {
            div()
        };
        let error_banner = if error.is_empty() {
            None
        } else {
            Some(
                div()
                    .mx(px(pad))
                    .mb(px(8.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .bg(super::ui::ink_alpha(p.danger, 30))
                    .text_size(px(13.))
                    .text_color(ink(p.danger))
                    .child(error),
            )
        };
        let body = match kind {
            SettingsKind::Personal => self.personal_body(window, cx),
            SettingsKind::Server => self.server_body(window, cx),
            SettingsKind::Channel => self.channel_body(window, cx),
            SettingsKind::ChannelNotifications => self.notifications_body(window, cx),
        };
        let scroll = self.settings_scroll.clone();
        let ring = self.settings_ring.clone();
        let close = self.settings_close_focus.clone();
        let mut card = div()
            .id("settings-card")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .h(px(height))
            .occlude()
            .rounded(px(22.))
            .border_1()
            .border_color(ink(p.line))
            .bg(ink(if p.light { 0xffffff } else { 0x1e1e1b }))
            .text_color(ink(p.fg))
            .flex()
            .flex_col()
            .capture_key_down(
                cx.listener(move |s, event: &gpui::KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    if key == "escape" {
                        s.close_settings(true, window, cx);
                        cx.stop_propagation();
                        return;
                    }
                    if key == "tab" {
                        if s.settings_ring.is_empty() {
                            let _ = &close;
                            window.blur();
                        } else {
                            let current = s
                                .settings_ring
                                .iter()
                                .position(|(handle, _)| handle.is_focused(window));
                            let next = match (current, event.keystroke.modifiers.shift) {
                                (Some(index), false) => (index + 1) % s.settings_ring.len(),
                                (Some(index), true) => {
                                    (index + s.settings_ring.len() - 1) % s.settings_ring.len()
                                }
                                (None, false) => 0,
                                (None, true) => s.settings_ring.len() - 1,
                            };
                            let handle = s.settings_ring[next].0.clone();
                            window.focus(&handle);
                        }
                        cx.stop_propagation();
                        return;
                    }
                    if (key == "enter" || key == "space") && !event.keystroke.modifiers.control {
                        let action = s
                            .settings_ring
                            .iter()
                            .find(|(handle, action)| action.is_some() && handle.is_focused(window))
                            .and_then(|(_, action)| action.clone());
                        if let Some(action) = action {
                            s.settings_action(action, window, cx);
                            cx.stop_propagation();
                        }
                    }
                }),
            )
            .child(header)
            .child(tab_strip);
        if let Some(banner) = error_banner {
            card = card.child(banner);
        }
        card = card.child(
            div()
                .id("settings-body")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .px(px(pad))
                .pt(px(16.))
                .pb(px(20.))
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(body),
        );
        let _ = ring;
        Some(
            div()
                .id("settings-backdrop")
                .absolute()
                .size_full()
                .bg(gpui::rgba(0x000000b0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|s, _, window, cx| s.close_settings(true, window, cx)),
                )
                .child(card)
                .into_any_element(),
        )
    }

    fn personal_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.personal.as_ref().map(|panel| panel.tab()).unwrap_or(0);
        match tab {
            1 => self.voice_body(window, cx),
            2 => self.privacy_body(window, cx),
            _ => self.profile_body(window, cx),
        }
    }

    fn privacy_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let Some(panel) = &self.personal else {
            return div().into_any_element();
        };
        let privacy = panel.privacy.clone();
        let notifications = self.notifications.enabled;
        let notification_error = if panel.notifications_error.is_empty() {
            self.notifications.error.clone()
        } else {
            panel.notifications_error.clone()
        };
        let mut root = div().flex().flex_col().gap(px(12.)).child(self.heading(
            "Кто может писать мне",
            "Выберите, от кого вы хотите получать личные сообщения.",
        ));
        let Some(privacy) = privacy else {
            return root
                .child(self.form_text("Загружаем настройки…", false))
                .into_any_element();
        };
        for (index, (label, description)) in crate::settings::PRIVACY_LABELS
            .iter()
            .zip(crate::settings::PRIVACY_DESCRIPTIONS.iter())
            .enumerate()
        {
            let mode = crate::settings::PRIVACY_MODES[index];
            root = root.child(self.settings_option(
                ElementId::Name(format!("privacy-{mode}").into()),
                label,
                description,
                privacy.mode == mode,
                false,
                SettingsAct::Personal(personal_settings::Action::SetPrivacy(index)),
                window,
                cx,
            ));
        }
        root = root
            .child(self.separator())
            .child(self.heading(
                "Системные уведомления",
                "Личные сообщения, звонки и события, пока Voxhold открыт в фоне.",
            ))
            .child(self.settings_toggle(
                ElementId::Name("desktop-notifications".into()),
                "Системные уведомления",
                "",
                notifications,
                false,
                SettingsAct::Personal(personal_settings::Action::ToggleNotifications),
                window,
                cx,
            ));
        if !notification_error.is_empty() {
            root = root.child(self.form_text(&notification_error, true));
        }
        root = root.child(self.separator()).child(self.heading(
            "Заблокированные пользователи",
            "Эти пользователи не смогут писать вам независимо от выбранной настройки.",
        ));
        if privacy.blocked_user_ids.is_empty() {
            root = root.child(self.form_text("Вы никого не заблокировали", false));
        }
        for id in &privacy.blocked_user_ids {
            let name = self
                .members
                .iter()
                .find(|member| member.user_id == *id)
                .map(|member| member.username.clone())
                .unwrap_or_else(|| format!("Пользователь #{id}"));
            root = root
                .child(div().text_size(px(13.)).child(name))
                .child(self.settings_button(
                    ElementId::Name(format!("unblock-{id}").into()),
                    "Разблокировать",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::Unblock(*id)),
                    window,
                    cx,
                ));
        }
        let _ = p;
        root.into_any_element()
    }

    fn voice_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let devices = self.voice_settings.devices.clone();
        let loading = self.voice_settings.devices_loading;
        let error = if self.voice_settings.devices_error.is_empty() {
            self.voice_settings.saved_error.clone()
        } else {
            self.voice_settings.devices_error.clone()
        };
        let prefs = self.voice_settings.prefs.clone();
        let (input_names, _, input_current) =
            crate::voice_settings::device_choices(&prefs, &devices, true);
        let (output_names, _, output_current) =
            crate::voice_settings::device_choices(&prefs, &devices, false);
        let mut root = div().flex().flex_col().gap(px(14.)).child(self.heading(
            "Голос и звук",
            "Устройства и уровни звука. Голосовой движок не перезапускается при изменении этих настроек.",
        ));
        root = root
            .child(self.heading(
                "Устройства",
                "Выберите, откуда брать голос и куда выводить звук комнаты.",
            ))
            .child(self.form_text(&format!("Устройство ввода · {input_current}"), false));
        for index in 0..input_names.len() {
            root = root.child(self.settings_button(
                ElementId::Name(format!("audio-in-{index}").into()),
                input_names[index].clone(),
                if input_names[index] == input_current {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                input_names[index] == input_current,
                false,
                SettingsAct::Personal(personal_settings::Action::SelectInput(index)),
                window,
                cx,
            ));
        }
        root = root.child(self.form_text(&format!("Устройство вывода · {output_current}"), false));
        for index in 0..output_names.len() {
            root = root.child(self.settings_button(
                ElementId::Name(format!("audio-out-{index}").into()),
                output_names[index].clone(),
                if output_names[index] == output_current {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                output_names[index] == output_current,
                false,
                SettingsAct::Personal(personal_settings::Action::SelectOutput(index)),
                window,
                cx,
            ));
        }
        root = root.child(self.settings_button(
            ElementId::Name("audio-refresh".into()),
            if loading {
                "Обновляем…"
            } else {
                "Обновить"
            },
            ButtonKind::Secondary,
            false,
            loading,
            SettingsAct::Personal(personal_settings::Action::RefreshDevices),
            window,
            cx,
        ));
        root = root
            .child(self.separator())
            .child(self.heading(
                "Уровни звука",
                "Громкость микрофона и обработка применяются в голосовом канале. Качество передачи применяется там же.",
            ))
            .child(self.settings_value_slider(
                "input-volume",
                "Громкость микрофона",
                prefs.input_volume,
                200,
                5,
                |value| SettingsAct::Personal(personal_settings::Action::SetVolume("input", value)),
                window,
                cx,
            ))
            .child(self.settings_value_slider(
                "output-volume",
                "Громкость вывода",
                prefs.output_volume,
                200,
                5,
                |value| SettingsAct::Personal(personal_settings::Action::SetVolume("output", value)),
                window,
                cx,
            ))
            .child(self.form_text(
                "Громкость микрофона 0–200 % применяется в голосовом канале до Opus. Громкость вывода 0–200 %: 0 % выключает комнату, числовой коэффициент появится в W02c.",
                false,
            ))
            .child(self.form_text(&format!("Качество передачи · {} Кбит/с", prefs.bitrate), false));
        for bitrate in crate::voice_settings::BITRATES {
            root = root.child(self.settings_button(
                ElementId::Name(format!("bitrate-{bitrate}").into()),
                bitrate.to_string(),
                if bitrate == prefs.bitrate {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                bitrate == prefs.bitrate,
                false,
                SettingsAct::Personal(personal_settings::Action::SetBitrate(bitrate)),
                window,
                cx,
            ));
        }
        root = root.child(self.separator()).child(self.settings_toggle(
            ElementId::Name("sounds".into()),
            "Звуки подключения и микрофона",
            "",
            prefs.sounds,
            false,
            SettingsAct::Personal(personal_settings::Action::ToggleSounds),
            window,
            cx,
        ));
        if prefs.sounds {
            root = root.child(self.settings_value_slider(
                "sound-volume",
                "Громкость сигналов",
                prefs.sound_volume,
                100,
                5,
                |value| SettingsAct::Personal(personal_settings::Action::SetVolume("sound", value)),
                window,
                cx,
            ));
        }
        root = root
            .child(self.separator())
            .child(self.heading("Обработка микрофона", ""))
            .child(self.settings_option(
                ElementId::Name("noise-auto".into()),
                "Автоматически",
                "",
                prefs.noise_mode == "auto",
                false,
                SettingsAct::Personal(personal_settings::Action::SetNoiseMode(0)),
                window,
                cx,
            ))
            .child(self.settings_option(
                ElementId::Name("noise-threshold".into()),
                "С нижней планкой",
                "",
                prefs.noise_mode == "threshold",
                false,
                SettingsAct::Personal(personal_settings::Action::SetNoiseMode(1)),
                window,
                cx,
            ));
        if prefs.noise_mode == "threshold" {
            root = root.child(self.settings_value_slider(
                "noise-gate",
                "Планка шума",
                prefs.noise_gate,
                100,
                5,
                |value| SettingsAct::Personal(personal_settings::Action::SetVolume("noise", value)),
                window,
                cx,
            ));
        }
        root = root
            .child(self.separator())
            .child(self.heading("Режим ввода и горячие клавиши", ""))
            .child(self.settings_option(
                ElementId::Name("input-mode-voice".into()),
                "Постоянный микрофон",
                "Голос передаётся, пока микрофон не выключен.",
                prefs.input_mode == "voice_activity",
                false,
                SettingsAct::Personal(personal_settings::Action::SetInputMode(0)),
                window,
                cx,
            ))
            .child(self.settings_option(
                ElementId::Name("input-mode-ptt".into()),
                "Нажми и говори",
                "Микрофон работает только при удержании клавиши.",
                prefs.input_mode == "push_to_talk",
                false,
                SettingsAct::Personal(personal_settings::Action::SetInputMode(1)),
                window,
                cx,
            ))
            .child(self.form_text(
                &format!(
                    "Микрофон: {} · Звук: {} · Нажми и говори: {}",
                    prefs.mute, prefs.deafen, prefs.push_to_talk
                ),
                false,
            ));
        if !prefs.shortcuts_valid() {
            root = root
                .child(self.form_text("Назначьте разные сочетания для каждого действия.", true));
        }
        if !error.is_empty() {
            root = root.child(self.form_text(&error, true));
        }
        root = root.child(self.separator()).child(
            div()
                .flex()
                .gap_2()
                .child(self.settings_button(
                    ElementId::Name("voice-reset".into()),
                    "Сбросить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::ResetVoice),
                    window,
                    cx,
                ))
                .child(self.settings_button(
                    ElementId::Name("voice-done".into()),
                    "Готово",
                    ButtonKind::Primary,
                    false,
                    false,
                    SettingsAct::Close,
                    window,
                    cx,
                )),
        );
        let _ = p;
        root.into_any_element()
    }
}
