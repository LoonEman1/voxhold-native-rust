//! Screen-sharing UI: the room toolbar action, the source picker and the
//! on-air card. All decisions belong to the media runtime; this file renders
//! its state and forwards explicit user intents.
//!
//! Only settings that the SDR publication really applies are shown: 720p or
//! 1080p, 30 or 60 FPS, 0,5–16 Мбит/с in 0,1 steps and the confirmed codec.
//! P2P, HDR and system audio belong to later tasks and have no widgets here.

use super::*;
use voxhold_media::publisher::{
    Codec, Phase, PublishRequest, Quality, Resolution, Sharing, MAX_CHANNEL_PUBLISHERS,
};
use voxhold_media::screen_capture::{self, ScreenSource, SourceKind};

/// The account socket always authenticates with `multi_streams: true`
/// (`crates/voxhold-api/src/lib.rs:638`), so the client capability is known.
/// It still does not allow publishing more than one stream in this stage.
pub(crate) const MULTI_STREAMS_SUPPORTED: bool = true;

/// One focusable action inside the picker. `Enter`/`Space` on a focused
/// element runs it, exactly like the settings modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShareKey {
    Source(usize),
    Monitors,
    Windows,
    Refresh,
    Resolution(Resolution),
    Fps(u32),
    BitrateDown,
    BitrateUp,
    Cancel,
    Start,
}

pub(super) struct ScreenShareUi {
    pub(super) open: bool,
    pub(super) picker: u64,
    pub(super) kind: SourceKind,
    pub(super) monitors: Vec<ScreenSource>,
    pub(super) windows: Vec<ScreenSource>,
    pub(super) monitor_generation: u64,
    pub(super) window_generation: u64,
    pub(super) monitor_loading: bool,
    pub(super) window_loading: bool,
    pub(super) monitor_error: String,
    pub(super) window_error: String,
    pub(super) selected: Option<ScreenSource>,
    pub(super) resolution: Resolution,
    pub(super) fps: u32,
    pub(super) bitrate_kbps: u32,
    pub(super) busy: bool,
    pub(super) error: String,
    pub(super) sharing: Sharing,
    pub(super) preview: Option<Arc<RenderImage>>,
    pub(super) ring: Vec<(FocusHandle, ShareKey)>,
    /// Set once the dialog focused its first ring element. The flag is reset
    /// with the dialog, so a repaint never steals focus back from the user.
    pub(super) focus_placed: bool,
}

impl Default for ScreenShareUi {
    fn default() -> Self {
        Self {
            open: false,
            picker: 0,
            kind: SourceKind::Monitor,
            monitors: Vec::new(),
            windows: Vec::new(),
            monitor_generation: 0,
            window_generation: 0,
            monitor_loading: false,
            window_loading: false,
            monitor_error: String::new(),
            window_error: String::new(),
            selected: None,
            resolution: Resolution::P720,
            fps: 30,
            bitrate_kbps: 4000,
            busy: false,
            error: String::new(),
            sharing: Sharing::default(),
            preview: None,
            ring: Vec::new(),
            focus_placed: false,
        }
    }
}

impl ScreenShareUi {
    fn sources(&self) -> &[ScreenSource] {
        match self.kind {
            SourceKind::Monitor => &self.monitors,
            SourceKind::Window => &self.windows,
        }
    }

    fn loading(&self) -> bool {
        match self.kind {
            SourceKind::Monitor => self.monitor_loading,
            SourceKind::Window => self.window_loading,
        }
    }

    fn error(&self) -> &str {
        match self.kind {
            SourceKind::Monitor => &self.monitor_error,
            SourceKind::Window => &self.window_error,
        }
    }

    fn quality(&self) -> Quality {
        Quality {
            resolution: self.resolution,
            fps: self.fps,
            bitrate_kbps: self.bitrate_kbps,
        }
    }

    /// The settings that a live attempt applies. Shown on the on-air card.
    fn quality_label(&self) -> String {
        self.sharing
            .quality
            .unwrap_or_else(|| self.quality())
            .describe()
    }
}

impl VoicePanel {
    pub fn set_stream_permission(&mut self, allowed: bool) {
        self.can_stream = allowed;
    }

    pub fn screen_share_open(&self) -> bool {
        self.screen.open
    }

    /// Whether a Start was pressed and the worker has not answered yet.
    #[cfg(test)]
    pub fn screen_share_busy(&self) -> bool {
        self.screen.busy
    }

    /// Whether this client currently owns a publication attempt. In this
    /// phase the «Экран» action shows a status instead of a disabled reason.
    pub(super) fn share_active(&self) -> bool {
        matches!(
            self.screen.sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping
        )
    }

