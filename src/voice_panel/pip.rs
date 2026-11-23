//! Compact in-client picture-in-picture of the surfaces this panel still owns
//! after the user leaves their room: an own publication, a watched stream, or
//! both with a role switcher. Geometry is pure and lives in `voice_room`; this
//! file paints it and forwards one explicit intent per action.

use super::*;
use voxhold_media::publisher::Phase;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PipRole {
    Viewer,
    Publisher,
}

impl VoicePanel {
    pub(in crate::voice_panel) fn pip_active(&self) -> bool {
        self.screen_pip_active() || self.viewer_pip_active()
    }

    /// The player owns the whole window. The fullscreen layer, not the compact
    /// card, is what the user sees and can reach with Tab.
    pub(in crate::voice_panel) fn fullscreen_player(&self) -> bool {
        self.player_fullscreen && self.player_open
    }

    /// Both roles are owned at once, so the PiP reserves the role switcher.
    pub(in crate::voice_panel) fn pip_tabs(&self) -> bool {
        self.screen_pip_active() && self.viewer_pip_active()
    }

    /// The role the PiP really shows. A selected role that just ended falls
    /// back to the remaining one instead of leaving an empty card.
    pub(in crate::voice_panel) fn active_pip_role(&self) -> PipRole {
        match self.pip_role {
            PipRole::Viewer if self.viewer_pip_active() => PipRole::Viewer,
            PipRole::Publisher if self.screen_pip_active() => PipRole::Publisher,
            _ if self.viewer_pip_active() => PipRole::Viewer,
            _ => PipRole::Publisher,
        }
    }

    pub(super) fn pip_title(&self) -> String {
        match self.active_pip_role() {
            PipRole::Viewer => self
                .watching
                .as_ref()
                .map(|publication| self.member_name(publication.publisher_user_id))
                .unwrap_or_else(|| "Трансляция экрана".into()),
            PipRole::Publisher => {
                if self.publisher_connection_matches() {
                    "Ваш экран".into()
                } else {
                    "Ваш экран · восстановление".into()
                }
            }
        }
    }

    pub(super) fn pip_status(&self) -> String {
        match self.active_pip_role() {
            PipRole::Viewer => self.player_status_line(),
            PipRole::Publisher => {
                let line = Self::screen_card_status(&self.screen.sharing);
                match self.screen.sharing.phase {
                    Phase::Failed => self
                        .screen
                        .sharing
                        .error
                        .clone()
                        .filter(|error| !error.is_empty())
                        .unwrap_or(line),
                    _ => line,
                }
            }
        }
    }

    /// Whether the settings action of the painted role really opens something.
    /// The publisher dialog only exists for a confirmed live attempt; a
    /// starting or failed one has no configuration to change.
    pub(super) fn pip_settings_enabled(&self) -> bool {
        match self.active_pip_role() {
            PipRole::Viewer => true,
            PipRole::Publisher => self.screen.sharing.phase == Phase::Live,
        }
    }

    /// Whether the close action of the painted role has a command or a local
    /// dismissal to run. A stop that is already in flight (`Stopping`) owns
    /// nothing more, so its control is a status, not a button.
    pub(super) fn pip_close_enabled(&self) -> bool {
        match self.active_pip_role() {
            PipRole::Viewer => true,
            PipRole::Publisher => self.screen.sharing.phase != Phase::Stopping,
        }
    }

    /// Tab entries of the painted fullscreen layer: its exit control and, when
    /// the evidence track exists, the viewer-only volume control.
    fn fullscreen_focus_entries(&self) -> Vec<(&'static str, FocusHandle)> {
        let mut ring = Vec::new();
        if self.has_audio() {
            ring.push(("stream-audio", self.player_actions[0].clone()));
        }
        ring.push(("stream-exit-full", self.player_actions[1].clone()));
        ring
    }

