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
/// Everything above and below the notes block: padding, gaps, header, version
/// line, the two preference rows, phase title and the 30 px button row. Keeps
/// the buttons painted.
const PANEL_CHROME: f32 = 186.0;
const NOTES_MAX_HEIGHT: f32 = 120.0;
const NOTES_MIN_HEIGHT: f32 = 40.0;

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
            .when(hovered && !self.update.panel_open, |element| {
                element.child(gpui::deferred(
                    div()
                        .id("update-tooltip")
                        .debug_selector(|| "update-tooltip".into())
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
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(ink(p.fg))
                        .child(tooltip),
                ))
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
        // The panel must not slide under the mobile bottom bar, so the chat
        // rectangle (not the viewport) is the real lower bound.
        let lower = chat.bottom().min(viewport_height);
        let max_height = (lower - top - PANEL_MARGIN).max(120.0);
        let notes_max = (max_height - PANEL_CHROME).clamp(NOTES_MIN_HEIGHT, NOTES_MAX_HEIGHT);
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
                    .flex_shrink_0()
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
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(ink(p.muted))
                    .child(format!("Текущая версия {current}")),
            )
            .child(self.update_prefs_rows(p, cx));

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
                    |view, _, cx| view.check_updates(cx),
                ));
            }
            UpdatePhase::Checking => {
                panel = panel.child(self.update_note("Проверяем обновления…", p));
                panel = panel.child(update_button(
                    "update-check",
                    "Проверяем…",
                    false,
                    false,
                    p,
                    cx,
                    |_, _, _| {},
                ));
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
                    |view, _, cx| view.check_updates(cx),
                ));
            }
            UpdatePhase::Available { version, notes } => {
                panel = panel.child(
                    div()
                        .flex_shrink_0()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("Доступна версия {version}")),
                );
                if !notes.trim().is_empty() {
                    panel = panel.child(
                        div()
                            .id("update-notes")
                            .debug_selector(|| "update-notes".into())
                            .min_h(px(0.))
                            .max_h(px(notes_max))
                            .overflow_y_scroll()
                            .text_color(ink(p.muted))
                            .child(notes),
                    );
                }
                panel = panel.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .flex_shrink_0()
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
            UpdatePhase::Preparing { version } => {
                panel = panel.child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("Версия {version}")),
                );
                panel = panel.child(self.update_note("Проверяем и готовим обновление…", p));
                // Staging still owns the cancel token here: a rejected delta
                // restarts the full download from this phase.
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
                let note = if self.update.confirm_install {
                    "Идёт звонок или трансляция. Нажмите ещё раз, чтобы перезапустить."
                } else {
                    update::ready_note(
                        self.update.prefs.install_on_next_start,
                        self.update.prefs.install_refused(&version),
                    )
                };
                panel = panel.child(self.update_note(note, p));
                panel = panel.child(update_button(
                    "update-install",
                    if self.update.confirm_install {
                        "Подтвердить перезапуск"
                    } else {
                        "Перезапустить и установить"
                    },
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
        if let Some(error) = &self.update.prefs_error {
            panel = panel.child(
                div()
                    .id("update-prefs-error")
                    .debug_selector(|| "update-prefs-error".into())
                    .text_size(px(11.))
                    .text_color(ink(p.danger))
                    .child(error.clone()),
            );
        }
        panel.into_any_element()
    }

    /// The two background switches. They are visible in every phase because
    /// they change what the next check and the next start will do.
    fn update_prefs_rows(&self, p: theme::Palette, cx: &mut Context<Self>) -> AnyElement {
        let auto = self.update.prefs.auto_download;
        let install = self.update.prefs.install_on_next_start;
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_1()
            .child(pref_row(
                "update-auto-download",
                "Скачивать обновления в фоне",
                auto,
                p,
                cx,
                |view, cx| view.toggle_auto_download(cx),
            ))
            .child(pref_row(
                "update-install-next",
                "Устанавливать при следующем запуске",
                install,
                p,
                cx,
                |view, cx| view.toggle_install_on_next_start(cx),
            ))
            .into_any_element()
    }

    /// Blocking surface while the helper takes over the installation. The
    /// process is about to exit, so the window only explains why. The mark and
    /// the static ring mirror the helper window that follows.
    pub(super) fn update_install_overlay(&self) -> AnyElement {
        let p = self.palette;
        div()
            .id("update-installing")
            .debug_selector(|| "update-installing".into())
            .absolute()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(ink(p.bg))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(92.))
                            .h(px(92.))
                            .rounded_full()
                            .border_2()
                            .border_color(ink(p.line))
                            .child(icon("icons/brand.svg", 64., ink(p.lime))),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(ink(p.fg))
                            .child("Обновляем Voxhold"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(ink(p.muted))
                            .child("Подготавливаем обновление…"),
                    ),
            )
            .into_any_element()
    }

    fn update_note(&self, text: &str, p: theme::Palette) -> AnyElement {
        div()
            .text_color(ink(p.muted))
            .child(text.to_string())
            .into_any_element()
    }
}

fn pref_row(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    p: theme::Palette,
    cx: &mut Context<Voxhold>,
    on_click: impl Fn(&mut Voxhold, &mut Context<Voxhold>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex()
        .items_center()
        .justify_between()
        .flex_shrink_0()
        .h(px(16.))
        .cursor_pointer()
        .text_size(px(11.))
        .child(div().child(label))
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(ink(if enabled { p.lime } else { p.muted }))
                .child(if enabled { "Вкл" } else { "Выкл" }),
        )
        .on_click(cx.listener(move |view, _, _, cx| on_click(view, cx)))
        .into_any_element()
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
        .flex_shrink_0()
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