    /// Why the «Экран» action is disabled, shown next to the toolbar. During
    /// an active share the reason would be the permanent "already running"
    /// text, so the room shows the publication state instead.
    pub(super) fn screen_reason(&self) -> Option<&'static str> {
        if self.share_active() {
            return None;
        }
        self.share_gate().err()
    }

    fn active_publishers_in_room(&self) -> usize {
        self.room_streams()
            .iter()
            .filter(|publication| {
                Some(&publication.publisher_connection_id) != self.self_connection.as_ref()
            })
            .count()
    }

    /// Why the «Экран» action is disabled. The same values are re-checked by
    /// the publisher state machine.
    pub fn share_gate(&self) -> Result<(), &'static str> {
        #[cfg(test)]
        if let Some(result) = self.test_share_gate {
            return result;
        }
        if !MULTI_STREAMS_SUPPORTED {
            return Err("Клиент не поддерживает несколько трансляций.");
        }
        if !self.access_confirmed {
            return Err("Права на канал ещё не подтверждены.");
        }
        match self.rooms.get(&self.room) {
            Some(access) if access.readable && !access.e2ee => {}
            _ => return Err("Нет доступа к голосовому каналу."),
        }
        if self.call_channel == 0 || self.call_channel != self.room {
            return Err("Сначала подключитесь к голосовому каналу.");
        }
        if self.phase != CallPhase::Connected {
            return Err("Голосовое соединение ещё не установлено.");
        }
        if !self.can_stream {
            return Err("Нет права запускать трансляции в этом канале.");
        }
        if screen_capture::capability_cached().is_err() {
            return Err("Захват экрана недоступен на этом устройстве.");
        }
        if Codec::available().is_empty() {
            return Err("В этой сборке нет подтверждённого кодека трансляции.");
        }
        if matches!(
            self.screen.sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping
        ) {
            return Err("Трансляция уже идёт.");
        }
        if self.active_publishers_in_room() >= MAX_CHANNEL_PUBLISHERS {
            return Err("В канале уже 8 трансляций. Дождитесь завершения одной из них.");
        }
        Ok(())
    }

    pub(super) fn open_screen_share(&mut self, cx: &mut Context<Self>) {
        if self.share_gate().is_err() {
            return;
        }
        self.release_push_to_talk();
        self.screen.open = true;
        self.screen.busy = false;
        self.screen.error.clear();
        self.screen.selected = None;
        self.screen.kind = SourceKind::Monitor;
        self.screen.focus_placed = false;
        if let Some(handle) = &self.handle {
            self.screen.picker = handle.begin_screen_picker();
        }
        self.request_screen_sources(cx);
        cx.notify();
    }

    /// Close the picker. Before Start this only cancels the picker and leaves
    /// the viewer alone. After Start (Escape, backdrop or a repeated close)
    /// it is a stop intent: the attempt already committed, so the capture is
    /// released and exactly one `stream.stop` is sent.
    pub(super) fn close_screen_share(&mut self, cx: &mut Context<Self>) {
        if !self.screen.open {
            return;
        }
        let committed = self.screen.busy || self.share_active();
        self.screen.open = false;
        self.screen.busy = false;
        self.screen.error.clear();
        self.screen.selected = None;
        self.screen.focus_placed = false;
        if let Some(handle) = &self.handle {
            if committed {
                handle.stop_screen_share();
            } else {
                handle.cancel_screen_picker(self.screen.picker);
            }
        }
        cx.notify();
    }

    fn request_screen_sources(&mut self, _cx: &mut Context<Self>) {
        let Some(handle) = &self.handle else {
            return;
        };
        self.screen.monitor_loading = true;
        self.screen.window_loading = true;
        self.screen.monitor_generation = handle.screen_sources(SourceKind::Monitor);
        self.screen.window_generation = handle.screen_sources(SourceKind::Window);
    }

    fn refresh_screen_sources(&mut self, cx: &mut Context<Self>) {
        match self.screen.kind {
            SourceKind::Monitor => {
                if let Some(handle) = &self.handle {
                    self.screen.monitor_generation = handle.screen_sources(SourceKind::Monitor);
                    self.screen.monitor_loading = true;
                    self.screen.monitor_error.clear();
                }
            }
            SourceKind::Window => {
                if let Some(handle) = &self.handle {
                    self.screen.window_generation = handle.screen_sources(SourceKind::Window);
                    self.screen.window_loading = true;
                    self.screen.window_error.clear();
                }
            }
        }
        cx.notify();
    }

    fn select_screen_source(&mut self, source: ScreenSource, cx: &mut Context<Self>) {
        self.screen.selected = Some(source);
        cx.notify();
    }

    fn start_screen_share(&mut self, cx: &mut Context<Self>) {
        if self.screen.busy {
            return;
        }
        if let Err(reason) = self.share_gate() {
            // An active publication is not an error for this button; its card
            // owns the status and the stop action.
            if !self.share_active() {
                self.screen.error = reason.to_owned();
            }
            cx.notify();
            return;
        }
        let Some(source) = self.screen.selected.clone() else {
            self.screen.error = "Выберите монитор или окно.".into();
            cx.notify();
            return;
        };
        let request = PublishRequest {
            source,
            quality: self.screen.quality(),
            codec: Codec::Vp8,
        };
        let Some(handle) = &self.handle else {
            self.screen.error = "Голосовое соединение уже закрыто.".into();
            cx.notify();
            return;
        };
        handle.publish_screen(
            self.screen.picker,
            request,
            self.can_stream,
            self.active_publishers_in_room() < MAX_CHANNEL_PUBLISHERS,
        );
        self.screen.busy = true;
        self.screen.error.clear();
        cx.notify();
    }

    fn stop_screen_share(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = &self.handle {
            handle.stop_screen_share();
        }
        cx.notify();
    }

    fn change_bitrate(&mut self, delta: i64, cx: &mut Context<Self>) {
        let next = (self.screen.bitrate_kbps as i64 + delta).clamp(500, 16_000);
        self.screen.bitrate_kbps = ((next / 100) * 100) as u32;
        cx.notify();
    }

    /// Apply one runtime update. Returns whether the panel must repaint.
    pub(super) fn apply_screen_update(&mut self, update: VoiceUpdate) -> bool {
        match update {
            VoiceUpdate::ScreenShare(sharing) => {
                let previous = self.screen.sharing.phase;
                self.screen.sharing = sharing;
                let phase = self.screen.sharing.phase;
                if matches!(phase, Phase::Publishing | Phase::Live) && self.screen.open {
                    // The attempt is accepted by the worker; the card in the
                    // room now owns the status and the stop action.
                    self.screen.open = false;
                    self.screen.busy = false;
                    self.screen.focus_placed = false;
                }
                if phase == Phase::Failed {
                    self.screen.busy = false;
                    self.screen.focus_placed = false;
                    self.screen.error = self.screen.sharing.error.clone().unwrap_or_default();
                    // A failed attempt has no picker any more; reopening the
                    // dialog calls `begin_picker` again.
                    if self.screen.open {
                        self.screen.open = false;
                    }
                }
                if phase == Phase::Idle {
                    if self.screen.open {
                        // The session ended (or a new one replaced the worker)
                        // while the dialog was open; its picker id is stale.
                        self.screen.open = false;
                        self.screen.busy = false;
                        self.screen.focus_placed = false;
                        if previous != Phase::Idle {
                            self.screen.error =
                                "Соединение было восстановлено, выберите экран снова.".into();
                        }
                    }
                }
                if phase == Phase::Idle && previous != Phase::Idle {
                    self.screen.preview = None;
                }
                true
            }
            VoiceUpdate::ScreenSources {
                generation,
                kind,
                sources,
                error,
            } => {
                match kind {
                    SourceKind::Monitor if generation >= self.screen.monitor_generation => {
                        self.screen.monitor_generation = generation;
                        self.screen.monitor_loading = false;
                        self.screen.monitor_error = error.unwrap_or_default();
                        if self
                            .screen
                            .selected
                            .as_ref()
                            .is_some_and(|selected| selected.kind == SourceKind::Monitor)
                            && !sources
                                .iter()
                                .any(|item| Some(item) == self.screen.selected.as_ref())
                        {
                            self.screen.selected = None;
                        }
                        // The non-active kind still refreshes its cache; the
                        // picker reads it when the user switches tabs.
                        self.screen.monitors = sources;
                    }
                    SourceKind::Window if generation >= self.screen.window_generation => {
                        self.screen.window_generation = generation;
                        self.screen.window_loading = false;
                        self.screen.window_error = error.unwrap_or_default();
                        if self
                            .screen
                            .selected
                            .as_ref()
                            .is_some_and(|selected| selected.kind == SourceKind::Window)
                            && !sources
                                .iter()
                                .any(|item| Some(item) == self.screen.selected.as_ref())
                        {
                            self.screen.selected = None;
                        }
                        self.screen.windows = sources;
                    }
                    _ => return false,
                }
                true
            }
            _ => false,
        }
    }

    /// Take the newest local preview for the on-air card.
    pub(super) fn take_screen_preview(&mut self) -> bool {
        let Some(frame) = self
            .handle
            .as_ref()
            .and_then(|handle| handle.take_screen_frame())
        else {
            return false;
        };
        let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra) else {
            return false;
        };
        self.screen.preview = Some(Arc::new(RenderImage::new(smallvec::smallvec![
            image::Frame::new(buffer)
        ])));
        true
    }

    /// Forget every screen-share surface: leaving the call or a revoked
    /// session must not keep a card, a picker or a stale preview.
    pub(super) fn reset_screen(&mut self) {
        self.screen = ScreenShareUi::default();
    }

    fn share_focus(&self, index: usize) -> FocusHandle {
        // A ring larger than the handle range reuses the last handle; the
        // visible rows stay reachable and no index overlaps the toolbar.
        self.picker_focus[index.min(self.picker_focus.len() - 1)].clone()
    }
}

