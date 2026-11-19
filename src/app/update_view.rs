//! Header update icon, hover tooltip and the 320 px popup.
//!
//! Only palette tokens are used, so light and dark themes share one layout.
//! The popup is positioned from the chat rectangle and clamped to the client
//! area; at narrow widths it shrinks instead of leaving the window.

use super::*;
use crate::update::{self, RetryKind, UpdatePhase};
use gpui::AnyElement;

const PANEL_WIDTH: f32 = 320.0;
const PANEL_MARGIN: f32 = 8.0;
const HEADER_GAP: f32 = 6.0;

impl Voxhold {
    /// Header button before the search action.
    pub(super) fn update_icon(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let current = update::current_version();
        let tooltip = self.update.phase.tooltip(&current);
        let icon_path = self.update.phase.icon();
        let color = match &self.update.phase {
            UpdatePhase::Available { .. } | UpdatePhase::Ready { .. } => p.lime,
            UpdatePhase::Error { .. } => p.danger,
            _ => p.muted,
        };
        let hovered = self.update.hover;
        div()
            .id("update-icon")
            .debug_selector(|| "update-icon".into())
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.))
            .h(px(24.))
            .cursor_pointer()
            .on_hover(cx.listener(|view, hovered, _, cx| {
                if view.update.hover != *hovered {
                    view.update.hover = *hovered;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|view, _, _, cx| view.toggle_update_panel(cx)))
            .child(icon(icon_path, 16., ink(color)))
            .when(hovered, |element| {
                element.child(
                    div()
                        .id("update-tooltip")
                        .debug_selector(|| "update-tooltip".into())
                        .occlude()
                        .absolute()
                        .top(px(28.))
                        .right(px(0.))
                        .w(px(210.))
                        .px(px(8.))
                        .py(px(5.))
                        .rounded(px(7.))
                        .bg(ink(p.surface))
                        .border_1()
                        .border_color(ink(p.line))
                        .text_size(px(11.))
                        .text_color(ink(p.muted))
                        .child(tooltip),
                )
            })
            .into_any_element()
    }

    /// Popup under the header. `chat` is the layout rectangle of the chat pane.
    pub(super) fn update_panel(
        &self,
        chat: layout::Rect,
        mobile: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let (viewport_width, viewport_height) = self.viewport;
        let header_height = if mobile { 58.0 } else { 67.0 };
        let width = PANEL_WIDTH
            .min((viewport_width - 2.0 * PANEL_MARGIN).max(1.0))
            .max(1.0);
        let x = (chat.x + chat.w - width - 12.0)
            .max(PANEL_MARGIN)
            .min((viewport_width - width - PANEL_MARGIN).max(PANEL_MARGIN));
        let top = chat.y + header_height + HEADER_GAP;
        let max_height = (viewport_height - top - PANEL_MARGIN).max(120.0);
        let current = update::current_version();

        let mut panel = div()
            .id("update-panel")
            .debug_selector(|| "update-panel".into())
            .occlude()
            .absolute()
            .left(px(x))
            .top(px(top))
            .w(px(width))
            .max_h(px(max_height))
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap_2()
            .p(px(12.))
            .rounded(px(12.))
            .bg(ink(p.surface))
            .border_1()
            .border_color(ink(p.line))
            .shadow_lg()
            .text_size(px(12.))
            .text_color(ink(p.fg))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Обновления"),
                    )
                    .child(
                        div()
                            .id("update-close")
                            .debug_selector(|| "update-close".into())
                            .cursor_pointer()
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|view, _, _, cx| view.close_update_panel(cx)))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .child(format!("Текущая версия {current}")),
            );

        let phase = self.update.phase.clone();
        match phase {
            UpdatePhase::Idle => {
                panel = panel.child(self.update_note("Проверка ещё не выполнялась.", p));
                panel = panel.child(update_button(
                    "update-check",
                    "Проверить",
                    true,
                    true,
                    p,
                    cx,
                    |view, _, cx| view.check_updates(true, cx),
                ));
            }
            UpdatePhase::Checking => {
                panel = panel.child(self.update_note("Проверяем обновления…", p));
            }
            UpdatePhase::UpToDate => {
                panel = panel.child(self.update_note("Установлена последняя версия.", p));
                panel = panel.child(update_button(
                    "update-check",
                    "Проверить снова",
                    false,
                    true,
                    p,
                    cx,
                    |view, _, cx| view.check_updates(true, cx),
                ));
            }
            UpdatePhase::Available { version, notes } => {
                panel = panel.child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("Доступна версия {version}")),
                );
                if !notes.trim().is_empty() {
                    panel = panel.child(
                        div()
                            .id("update-notes")
                            .debug_selector(|| "update-notes".into())
                            .max_h(px(120.))
                            .overflow_y_scroll()
                            .text_color(ink(p.muted))
                            .child(notes),
                    );
                }
                panel = panel.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(update_button(
                            "update-download",
                            "Скачать",
                            true,
                            true,
                            p,
                            cx,
                            |view, _, cx| view.start_download(cx),
                        ))
                        .child(update_button(
                            "update-skip",
                            "Пропустить версию",
                            false,
                            true,
                            p,
                            cx,
                            |view, _, cx| view.skip_version(cx),
                        )),
                );
            }
            UpdatePhase::Downloading {
                version,
                downloaded,
                total,
            } => {
                let pct = update::percent(downloaded, total);
                panel = panel.child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("Загружаем {version}")),
                );
                panel = panel.child(
                    div()
                        .id("update-progress")
                        .debug_selector(|| "update-progress".into())
                        .h(px(6.))
                        .w_full()
                        .rounded_full()
                        .bg(ink(p.line))
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(ink(p.lime))
                                .w(px((width - 24.0) * f32::from(pct) / 100.0)),
                        ),
                );
                panel = panel.child(
                    div()
                        .id("update-percent")
                        .debug_selector(|| "update-percent".into())
                        .text_size(px(11.))
                        .text_color(ink(p.muted))
                        .child(format!(
                            "{pct}% · {} из {}",
                            format_bytes(downloaded),
                            format_bytes(total)
                        )),
                );
                let cancelling = self.update.cancelling;
                panel = panel.child(update_button(
                    "update-cancel",
                    if cancelling {
                        "Отмена…"
                    } else {
                        "Отмена"
                    },
                    false,
                    !cancelling,
                    p,
                    cx,
                    |view, _, cx| view.cancel_download(cx),
                ));
            }
            UpdatePhase::Ready { version, .. } => {
                panel = panel.child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("Версия {version} готова")),
                );
                panel = panel.child(
                    self.update_note("Приложение перезапустится и установит обновление.", p),
                );
                panel = panel.child(update_button(
                    "update-install",
                    "Перезапустить и установить",
                    true,
                    true,
                    p,
                    cx,
                    |view, window, cx| view.install_ready(window, cx),
                ));
            }
            UpdatePhase::Error { message, retry } => {
                panel = panel.child(
                    div()
                        .id("update-error")
                        .debug_selector(|| "update-error".into())
                        .text_color(ink(p.danger))
                        .child(message),
                );
                let label = match retry {
                    RetryKind::Check => "Повторить проверку",
                    RetryKind::Download => "Повторить загрузку",
                    RetryKind::Install => "Повторить установку",
                };
                panel = panel.child(update_button(
                    "update-retry",
                    label,
                    true,
                    true,
                    p,
                    cx,
                    |view, window, cx| view.retry_update(window, cx),
                ));
            }
            UpdatePhase::Installing => {
                panel = panel.child(self.update_note("Устанавливаем обновление…", p));
            }
        }

        if let Some(error) = &self.update.handshake_error {
            panel = panel.child(
                div()
                    .text_size(px(11.))
                    .text_color(ink(p.danger))
                    .child(error.clone()),
            );
        }
        panel.into_any_element()
    }

    fn update_note(&self, text: &str, p: theme::Palette) -> AnyElement {
        div()
            .text_color(ink(p.muted))
            .child(text.to_string())
            .into_any_element()
    }
}

fn update_button(
    id: &'static str,
    label: impl Into<String>,
    primary: bool,
    enabled: bool,
    p: theme::Palette,
    cx: &mut Context<Voxhold>,
    on_click: impl Fn(&mut Voxhold, &mut Window, &mut Context<Voxhold>) + 'static,
) -> AnyElement {
    let background = if primary { p.lime } else { p.bg };
    let foreground = if primary {
        p.accent_button_ink
    } else if enabled {
        p.fg
    } else {
        p.muted
    };
    let mut button = div()
        .id(id)
        .debug_selector(move || id.into())
        .min_w(px(96.))
        .h(px(30.))
        .px(px(10.))
        .rounded(px(8.))
        .bg(ink(background))
        .border_1()
        .border_color(ink(if primary { background } else { p.line }))
        .text_color(ink(foreground))
        .font_weight(gpui::FontWeight::BOLD)
        .text_size(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .child(label.into());
    if enabled {
        button = button
            .cursor_pointer()
            .on_click(cx.listener(move |view, _, window, cx| on_click(view, window, cx)));
    }
    button.into_any_element()
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} МБ", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} КБ", bytes / KIB)
    } else {
        format!("{bytes} Б")
    }
}