    /// The Tab ring entries of the painted PiP. They exist only for the role
    /// currently visible and for the compact panel that is really open. While
    /// the player owns the window, the fullscreen layer replaces the card
    /// entirely, so none of the card's buttons are painted or focusable.
    pub(super) fn pip_focus_entries(&self) -> Vec<(&'static str, FocusHandle)> {
        if self.fullscreen_player() {
            return self.fullscreen_focus_entries();
        }
        if !self.pip_active() {
            return Vec::new();
        }
        if self.pip_settings && self.active_pip_role() == PipRole::Viewer {
            let mut ring = Vec::new();
            if self.has_audio() {
                ring.push(("pip-settings-audio", self.player_actions[6].clone()));
            } else {
                ring.push(("pip-settings-fullscreen", self.player_actions[7].clone()));
            }
            ring.push(("pip-settings-leave", self.player_actions[8].clone()));
            ring.push(("pip-settings", self.player_actions[3].clone()));
            return ring;
        }
        let mut ring = Vec::new();
        if self.pip_tabs() {
            ring.push(("pip-tab-viewer", self.player_actions[4].clone()));
            ring.push(("pip-tab-publisher", self.player_actions[5].clone()));
        }
        if self.pip_settings_enabled() {
            ring.push(("pip-settings", self.player_actions[3].clone()));
        }
        if self.pip_close_enabled() {
            ring.push(("pip-close", self.player_actions[2].clone()));
        }
        ring
    }

    /// Index in `player_actions` of one PiP entry id. The fullscreen exit uses
    /// it to land on an action the restored card really paints: a hardcoded
    /// index would target a button that this role may not show.
    fn pip_action_index(id: &str) -> Option<usize> {
        Some(match id {
            "pip-settings-audio" => 6,
            "pip-settings-fullscreen" => 7,
            "pip-settings-leave" => 8,
            "pip-tab-viewer" => 4,
            "pip-tab-publisher" => 5,
            "pip-settings" => 3,
            "pip-close" => 2,
            _ => return None,
        })
    }

    /// The first action of the painted PiP this frame, in Tab order. `None`
    /// means no control of the card is focusable (a stopping publication paints
    /// its close as a status), so the caller must keep the keyboard on a
    /// surface that exists.
    pub(super) fn pip_first_focus_index(&self) -> Option<usize> {
        self.pip_focus_entries()
            .into_iter()
            .find_map(|(id, _)| Self::pip_action_index(id))
    }

    /// The action the restored card must focus after leaving fullscreen: the
    /// first painted control of the role that is really shown, not a hardcoded
    /// index. The publisher role paints no settings button before its attempt
    /// is Live, so its first real action is the publisher tab or the close.
    pub(super) fn pip_exit_focus_index(&self) -> Option<usize> {
        let entries = self.pip_focus_entries();
        let preferred: &[&str] = match self.active_pip_role() {
            PipRole::Publisher => &["pip-tab-publisher", "pip-settings", "pip-close"],
            PipRole::Viewer => &[
                "pip-tab-viewer",
                "pip-settings",
                "pip-close",
                "pip-settings-audio",
                "pip-settings-fullscreen",
                "pip-settings-leave",
            ],
        };
        preferred
            .iter()
            .find(|id| entries.iter().any(|(entry, _)| entry == *id))
            .and_then(|id| Self::pip_action_index(id))
            .or_else(|| self.pip_first_focus_index())
    }