impl VoicePanel {
    /// The «Экран» button inside the room toolbar. The reason of a disabled
    /// state is rendered next to the toolbar by the room body.
    pub(super) fn screen_share_button(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let c = self.colors();
        let enabled = self.share_gate().is_ok();
        let active = matches!(
            self.screen.sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping
        );
        let color = if active {
            c.danger
        } else if enabled {
            c.muted
        } else {
            c.line
        };
        div()
            .id("voice-screen")
            .debug_selector(|| "voice-screen".into())
            .h_full()
            .w(px(44.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(10.))
            .bg(rgb(if active { c.danger_bg } else { c.surface }))
            .border_1()
            .border_color(rgb(if active { c.danger } else { c.line }))
            .text_color(rgb(color))
            .when(enabled, |button| button.cursor_pointer())
            .track_focus(&self.screen_focus[1])
            .on_click(cx.listener(|panel, _, _, cx| {
                if panel.share_gate().is_ok() {
                    panel.open_screen_share(cx);
                }
            }))
            .child(img("icons/monitor.svg").w(px(16.)).h(px(16.)))
            .into_any_element()
    }

    /// Whether the local on-air card takes one room slot.
    pub(super) fn screen_card_active(&self) -> bool {
        matches!(
            self.screen.sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping | Phase::Failed
        )
    }

    /// The card that replaces the local publication in the room grid while a
    /// screen is on air. `None` when nothing is being published.
    pub(super) fn screen_card(
        &mut self,
        rect: Rect,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let sharing = self.screen.sharing.clone();
        if !matches!(
            sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping | Phase::Failed
        ) {
            return None;
        }
        let c = self.colors();
        let live = sharing.phase == Phase::Live;
        let status = if live {
            "В ЭФИРЕ".to_owned()
        } else if sharing.phase == Phase::Failed {
            "ОШИБКА".to_owned()
        } else if sharing.phase == Phase::Stopping {
            "ОСТАНОВКА…".to_owned()
        } else {
            sharing.status.clone()
        };
        let stats = sharing.stats.map(|stats| {
            // FPS, drop, real resolution, limitation and source count come
            // from OutboundRtpStats; this build has no GPU/CPU percentage.
            stats.describe()
        });
        let quality = self.screen.quality_label();
        let mut card = div()
            .id("screen-on-air")
            .debug_selector(|| "screen-on-air".into())
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(12.))
            .bg(rgb(c.surface))
            .border_1()
            .border_color(rgb(if live { c.danger } else { c.line }))
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top(px(12.))
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(5.))
                    .bg(rgb(c.live_bg))
                    .text_size(px(10.))
                    .text_color(rgb(c.danger))
                    .child(if live { "LIVE" } else { "ВАШ ЭКРАН" }),
            )
            .child(
                div()
                    .absolute()
                    .right(px(12.))
                    .top(px(12.))
                    .text_size(px(10.))
                    .text_color(rgb(c.muted))
                    .truncate()
                    .child(status),
            )
            .child(
                div()
                    .absolute()
                    .left(px(10.))
                    .top(px(34.))
                    .w(px((rect.w - 20.).max(1.)))
                    .h(px((rect.h - 108.).max(1.)))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .bg(rgb(0x080808))
                    .when_some(self.screen.preview.clone(), |picture, image| {
                        picture.child(
                            img(image)
                                .w_full()
                                .h_full()
                                .object_fit(gpui::ObjectFit::Contain),
                        )
                    }),
            )
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top(px(rect.h - 66.))
                    .w(px((rect.w - 24.).max(0.)))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .truncate()
                    .child("Ваш экран"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top(px(rect.h - 50.))
                    .w(px((rect.w - 24.).max(0.)))
                    .text_size(px(10.))
                    .text_color(rgb(c.muted))
                    .truncate()
                    .child(match stats {
                        Some(stats) => format!("{quality} · {stats}"),
                        None => quality.clone(),
                    }),
            );
        if sharing.phase != Phase::Stopping {
            let focus = self.screen_focus[0].clone();
            card = card.child(
                div()
                    .id("screen-stop")
                    .absolute()
                    .left(px(10.))
                    .top(px(rect.h - 36.))
                    .w(px((rect.w - 20.).max(0.)))
                    .h(px(28.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(c.danger))
                    .text_color(rgb(c.danger))
                    .text_size(px(11.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&focus)
                    .on_click(cx.listener(|panel, _, _, cx| panel.stop_screen_share(cx)))
                    .child("Завершить трансляцию"),
            );
        }
        Some(card.into_any_element())
    }
}

mod dialog;
