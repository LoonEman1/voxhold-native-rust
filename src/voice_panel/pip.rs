//! The stream surface model and the compact PiP.
//!
//! One predicate decides where each role paints: a stream whose owner is the
//! call channel that is really shown keeps the room surface (the own card or
//! the standard player); the same stream in any other surface is the compact
//! PiP. Slot count, painted content and the focus ring are all derived from
//! that one decision, so navigation can never leave a card in a foreign grid
//! or drop the picture.
//!
//! The PiP is a UI surface only: the viewer keeps its `WatchIntent`, selection
//! and frame slot, and the publisher keeps its capture, `screen.preview` and
//! audio track. Switching tabs or surfaces never sends a media command.

use super::*;
use crate::voice_room::PIP_WIDTH;
use voxhold_media::publisher::Phase;

/// Which role the single PiP shows when both intents are alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipTab {
    View,
    Own,
}

/// Where one role paints right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamSurface {
    Hidden,
    Room,
    Pip,
}

/// Confirmed access of the channel that owns the live publication. It is
/// captured from the source channel and never from the displayed room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishAccess {
    pub readable: bool,
    pub can_stream: bool,
    pub slot_available: bool,
}

impl Default for PublishAccess {
    fn default() -> Self {
        Self {
            readable: true,
            can_stream: true,
            slot_available: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PipKey {
    View,
    Own,
    Audio,
    Fullscreen,
    Settings,
    Close,
    Leave,
}

impl PipKey {
    fn index(self) -> usize {
        match self {
            Self::View => 0,
            Self::Own => 1,
            Self::Audio => 2,
            Self::Fullscreen => 3,
            Self::Settings => 4,
            Self::Close => 5,
            Self::Leave => 6,
        }
    }
}

impl VoicePanel {
    pub fn self_connection(&self) -> Option<&str> {
        self.self_connection.as_deref()
    }

    pub fn publish_room(&self) -> Option<i64> {
        self.publish_room
    }

    /// The shell's foreground answer: the voice room pane of this server and
    /// channel really paints this frame. Every stream surface compares its
    /// owner against this one value.
    pub fn set_foreground(&mut self, room_visible: bool, server: i64, channel: i64) {
        self.room_painted = room_visible && server == self.server && channel == self.room;
    }

    /// Push the confirmed access of the publication's own channel. A revoke
    /// closes an open live-settings surface; the media worker already received
    /// the same gate through `sync_screen_access`.
    pub fn set_publish_access(&mut self, readable: bool, can_stream: bool, slot_available: bool) {
        let next = PublishAccess {
            readable,
            can_stream,
            slot_available,
        };
        if self.publish_access == next {
            return;
        }
        self.publish_access = next;
        if !readable || !can_stream {
            if self.screen.open && self.screen.restart {
                self.screen.open = false;
                self.screen.restart = false;
                self.screen.preflight = false;
                self.screen.busy = false;
                self.screen.pending_quality_op = None;
                self.screen.focus_placed = false;
                self.screen.clear_previews();
            }
            self.retain_pip_state();
        }
        self.sync_screen_access();
    }

    /// The one predicate behind slot count, painted content and focus ring.
    fn stream_foreground(&self, owner: i64) -> bool {
        self.access_confirmed
            && self.room_painted
            && self.room == owner
            && self.call_channel == owner
    }

    /// The local publication surface. `Failed` keeps the error card only in
    /// the source grid: it never returns as a PiP over another room.
    pub(crate) fn own_surface(&self) -> StreamSurface {
        let Some(owner) = self.publish_room else {
            return StreamSurface::Hidden;
        };
        let phase = self.screen.sharing.phase;
        if !matches!(
            phase,
            Phase::Publishing | Phase::Live | Phase::Stopping | Phase::Failed
        ) {
            return StreamSurface::Hidden;
        }
        if self.call_channel == 0 || self.call_channel != owner {
            return StreamSurface::Hidden;
        }
        if self.stream_foreground(owner) {
            return StreamSurface::Room;
        }
        if phase == Phase::Failed {
            return StreamSurface::Hidden;
        }
        StreamSurface::Pip
    }

    /// The watched publication surface. The selection id and the publisher
    /// connection stay the same on both surfaces.
    pub(crate) fn view_surface(&self) -> StreamSurface {
        let Some(watching) = &self.watching else {
            return StreamSurface::Hidden;
        };
        if !self.player_open {
            return StreamSurface::Hidden;
        }
        if self.call_channel == 0 || self.call_channel != watching.channel_id {
            return StreamSurface::Hidden;
        }
        if self.stream_foreground(watching.channel_id) {
            StreamSurface::Room
        } else {
            StreamSurface::Pip
        }
    }

    pub(crate) fn pip_visible(&self) -> bool {
        self.own_surface() == StreamSurface::Pip || self.view_surface() == StreamSurface::Pip
    }

    fn pip_both(&self) -> bool {
        self.own_surface() == StreamSurface::Pip && self.view_surface() == StreamSurface::Pip
    }

    /// The tab the single PiP really shows. A closed role drops out without
    /// changing the remaining surface.
    pub(crate) fn pip_tab(&self) -> PipTab {
        match (
            self.view_surface() == StreamSurface::Pip,
            self.own_surface() == StreamSurface::Pip,
        ) {
            (true, true) => self.pip_tab,
            (true, false) => PipTab::View,
            (false, true) => PipTab::Own,
            (false, false) => PipTab::View,
        }
    }

    pub(crate) fn pip_box(&self) -> Option<Rect> {
        if !self.pip_visible() || self.player_area.is_empty() {
            return None;
        }
        Some(voice_room::compact_stream_box(
            self.player_area,
            self.pip_pos,
            self.pip_width,
        ))
    }

    /// The box the pointer drags: the compact PiP when it is painted, the
    /// floating player of the source room otherwise.
    pub(super) fn surface_box(&self) -> Option<Rect> {
        if self.pip_visible() {
            self.pip_box()
        } else {
            self.player_box()
        }
    }

    pub(crate) fn switch_pip_tab(&mut self, tab: PipTab, cx: &mut Context<Self>) {
        if !self.pip_both() || self.pip_tab == tab {
            return;
        }
        self.pip_tab = tab;
        self.pip_settings_open = false;
        cx.notify();
    }

    fn pip_frame(&self) -> Option<Arc<RenderImage>> {
        if self.pip_tab() == PipTab::Own {
            self.screen.preview.clone()
        } else {
            self.frame.clone()
        }
    }

    fn own_status_line(&self) -> String {
        let sharing = &self.screen.sharing;
        if let Some(error) = sharing.error.as_deref().filter(|error| !error.is_empty()) {
            return error.to_owned();
        }
        if !sharing.status.is_empty() {
            return sharing.status.clone();
        }
        match sharing.phase {
            Phase::Publishing => "Запускаем…".into(),
            Phase::Live => "В эфире".into(),
            Phase::Stopping => "Останавливаем…".into(),
            Phase::Failed => "Ошибка".into(),
            Phase::Idle | Phase::Picking => String::new(),
        }
    }

    fn pip_status(&self) -> String {
        if self.pip_tab() == PipTab::Own {
            self.own_status_line()
        } else {
            self.player_status_line()
        }
    }

    fn pip_title(&self) -> String {
        if self.pip_tab() == PipTab::Own {
            return "Ваш экран".into();
        }
        let Some(watching) = &self.watching else {
            return "Трансляция экрана".into();
        };
        let room = self
            .rooms
            .get(&watching.channel_id)
            .map(|room| room.name.clone())
            .unwrap_or_else(|| "Трансляция экрана".into());
        let publisher = self.member_name(watching.publisher_user_id);
        if publisher.is_empty() {
            room
        } else {
            format!("{room} · {publisher}")
        }
    }

    fn pip_handle(&self, key: PipKey) -> FocusHandle {
        self.pip_focus[key.index()].clone()
    }

    /// Focus entries painted by the PiP: tabs, the actions of the active role
    /// and the open settings panel. The panel keeps focus inside itself while
    /// it is open.
    pub(super) fn pip_ring(&self) -> Vec<(PipKey, &'static str, FocusHandle)> {
        let mut ring = Vec::new();
        if !self.pip_visible() {
            return ring;
        }
        if self.pip_settings_open && self.pip_tab() == PipTab::View {
            if self.has_audio() {
                ring.push((
                    PipKey::Audio,
                    "pip-settings-audio",
                    self.pip_handle(PipKey::Audio),
                ));
            }
            ring.push((
                PipKey::Fullscreen,
                "pip-settings-fullscreen",
                self.pip_handle(PipKey::Fullscreen),
            ));
            ring.push((
                PipKey::Leave,
                "pip-settings-leave",
                self.pip_handle(PipKey::Leave),
            ));
            return ring;
        }
        if self.pip_both() {
            ring.push((PipKey::View, "pip-tab-view", self.pip_handle(PipKey::View)));
            ring.push((PipKey::Own, "pip-tab-own", self.pip_handle(PipKey::Own)));
        }
        if self.pip_tab() == PipTab::View && self.has_audio() {
            ring.push((PipKey::Audio, "pip-audio", self.pip_handle(PipKey::Audio)));
        }
        ring.push((
            PipKey::Settings,
            "pip-settings",
            self.pip_handle(PipKey::Settings),
        ));
        if self.pip_tab() == PipTab::View {
            ring.push((
                PipKey::Fullscreen,
                "pip-fullscreen",
                self.pip_handle(PipKey::Fullscreen),
            ));
        }
        ring.push((PipKey::Close, "pip-close", self.pip_handle(PipKey::Close)));
        ring
    }

    pub(super) fn open_pip_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pip_tab() == PipTab::Own {
            if self.screen.sharing.phase != Phase::Live {
                return;
            }
            // The publisher gear opens the existing live dialog, bound to the
            // confirmed request of the owner channel.
            self.pip_restore_focus = true;
            self.open_screen_settings(cx);
            return;
        }
        self.pip_settings_open = true;
        let key = if self.has_audio() {
            PipKey::Audio
        } else {
            PipKey::Fullscreen
        };
        let handle = self.pip_handle(key);
        window.defer(cx, move |window, _| window.focus(&handle));
        cx.notify();
    }

    /// Escape closes the top-most non-modal PiP surface. A real modal above
    /// the PiP is closed by the shell first.
    pub(crate) fn pip_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.pip_settings_open {
            return false;
        }
        self.pip_settings_open = false;
        self.pip_restore_focus = true;
        cx.notify();
        true
    }

    pub(crate) fn activate_focused_pip(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(key) = self
            .pip_ring()
            .into_iter()
            .find(|(_, _, handle)| handle.is_focused(window))
            .map(|(key, _, _)| key)
        else {
            return false;
        };
        match key {
            PipKey::View => self.switch_pip_tab(PipTab::View, cx),
            PipKey::Own => self.switch_pip_tab(PipTab::Own, cx),
            PipKey::Audio => self.toggle_stream_mute(cx),
            PipKey::Fullscreen => self.player_toggle_fullscreen(window),
            PipKey::Settings => self.open_pip_settings(window, cx),
            PipKey::Close => self.close_pip_role(window, cx),
            PipKey::Leave => {
                if self.player_fullscreen {
                    self.exit_fullscreen(window);
                }
                self.leave_stream(cx);
            }
        }
        true
    }

    /// Close the active role exactly once: the own broadcast sends one
    /// `stop`, the viewer one `leave`. Voice, audio and the other role stay.
    pub(super) fn close_pip_role(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pip_tab() == PipTab::Own {
            self.stop_screen_share(cx);
        } else {
            if self.player_fullscreen {
                self.exit_fullscreen(window);
            }
            self.leave_stream(cx);
        }
    }

    /// Drop the remembered PiP box only when both intents ended. Navigation
    /// between rooms keeps it for the next exit.
    pub(super) fn retain_pip_state(&mut self) {
        if self.pip_visible() {
            return;
        }
        self.pip_pos = None;
        self.pip_width = PIP_WIDTH;
        self.pip_tab = PipTab::View;
        self.pip_settings_open = false;
        self.pip_restore_focus = false;
    }
}

impl VoicePanel {
    /// Position of the compact box after a placement, drag or resize. The box
    /// itself clamps into the safe area, so a smaller window or a new DPI
    /// cannot leave a part of the PiP outside.
    pub(super) fn place_pip(&mut self) {
        if !self.pip_visible() {
            return;
        }
        let pip = voice_room::compact_stream_box(self.player_area, self.pip_pos, self.pip_width);
        // The stored width stays the requested one: a narrow area only clamps
        // the painted box and restores the compact width when it widens.
        self.pip_pos = Some((pip.x, pip.y));
        self.sync_viewport();
    }

