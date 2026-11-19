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

/// Inner padding `settings_card` paints on every side. A row inside a card is
/// narrower than the dialog body by this amount on each side.
pub(super) const CARD_INSET: f32 = 16.0;

/// Logical box of the settings card. The voice tab reuses the same numbers to
/// decide whether its form is wide enough for two columns, so the predicate
/// follows the real painted dialog instead of a hard-coded window guess.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CardMetrics {
    pub width: f32,
    pub height: f32,
    pub pad: f32,
}

impl CardMetrics {
    /// Usable width of one form row: the card minus both dialog insets and the
    /// card's own inner padding on each side.
    pub(super) fn form_width(self) -> f32 {
        self.width - 2.0 * self.pad - 2.0 * CARD_INSET
    }
}

/// The card box from the window viewport and the view scale. Physical margins
/// stay constant across DPI steps: the logical inset grows with the scale.
pub(super) fn settings_card_metrics(viewport: (f32, f32), scale: f32) -> CardMetrics {
    let scale = scale.clamp(1.0, 3.0);
    let width = 680_f32.min(viewport.0 - 16. * scale).max(260.);
    let height = 780_f32.min(viewport.1 - 24. * scale).max(240.);
    let pad = if width < 440. { 16. } else { 24. };
    CardMetrics { width, height, pad }
}

impl Voxhold {
    /// The settings elements are looked up by their id in the layout tests; the
    /// debug selector is a no-op outside test builds.
    fn element_selector(id: &ElementId) -> Option<String> {
        match id {
            ElementId::Name(name) => Some(name.to_string()),
            _ => None,
        }
    }

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
        let selector = Self::element_selector(&id);
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
        if let Some(selector) = selector {
            button = button.debug_selector(move || selector.clone());
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
        let selector = Self::element_selector(&id);
        let mut row = div()
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
            });
        if let Some(selector) = selector {
            row = row.debug_selector(move || selector.clone());
        }
        row.into_any_element()
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
        let selector = Self::element_selector(&id);
        if !disabled {
            self.settings_ring
                .push((focus.clone(), Some(action.clone())));
        }
        let mut row = div()
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
            );
        if let Some(selector) = selector {
            row = row.debug_selector(move || selector.clone());
        }
        row.into_any_element()
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

    /// One bordered group inside the settings form, as in the web
    /// `voice-settings__section`: a title, an optional description, an optional
    /// trailing action and the body. The theme's `surface`/`line` tokens make
    /// every card read as part of the same dialog.
    pub(super) fn settings_card(
        &self,
        id: &'static str,
        title: &str,
        description: &str,
        trailing: Option<AnyElement>,
        body: AnyElement,
    ) -> AnyElement {
        let p = self.palette;
        let mut header = div()
            .w_full()
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(title.to_owned()),
                    )
                    .when(!description.is_empty(), |text| {
                        text.child(
                            div()
                                .text_size(px(12.))
                                .text_color(ink(p.muted))
                                .child(description.to_owned()),
                        )
                    }),
            );
        if let Some(trailing) = trailing {
            header = header.child(trailing);
        }
        div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || id.to_string())
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(CARD_INSET))
            .rounded(px(14.))
            .border_1()
            .border_color(ink(p.line))
            .bg(ink(p.surface))
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// Pin the logical scale the settings card lays out against. The production
    /// path samples `window.scale_factor()` on every paint; the layout tests
    /// pin it to emulate 125 %/150 % on a fixed test window.
    #[cfg(test)]
    pub(super) fn test_pin_ui_scale(&mut self, scale: f32) {
        self.ui_scale = scale.max(1.0);
        self.ui_scale_pinned = true;
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
        let metrics = settings_card_metrics((vw, vh), self.ui_scale);
        let (width, height, pad) = (metrics.width, metrics.height, metrics.pad);
        let left = (vw - width) / 2.;
        let top = (vh - height) / 2.;
        self.settings_ring.clear();
        self.settings_slider_focus.clear();

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
            .debug_selector(|| "settings-card".into())
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
            .child(tab_strip)
            // Moves while the pointer is over the form (the card occludes the
            // backdrop, so only one of the two receives the event).
            .on_mouse_move(cx.listener(|s, event: &gpui::MouseMoveEvent, window, cx| {
                s.slider_pointer_moved(event, window, cx)
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|s, _, _, _| s.end_slider_drag()),
            );
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
                // The pointer capture of the Go slider: a drag that leaves the
                // card keeps updating the value while the button is held, and
                // the track math clamps it to the row ends. The backdrop covers
                // the whole window, so moves outside the card still arrive.
                .on_mouse_move(cx.listener(|s, event: &gpui::MouseMoveEvent, window, cx| {
                    s.slider_pointer_moved(event, window, cx)
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, _| s.end_slider_drag()),
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
}