    fn focus_pip(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.player_actions[index]);
        cx.notify();
    }

    pub(super) fn pip_toggle_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.pip_active() {
            return false;
        }
        match self.active_pip_role() {
            PipRole::Publisher => {
                // The existing publication dialog owns the quality/audio form;
                // it is bound to the confirmed publish request, not to the
                // displayed room.
                self.pip_restore_focus = true;
                self.open_screen_settings(window, cx);
                if !self.screen.open {
                    self.pip_restore_focus = false;
                }
                true
            }
            PipRole::Viewer => {
                if self.pip_settings {
                    self.pip_close_settings(window, cx);
                } else {
                    self.release_push_to_talk();
                    self.pip_settings = true;
                    let index = if self.has_audio() { 6 } else { 7 };
                    self.focus_pip(index, window, cx);
                }
                true
            }
        }
    }

    pub(super) fn pip_close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pip_settings = false;
        if self.pip_active() {
            // The panel belongs to the settings button; returning there keeps
            // the keyboard on the surface the user was using.
            self.focus_pip(3, window, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn pip_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.pip_settings {
            self.pip_close_settings(window, cx);
            return true;
        }
        false
    }

    pub(super) fn pip_close_role(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pip_settings = false;
        match self.active_pip_role() {
            // Closing the viewer while it owns the window must give the window
            // back; otherwise the user is left in fullscreen without a player.
            PipRole::Viewer => self.leave_stream(window, cx),
            PipRole::Publisher => self.stop_screen_share(window, cx),
        }
    }

    /// Enter/Space on a focused PiP or fullscreen action. Returns whether the
    /// key belonged to the surface. PTT and text input keep their existing path
    /// when no control is focused.
    pub(crate) fn pip_key_down(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let fullscreen = self.fullscreen_player();
        if !fullscreen && !self.pip_active() {
            return false;
        }
        if key == "escape" {
            // A fullscreen Escape belongs to the player and was already routed.
            return if fullscreen {
                false
            } else {
                self.pip_escape(window, cx)
            };
        }
        if key != "enter" && key != "space" {
            return false;
        }
        let focused = self
            .pip_focus_entries()
            .into_iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(id, _)| id);
        match focused {
            Some("stream-exit-full") => {
                self.exit_fullscreen(window);
                true
            }
            Some("stream-audio") => {
                self.toggle_stream_mute(cx);
                true
            }
            Some("pip-settings") => self.pip_toggle_settings(window, cx),
            Some("pip-close") => {
                self.pip_close_role(window, cx);
                true
            }
            Some("pip-tab-viewer") => {
                self.pip_role = PipRole::Viewer;
                cx.notify();
                true
            }
            Some("pip-tab-publisher") => {
                self.pip_role = PipRole::Publisher;
                cx.notify();
                true
            }
            Some("pip-settings-audio") => {
                self.toggle_stream_mute(cx);
                true
            }
            Some("pip-settings-fullscreen") => {
                self.pip_settings = false;
                if self.player_open {
                    self.player_toggle_fullscreen(window);
                }
                true
            }
            Some("pip-settings-leave") => {
                self.pip_settings = false;
                self.leave_stream(window, cx);
                true
            }
            _ => false,
        }
    }

    /// Drop panel/tab state that no longer has a surface, and return focus
    /// from a settings dialog that just closed.
    pub(super) fn sync_pip(&mut self, window: &mut Window) {
        if !self.pip_active() {
            self.pip_settings = false;
            self.pip_restore_focus = false;
            return;
        }
        if self.pip_settings && self.active_pip_role() != PipRole::Viewer {
            self.pip_settings = false;
        }
        if self.pip_restore_focus && !self.screen.open {
            self.pip_restore_focus = false;
            window.focus(&self.player_actions[3]);
        }
    }

    fn pip_panel_button(
        &self,
        id: &'static str,
        label: String,
        danger: bool,
        index: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let color = if danger { c.danger } else { c.fg };
        let selector = id;
        let mut button = div()
            .id(id)
            .debug_selector(move || format!("{selector}").into())
            .h(px(26.))
            .w_full()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(if danger { c.danger } else { c.line }))
            .bg(rgb(if danger { c.danger_bg } else { c.surface }))
            .text_color(rgb(color))
            .text_size(px(10.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .track_focus(&self.player_actions[index])
            .child(label)
            .on_click(cx.listener(move |panel, _, window, cx| {
                panel.activate_pip_panel(id, window, cx);
            }));
        button = button.when(danger, |button| button.font_weight(gpui::FontWeight::BOLD));
        button.into_any_element()
    }

    fn activate_pip_panel(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "pip-settings-audio" => self.toggle_stream_mute(cx),
            "pip-settings-fullscreen" => {
                self.pip_settings = false;
                if self.player_open {
                    self.player_toggle_fullscreen(window);
                }
            }
            "pip-settings-leave" => {
                self.pip_settings = false;
                self.leave_stream(window, cx);
            }
            _ => {}
        }
    }

    pub(super) fn pip_overlay(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.pip_active() {
            return None;
        }
        let player = self.player_box()?;
        let c = self.colors();
        let role = self.active_pip_role();
        let tabs = self.pip_tabs();
        let geometry = voice_room::pip_geometry(player, tabs);
        let local = |rect: Rect| Rect::new(rect.x - player.x, rect.y - player.y, rect.w, rect.h);
        let title = self.pip_title();
        let status = self.pip_status();
        let picture = local(geometry.picture);
        let header = local(geometry.header);
        let settings = local(geometry.settings);
        let close = local(geometry.close);
        let corner = local(geometry.corner);
        let panel_rect = local(geometry.panel);

        let image = match role {
            PipRole::Viewer => self.frame.clone(),
            PipRole::Publisher => self.screen.preview.clone(),
        };
        // The PiP publisher paints the same preview slot as the on-air card, so
        // the demand of this frame follows the PiP picture when that role is
        // on screen. The viewer role paints no screen preview.
        if role == PipRole::Publisher {
            self.screen_card_rect = Some(Rect::new(
                player.x + picture.x,
                player.y + picture.y,
                picture.w,
                picture.h,
            ));
        }
        let mut picture_div = div()
            .id("pip-picture")
            .debug_selector(|| "pip-picture".into())
            .absolute()
            .left(px(picture.x))
            .top(px(picture.y))
            .w(px(picture.w))
            .h(px(picture.h))
            .overflow_hidden()
            .bg(rgb(0x080808));
        if let Some(image) = image {
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
                    .left(px(6.))
                    .top(px(((picture.h - 28.) / 2.).max(0.)))
                    .w(px((picture.w - 12.).max(0.)))
                    .h(px(28.))
                    .rounded(px(7.))
                    .bg(gpui::rgba(0x000000b3))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .truncate()
                    .child(status),
            );
        }

        let mut card = div()
            .id("stream-pip")
            .debug_selector(|| "voice-pip".into())
            .key_context("VoiceRoom")
            .track_focus(&self.focus)
            .absolute()
            .left(px(player.x))
            .top(px(player.y))
            .w(px(player.w))
            .h(px(player.h))
            .rounded(px(12.))
            .bg(rgb(c.surface))
            .border_1()
            .border_color(rgb(c.line))
            .child(
                div()
                    .id("pip-header")
                    .debug_selector(|| "pip-header".into())
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
                        cx.listener(|panel, event: &gpui::MouseDownEvent, window, _| {
                            // A drag also puts the keyboard on the PiP, so Tab
                            // reaches its settings and close buttons afterwards.
                            window.focus(&panel.focus);
                            let point = event.position;
                            panel.player_pointer_down(point.x.into(), point.y.into());
                        }),
                    )
                    .child(img("icons/grip.svg").w(px(10.)).h(px(12.)))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .truncate()
                            .child(title),
                    ),
            )
            .child(picture_div);

        if tabs {
            let viewer_tab = local(geometry.viewer_tab);
            let publisher_tab = local(geometry.publisher_tab);
            let tab =
                |rect: Rect, id: &'static str, label: &'static str, active: bool, index: usize| {
                    let selector = id;
                    div()
                        .id(id)
                        .debug_selector(move || format!("{selector}").into())
                        .absolute()
                        .left(px(rect.x))
                        .top(px(rect.y))
                        .w(px(rect.w))
                        .h(px(rect.h))
                        .rounded(px(5.))
                        .bg(rgb(if active { c.accent } else { c.avatar_bg }))
                        .text_color(rgb(if active { c.ink } else { c.muted }))
                        .text_size(px(9.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .track_focus(&self.player_actions[index])
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.pip_role = if index == 4 {
                                PipRole::Viewer
                            } else {
                                PipRole::Publisher
                            };
                            panel.pip_settings = false;
                            cx.notify();
                        }))
                        .child(label)
                };
            card = card
                .child(tab(
                    viewer_tab,
                    "pip-tab-viewer",
                    "Просмотр",
                    role == PipRole::Viewer,
                    4,
                ))
                .child(tab(
                    publisher_tab,
                    "pip-tab-publisher",
                    "Ваш экран",
                    role == PipRole::Publisher,
                    5,
                ));
        }

        let close_label = match (role, self.screen.sharing.phase) {
            (PipRole::Viewer, _) => "Выйти",
            (PipRole::Publisher, Phase::Failed) => "Закрыть",
            (PipRole::Publisher, Phase::Stopping) => "Остановка…",
            (PipRole::Publisher, _) => "Завершить",
        };
        // A stopping attempt and an unconfirmed role have no command left to
        // send; their control is painted as a status instead of a live button.
        let close_enabled = self.pip_close_enabled();
        let settings_enabled = self.pip_settings_enabled();
        let mut close_button = div()
            .id("pip-close")
            .debug_selector(|| "pip-close".into())
            .absolute()
            .left(px(close.x))
            .top(px(close.y))
            .w(px(close.w))
            .h(px(close.h))
            .rounded(px(5.))
            .border_1()
            .border_color(rgb(if close_enabled { c.danger } else { c.line }))
            .text_color(rgb(if close_enabled { c.danger } else { c.muted }))
            .text_size(px(9.))
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .child(img("icons/close.svg").w(px(9.)).h(px(9.)))
            .child(close_label);
        if close_enabled {
            close_button = close_button
                .font_weight(gpui::FontWeight::BOLD)
                .cursor_pointer()
                .track_focus(&self.player_actions[2])
                .on_click(cx.listener(|panel, _, window, cx| {
                    panel.pip_close_role(window, cx);
                }));
        }
        let mut settings_button = div()
            .id("pip-settings")
            .debug_selector(|| "pip-settings".into())
            .absolute()
            .left(px(settings.x))
            .top(px(settings.y))
            .w(px(settings.w))
            .h(px(settings.h))
            .rounded(px(5.))
            .border_1()
            .border_color(rgb(c.line))
            .text_color(rgb(if settings_enabled { c.muted } else { c.line }))
            .text_size(px(9.))
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .child(img("icons/settings.svg").w(px(10.)).h(px(10.)))
            .child("Настройки");
        if settings_enabled {
            settings_button = settings_button
                .cursor_pointer()
                .track_focus(&self.player_actions[3])
                .on_click(cx.listener(|panel, _, window, cx| {
                    panel.pip_toggle_settings(window, cx);
                }));
        }
        card = card.child(settings_button).child(close_button);

        if self.pip_settings && role == PipRole::Viewer {
            let audio = self.has_audio();
            let muted = self.stream_muted;
            let mut panel = div()
                .id("pip-settings-panel")
                .debug_selector(|| "pip-settings-panel".into())
                .absolute()
                .left(px(panel_rect.x + 6.))
                .top(px(panel_rect.y + 6.))
                .w(px((panel_rect.w - 12.).max(1.)))
                .h(px((panel_rect.h - 12.).max(1.)))
                .rounded(px(9.))
                .bg(gpui::rgba(0x101010f2))
                .border_1()
                .border_color(rgb(c.line))
                .p(px(8.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(c.muted))
                        .child("Настройки просмотра"),
                );
            if audio {
                // The same mute path as the room player. It changes only this
                // viewer's output, never the publication's own sound.
                panel = panel.child(self.pip_panel_button(
                    "pip-settings-audio",
                    if muted {
                        "Звук: выключен".into()
                    } else {
                        "Звук: включён".into()
                    },
                    false,
                    6,
                    cx,
                ));
            } else {
                // No audio track: the honest available action is the full
                // screen picture, not a dead volume control.
                panel = panel.child(self.pip_panel_button(
                    "pip-settings-fullscreen",
                    "На весь экран".into(),
                    false,
                    7,
                    cx,
                ));
            }
            panel = panel.child(self.pip_panel_button(
                "pip-settings-leave",
                "Выйти из просмотра".into(),
                true,
                8,
                cx,
            ));
            card = card.child(panel);
        }

        card = card.child(
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
        Some(card.into_any_element())
    }
}