    fn focus_restore_pending(&self) -> bool {
        self.pip_restore_focus && !self.screen.open
    }

    fn pip_settings_row(
        &self,
        key: PipKey,
        label: &'static str,
        active: bool,
        danger: bool,
        debug: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let color = if danger { c.danger } else { c.fg };
        let focus = self.pip_handle(key);
        div()
            .id(debug)
            .debug_selector(move || debug.into())
            .h(px(30.))
            .px(px(9.))
            .rounded(px(7.))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .cursor_pointer()
            .text_size(px(11.))
            .text_color(rgb(color))
            .bg(rgb(if active { c.avatar_bg } else { c.surface }))
            .track_focus(&focus)
            .on_click(cx.listener(move |panel, _, window, cx| match key {
                PipKey::Audio => panel.toggle_stream_mute(cx),
                PipKey::Fullscreen => panel.player_toggle_fullscreen(window),
                _ => {
                    if panel.player_fullscreen {
                        panel.exit_fullscreen(window);
                    }
                    panel.leave_stream(cx);
                }
            }))
            .child(div().truncate().child(label))
            .when(active, |row| {
                row.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(c.muted))
                        .child("Вкл"),
                )
            })
            .into_any_element()
    }

    fn pip_settings_panel(
        &mut self,
        width: f32,
        height: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let has_audio = self.has_audio();
        let rows = u32::from(has_audio) + 2;
        let panel_height = 12.0 + rows as f32 * 32.0;
        let panel_width = width.min(260.0);
        let mut panel = div()
            .id("pip-settings-panel")
            .debug_selector(|| "pip-settings-panel".into())
            .absolute()
            .left(px((width - panel_width - 12.0).max(4.)))
            .top(px((height - panel_height - 42.0).max(4.)))
            .w(px(panel_width))
            .h(px(panel_height))
            .rounded(px(10.))
            .bg(rgb(c.surface))
            .border_1()
            .border_color(rgb(c.line))
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(5.));
        if has_audio {
            let muted = self.stream_muted;
            panel = panel.child(self.pip_settings_row(
                PipKey::Audio,
                "Звук трансляции",
                !muted,
                false,
                "pip-settings-audio",
                cx,
            ));
        }
        panel = panel
            .child(self.pip_settings_row(
                PipKey::Fullscreen,
                "На весь экран",
                false,
                false,
                "pip-settings-fullscreen",
                cx,
            ))
            .child(self.pip_settings_row(
                PipKey::Leave,
                "Выйти из просмотра",
                false,
                true,
                "pip-settings-leave",
                cx,
            ));
        panel.into_any_element()
    }

    fn pip_action_button(
        &self,
        key: PipKey,
        label: &'static str,
        icon: &'static str,
        danger: bool,
        debug: &'static str,
        width: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let color = if danger { c.danger } else { c.muted };
        let focus = self.pip_handle(key);
        div()
            .id(debug)
            .debug_selector(move || debug.into())
            .h(px(28.))
            .w(px(width))
            .rounded(px(7.))
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .cursor_pointer()
            .text_size(px(10.))
            .text_color(rgb(color))
            .border_1()
            .border_color(rgb(if danger { c.danger } else { c.line }))
            .track_focus(&focus)
            .on_click(cx.listener(move |panel, _, window, cx| match key {
                PipKey::Settings => panel.open_pip_settings(window, cx),
                PipKey::Close => panel.close_pip_role(window, cx),
                _ => {
                    panel.activate_focused_pip(window, cx);
                }
            }))
            .child(super::view::vector_icon(icon, 13., rgb(color)))
            .child(label)
            .into_any_element()
    }

    /// The compact PiP. It sits above the channel content and below modals;
    /// the shell places it in that layer. `None` when both roles are in their
    /// own surfaces.
    pub fn pip_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let pip = self.pip_box()?;
        if self.player_fullscreen {
            return None;
        }
        let c = self.colors();
        // A settings dialog of the publisher owns the keyboard while open;
        // once it closes, the gear of a still-live PiP takes focus back.
        if self.focus_restore_pending() {
            self.pip_restore_focus = false;
            let handle = self.pip_handle(PipKey::Settings);
            window.defer(cx, move |window, _| window.focus(&handle));
        }
        let local = |rect: Rect| Rect::new(rect.x - pip.x, rect.y - pip.y, rect.w, rect.h);
        let view_tab = self.pip_tab() == PipTab::View;
        let has_audio = view_tab && self.has_audio();
        let geometry = voice_room::pip_player_geometry(self.player_area, pip, has_audio);
        let header = local(geometry.header);
        let picture = local(geometry.picture);
        let corner = local(geometry.corner);
        let fullscreen = local(geometry.fullscreen);
        let settings = local(geometry.settings);
        let close = local(geometry.close);
        let audio = geometry.audio.map(local);
        let title = self.pip_title();
        let status = self.pip_status();
        let frame = self.pip_frame();

        let mut picture_div = div()
            .id("pip-picture")
            .debug_selector(|| "stream-pip-picture".into())
            .absolute()
            .left(px(picture.x))
            .top(px(picture.y))
            .w(px(picture.w))
            .h(px(picture.h))
            .overflow_hidden()
            .bg(rgb(0x080808))
            .on_click(cx.listener(|panel, event: &gpui::ClickEvent, window, _| {
                if event.click_count() == 2 && panel.pip_tab() == PipTab::View {
                    panel.player_double_click(window);
                }
            }));
        if let Some(image) = frame {
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
                    .top(px((picture.h - 30.) / 2.))
                    .w(px((picture.w - 12.).max(0.)))
                    .h(px(30.))
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
        let live = self.pip_tab() == PipTab::Own
            && matches!(
                self.screen.sharing.phase,
                Phase::Publishing | Phase::Live | Phase::Stopping
            );
        let mut card = div()
            .id("stream-pip")
            .debug_selector(|| "stream-pip".into())
            .key_context("VoiceRoom")
            .absolute()
            .left(px(pip.x))
            .top(px(pip.y))
            .w(px(pip.w))
            .h(px(pip.h))
            .rounded(px(12.))
            .bg(rgb(c.surface))
            .border_1()
            .border_color(rgb(if live { c.danger } else { c.line }))
            .child(picture_div)
            .child(
                div()
                    .id("pip-header")
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
                    .child(img("icons/grip.svg").w(px(11.)).h(px(13.)))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .truncate()
                            .child(title),
                    ),
            )
            .child(
                div()
                    .id("pip-fullscreen")
                    .absolute()
                    .left(px(fullscreen.x))
                    .top(px(fullscreen.y))
                    .w(px(fullscreen.w))
                    .h(px(fullscreen.h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(!view_tab, |button| button.opacity(0.25))
                    .track_focus(&self.pip_handle(PipKey::Fullscreen))
                    .on_click(cx.listener(|panel, _, window, _| {
                        if panel.pip_tab() == PipTab::View {
                            panel.player_toggle_fullscreen(window);
                        }
                    }))
                    .child(img("icons/maximize.svg").w(px(13.)).h(px(13.))),
            )
            .child(
                div()
                    .id("pip-settings")
                    .debug_selector(|| "pip-settings".into())
                    .absolute()
                    .left(px(settings.x))
                    .top(px(settings.y))
                    .w(px(settings.w))
                    .h(px(settings.h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&self.pip_handle(PipKey::Settings))
                    .on_click(
                        cx.listener(|panel, _, window, cx| panel.open_pip_settings(window, cx)),
                    )
                    .child(img("icons/settings.svg").w(px(14.)).h(px(14.))),
            )
            .child(
                div()
                    .id("pip-close")
                    .debug_selector(|| "pip-close".into())
                    .absolute()
                    .left(px(close.x))
                    .top(px(close.y))
                    .w(px(close.w))
                    .h(px(close.h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&self.pip_handle(PipKey::Close))
                    .on_click(cx.listener(|panel, _, window, cx| panel.close_pip_role(window, cx)))
                    .child(img("icons/close.svg").w(px(12.)).h(px(12.))),
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
            let focus = self.pip_handle(PipKey::Audio);
            card = card.child(
                div()
                    .id("stream-audio")
                    .debug_selector(|| "pip-audio".into())
                    .absolute()
                    .left(px(audio.x))
                    .top(px(audio.y))
                    .w(px(audio.w))
                    .h(px(audio.h))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&focus)
                    .child(
                        img(if self.stream_muted {
                            "icons/headphones-off.svg"
                        } else {
                            "icons/volume.svg"
                        })
                        .w(px(14.))
                        .h(px(14.)),
                    )
                    .on_click(cx.listener(|panel, _, _, cx| panel.toggle_stream_mute(cx))),
            );
        }
        // Footer: tabs of both roles, then the two labelled actions of the
        // active role. Labels are visible, so the compact surface stays
        // understandable without hover.
        let footer_top = pip.h - 34.0;
        let mut footer = div()
            .id("pip-footer")
            .absolute()
            .left(px(8.))
            .top(px(footer_top))
            .w(px((pip.w - 16.).max(1.)))
            .h(px(28.))
            .flex()
            .items_center()
            .gap_1();
        if self.pip_both() {
            let view_focus = self.pip_handle(PipKey::View);
            let own_focus = self.pip_handle(PipKey::Own);
            for (label, tab, focus, debug) in [
                ("Просмотр", PipTab::View, view_focus, "pip-tab-view"),
                ("Ваш экран", PipTab::Own, own_focus, "pip-tab-own"),
            ] {
                let active = self.pip_tab() == tab;
                footer = footer.child(
                    div()
                        .id(debug)
                        .debug_selector(move || debug.into())
                        .h(px(26.))
                        .px(px(6.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .text_size(px(10.))
                        .text_color(rgb(if active { c.fg } else { c.muted }))
                        .bg(rgb(if active { c.avatar_bg } else { c.surface }))
                        .border_1()
                        .border_color(rgb(if active { c.accent_text } else { c.line }))
                        .track_focus(&focus)
                        .on_click(cx.listener(move |panel, _, _, cx| panel.switch_pip_tab(tab, cx)))
                        .child(label),
                );
            }
        }
        footer = footer.child(div().flex_1());
        footer = footer.child(self.pip_action_button(
            PipKey::Settings,
            "Настройки",
            "icons/settings.svg",
            false,
            "pip-footer-settings",
            66.,
            cx,
        ));
        let (label, danger) = if self.pip_tab() == PipTab::Own {
            ("Завершить", true)
        } else {
            ("Выйти", true)
        };
        footer = footer.child(self.pip_action_button(
            PipKey::Close,
            label,
            "icons/close.svg",
            danger,
            "pip-footer-close",
            52.,
            cx,
        ));
        card = card.child(footer);
        if self.pip_settings_open && view_tab {
            card = card.child(self.pip_settings_panel(pip.w, pip.h, cx));
        }
        Some(card.into_any_element())
    }
    /// Other publishers of one channel. The call runtime keeps the source
    /// room's list even while another room is shown, so the owner slot is
    /// always counted from the room that really holds the publication.
    pub(super) fn publishers_in(&self, channel: i64) -> usize {
        let list: &[Publication] = if self.call_channel != 0 && self.call_channel == channel {
            &self.streams
        } else if self.room == channel {
            &self.room_streams
        } else {
            return 0;
        };
        list.iter()
            .filter(|publication| {
                publication.channel_id == channel
                    && Some(&publication.publisher_connection_id) != self.self_connection.as_ref()
            })
            .count()
    }
}
