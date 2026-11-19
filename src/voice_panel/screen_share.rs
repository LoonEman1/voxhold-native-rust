//! Screen-sharing UI: the room toolbar action, the source picker and the
//! on-air card. All decisions belong to the media runtime; this file renders
//! its state and forwards explicit user intents.
//!
//! Only settings that the SDR publication really applies are shown: the four
//! web presets, 720p or 1080p, 30/60 FPS, 0,5–16 Мбит/с, the six Opus audio
//! bitrates and the confirmed codec. P2P, HDR and the web «Содержание»
//! (degradation preference) are absent because the pinned engine or backend
//! cannot apply them.

use super::*;
use voxhold_media::publisher::{
    Codec, Phase, Preset, PublishRequest, Quality, Resolution, Sharing, AUDIO_BITRATES,
    MAX_CHANNEL_PUBLISHERS,
};
use voxhold_media::screen_capture::{self, ScreenSource, SourceId, SourceKind};
use voxhold_media::system_audio::{self, AudioDeviceInfo};

/// Windows loopback sees every render session of the endpoint, including this
/// application's own room and player. State it instead of promising otherwise.
pub(crate) const SYSTEM_AUDIO_WARNING: &str =
    "Системный звук записывается через loopback всего устройства вывода: в него попадут голос комнаты и плеер Voxhold. Windows не позволяет исключить звук отдельного приложения.";
/// The backend has no runtime reconfigure; the dialog must warn honestly.
pub(crate) const RESTART_WARNING: &str =
    "Изменение качества перезапустит трансляцию: текущая сессия будет остановлена, зрители увидят новое подключение.";
/// A Start pressed before the endpoint probe answered; cleared by the answer.
pub(crate) const AUDIO_PENDING_REASON: &str = "Дождитесь проверки системного звука.";
/// The selected identity is gone from the current snapshot; the publish path
/// repeats this check, so a stale dialog never sends a request.
pub(crate) const SOURCE_GONE_REASON: &str =
    "Выбранный источник больше недоступен. Обновите список и выберите источник снова.";

/// The account socket always authenticates with `multi_streams: true`
/// (`crates/voxhold-api/src/lib.rs:638`), so the client capability is known.
/// It still does not allow publishing more than one stream in this stage.
pub(crate) const MULTI_STREAMS_SUPPORTED: bool = true;

/// One focusable action inside the picker. `Enter`/`Space` on a focused
/// element runs it, exactly like the settings modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShareKey {
    /// A picker row, bound to the stable `kind + capturer id`, never a position.
    Source(SourceId),
    Monitors,
    Windows,
    Refresh,
    Resolution(Resolution),
    Fps(u32),
    BitrateDown,
    BitrateUp,
    Preset(Preset),
    Audio,
    AudioBitrateDown,
    AudioBitrateUp,
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
    /// Stable identity of the chosen source, resolved from the current snapshot.
    pub(super) selected: Option<SourceId>,
    pub(super) resolution: Resolution,
    pub(super) fps: u32,
    pub(super) bitrate_kbps: u32,
    pub(super) audio_enabled: bool,
    pub(super) audio_bitrate_kbps: u32,
    pub(super) audio_availability: Option<Result<AudioDeviceInfo, String>>,
    /// The dialog was opened from the live card to change quality. Start then
    /// becomes a confirmed stop+start, never a live reconfigure.
    pub(super) restart: bool,
    /// The request of the accepted attempt. A restart prefills from it.
    pub(super) last_request: Option<PublishRequest>,
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
            resolution: Resolution::P1080,
            fps: 30,
            bitrate_kbps: 6000,
            audio_enabled: true,
            audio_bitrate_kbps: 128,
            audio_availability: None,
            restart: false,
            last_request: None,
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
        self.snapshot(self.kind)
    }

    fn snapshot(&self, kind: SourceKind) -> &[ScreenSource] {
        match kind {
            SourceKind::Monitor => &self.monitors,
            SourceKind::Window => &self.windows,
        }
    }

    /// Resolve an identity in its snapshot; a duplicate id resolves to `None`.
    pub(super) fn source_for(&self, id: SourceId) -> Option<&ScreenSource> {
        let mut found = self
            .snapshot(id.kind)
            .iter()
            .filter(|item| item.id == id.id);
        let source = found.next()?;
        found.next().is_none().then_some(source)
    }

    /// The selected source as the current snapshot describes it.
    pub(super) fn selected_source(&self) -> Option<&ScreenSource> {
        self.selected.and_then(|id| self.source_for(id))
    }

    /// Whether the selection is in the visible category and still present.
    pub(super) fn selection_is_current(&self) -> bool {
        matches!(self.selected, Some(id) if id.kind == self.kind && self.source_for(id).is_some())
    }

    /// The confirmed live source of a restart dialog. It comes from the
    /// accepted request only; enumeration answers never define or inject it.
    pub(super) fn restart_source(&self) -> Option<&ScreenSource> {
        if !self.restart {
            return None;
        }
        self.last_request.as_ref().map(|request| &request.source)
    }

    /// Whether the restart still points at the identity the engine accepted.
    /// The snapshot is irrelevant: the live source may be absent from it.
    pub(super) fn restart_is_confirmed(&self) -> bool {
        self.restart_source()
            .is_some_and(|source| self.selected == Some(source.source_id()))
    }

    /// Drop a selection the newest snapshot no longer contains. A restart is
    /// bound to the accepted source, so a background answer never clears it.
    fn retain_selection(&mut self, kind: SourceKind, sources: &[ScreenSource]) {
        if self.restart {
            return;
        }
        let Some(id) = self.selected else { return };
        if id.kind != kind {
            return;
        }
        if sources.iter().filter(|item| item.id == id.id).count() != 1 {
            self.selected = None;
            self.error = SOURCE_GONE_REASON.into();
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

    pub(super) fn quality(&self) -> Quality {
        Quality {
            resolution: self.resolution,
            fps: self.fps,
            bitrate_kbps: self.bitrate_kbps,
            audio_bitrate_kbps: self.audio_bitrate_kbps,
        }
    }

    /// The settings that a live attempt applies. Shown on the on-air card.
    fn quality_label(&self) -> String {
        self.sharing
            .quality
            .unwrap_or_else(|| self.quality())
            .describe()
    }

    pub(super) fn audio_available(&self) -> bool {
        matches!(self.audio_availability, Some(Ok(_)))
    }

    /// Availability and the real mix format, shown before the start.
    pub(super) fn audio_status(&self) -> String {
        match &self.audio_availability {
            None => "Проверяем устройство вывода…".into(),
            Some(Ok(device)) => format!("{} · {}", device.label, device.format),
            Some(Err(error)) => format!("Системный звук недоступен: {error}"),
        }
    }

    /// Whether the publication will really request the audio track.
    pub(super) fn audio_requested(&self) -> bool {
        self.audio_enabled && self.audio_available()
    }

    /// The toggle may claim «Включён» only for the track that will really be
    /// published; a pending probe has not confirmed the endpoint yet.
    pub(super) fn audio_toggle_label(&self) -> &'static str {
        if self.audio_requested() {
            "Включён"
        } else {
            "Выключен"
        }
    }

    /// The honest on-air sound line. The measured rate comes from the RTP
    /// stats; before the first sample only the configured maximum is shown.
    pub(super) fn sound_label(&self) -> String {
        if !self.sharing.has_audio {
            return "Без звука".into();
        }
        let max = self
            .sharing
            .quality
            .map(|quality| quality.audio_bitrate_kbps)
            .unwrap_or(self.audio_bitrate_kbps);
        match self.sharing.stats {
            Some(stats) if stats.audio_kbps > 0.0 => {
                format!("Звук ~{:.0} из до {} кбит/с", stats.audio_kbps, max)
            }
            _ => format!("Звук до {max} кбит/с"),
        }
    }

    pub(super) fn restart_warning(&self) -> Option<&'static str> {
        self.restart.then_some(RESTART_WARNING)
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
        self.screen.restart = false;
        self.screen.busy = false;
        self.screen.error.clear();
        self.screen.selected = None;
        self.screen.last_request = None;
        self.screen.kind = SourceKind::Monitor;
        // A new picker starts with no rows: an identity from the previous
        // answer (or a confirmed live source) must not be selectable while
        // the fresh lists load.
        self.screen.monitors.clear();
        self.screen.windows.clear();
        self.screen.monitor_error.clear();
        self.screen.window_error.clear();
        // A fresh publication defaults to system audio; the background probe
        // fills in availability and disables the toggle if the endpoint fails.
        // The previous picker's answer must not leak into this one: Start
        // stays locked until the new probe settles.
        self.screen.audio_enabled = true;
        self.screen.audio_availability = None;
        self.begin_audio_probe(cx);
        self.screen.focus_placed = false;
        if let Some(handle) = &self.handle {
            self.screen.picker = handle.begin_screen_picker();
        }
        self.request_screen_sources(cx);
        cx.notify();
    }

    /// Open the quality dialog from the live card. The source is locked; the
    /// backend only applies the change through a stop and a new start.
    pub(super) fn open_screen_settings(&mut self, cx: &mut Context<Self>) {
        if self.screen.sharing.phase != Phase::Live {
            return;
        }
        self.release_push_to_talk();
        let sharing = self.screen.sharing.clone();
        if let Some(quality) = sharing.quality {
            self.screen.resolution = quality.resolution;
            self.screen.fps = quality.fps;
            self.screen.bitrate_kbps = quality.bitrate_kbps;
            self.screen.audio_bitrate_kbps = quality.audio_bitrate_kbps;
        }
        self.screen.audio_enabled = sharing.has_audio;
        self.begin_audio_probe(cx);
        self.screen.restart = true;
        // The restart dialog is bound to the confirmed live source here, and
        // only here: the identity stays in `last_request`, never in the
        // enumeration caches, so the current snapshot may be empty or stale.
        self.screen.bind_confirmed_live_source();
        self.screen.open = true;
        self.screen.busy = false;
        self.screen.error.clear();
        self.screen.focus_placed = false;
        cx.notify();
    }

    /// Probe the render endpoint on the background executor. COM plus a
    /// driver call can take up to two seconds, so the click must only start a
    /// task and repaint; the result lands when it still matches the newest
    /// request. A device failure disables the audio toggle, a success leaves
    /// the user's choice alone.
    fn begin_audio_probe(&mut self, cx: &mut Context<Self>) {
        self.screen_audio_probe = self.screen_audio_probe.wrapping_add(1);
        let generation = self.screen_audio_probe;
        #[cfg(test)]
        let injected = self.test_audio_probe.take();
        let task = cx.background_spawn(async move {
            #[cfg(test)]
            if let Some(probe) = injected {
                return probe();
            }
            system_audio::probe()
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |panel, cx| {
                if panel.screen_audio_probe != generation {
                    return;
                }
                if result.is_err() {
                    panel.screen.audio_enabled = false;
                }
                panel.screen.audio_availability = Some(result);
                // The blocked-Start reason is no longer true; any other error
                // stays until the next intent.
                if panel.screen.error == AUDIO_PENDING_REASON {
                    panel.screen.error.clear();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Close the picker. Before Start this only cancels the picker and leaves
    /// the viewer alone. After Start (Escape, backdrop or a repeated close)
    /// it is a stop intent: the attempt already committed, so the capture is
    /// released and exactly one `stream.stop` is sent.
    pub(super) fn close_screen_share(&mut self, cx: &mut Context<Self>) {
        if !self.screen.open {
            return;
        }
        let restart_open = self.screen.restart;
        // The live publication existed before this dialog; closing it without
        // pressing Restart must leave the stream untouched.
        let committed = if restart_open {
            self.screen.busy
        } else {
            self.screen.busy || self.share_active()
        };
        self.screen.open = false;
        self.screen.restart = false;
        self.screen.busy = false;
        self.screen.error.clear();
        self.screen.selected = None;
        // No row of this dialog may survive into the next picker. The
        // confirmed live identity stays in `last_request` for the quality
        // dialog, not in the enumeration caches.
        self.screen.monitors.clear();
        self.screen.windows.clear();
        self.screen.focus_placed = false;
        if let Some(handle) = &self.handle {
            if committed {
                handle.stop_screen_share();
            } else if !restart_open {
                // A restart dialog never opened a picker, so there is nothing
                // to cancel and the live publication stays untouched.
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

    pub(super) fn refresh_screen_sources(&mut self, cx: &mut Context<Self>) {
        // A restart is source-locked: the enumeration cannot change it.
        if self.screen.restart {
            return;
        }
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

    pub(super) fn select_screen_source(&mut self, id: SourceId, cx: &mut Context<Self>) {
        // A restart is bound to the confirmed live source: no cached row,
        // clicked or reached by Enter, may move it elsewhere.
        if self.screen.restart {
            return;
        }
        // A row may only select a source the current snapshot really offers.
        if id.kind != self.screen.kind || self.screen.source_for(id).is_none() {
            return;
        }
        self.screen.selected = Some(id);
        self.screen.error.clear();
        cx.notify();
    }

    /// Switch the picker category; a selection of the other one is dropped.
    pub(super) fn change_share_kind(&mut self, kind: SourceKind, cx: &mut Context<Self>) {
        // A restart is source-locked, so the category cannot change.
        if self.screen.restart {
            return;
        }
        if self.screen.kind == kind {
            return;
        }
        self.screen.kind = kind;
        if self.screen.selected.is_some_and(|id| id.kind != kind) {
            self.screen.selected = None;
        }
        cx.notify();
    }

    /// Start is possible only after the endpoint probe answered: committing
    /// while the answer is pending would publish video without the requested
    /// system audio. A normal Start needs its id in the visible snapshot; a
    /// restart needs only the confirmed identity of the live attempt.
    pub(super) fn screen_can_start(&self) -> bool {
        let source_ready = if self.screen.restart {
            self.screen.restart_is_confirmed()
        } else {
            self.screen.selection_is_current()
        };
        source_ready
            && !self.screen.busy
            && !Codec::available().is_empty()
            && self.screen.audio_availability.is_some()
            && (!self.screen.restart || self.screen.sharing.phase == Phase::Live)
    }

    pub(super) fn start_screen_share(&mut self, cx: &mut Context<Self>) {
        if self.screen.busy {
            return;
        }
        if self.screen.audio_availability.is_none() {
            self.screen.error = AUDIO_PENDING_REASON.into();
            cx.notify();
            return;
        }
        if let Err(reason) = self.share_gate() {
            // The live quality dialog is allowed to pass the "already live"
            // gate: it replaces the current attempt only through a confirmed
            // stop+start. Every other gate (join, permission, slot, capture)
            // still blocks it.
            let restart_bypass = self.screen.restart && reason == "Трансляция уже идёт.";
            if !restart_bypass {
                // An active publication is not an error for this button; its
                // card owns the status and the stop action.
                if !self.share_active() {
                    self.screen.error = reason.to_owned();
                }
                cx.notify();
                return;
            }
        }
        let Some(id) = self.screen.selected else {
            self.screen.error = "Выберите монитор или окно.".into();
            cx.notify();
            return;
        };
        let source = if self.screen.restart {
            // The restart is locked to the source the engine already accepted.
            // The current snapshot may be empty, stale or describe a renamed
            // row; none of that may move the restart to another source.
            if self.screen.sharing.phase != Phase::Live {
                self.screen.error = "Трансляция уже не идёт; откройте выбор экрана заново.".into();
                self.screen.open = false;
                self.screen.restart = false;
                self.screen.focus_placed = false;
                cx.notify();
                return;
            }
            let Some(live) = self.screen.restart_source().cloned() else {
                self.screen.error = SOURCE_GONE_REASON.into();
                cx.notify();
                return;
            };
            if live.source_id() != id {
                self.screen.error = SOURCE_GONE_REASON.into();
                cx.notify();
                return;
            }
            live
        } else {
            // The dialog may be stale: build the request from the row that
            // exists now, and refuse an absent or duplicated id.
            let source = self
                .screen
                .source_for(id)
                .filter(|_| id.kind == self.screen.kind)
                .cloned();
            let Some(source) = source else {
                self.screen.error = SOURCE_GONE_REASON.into();
                cx.notify();
                return;
            };
            source
        };
        let request = PublishRequest {
            source,
            quality: self.screen.quality(),
            codec: Codec::Vp8,
            audio: self.screen.audio_requested(),
        };
        let Some(handle) = &self.handle else {
            self.screen.error = "Голосовое соединение уже закрыто.".into();
            cx.notify();
            return;
        };
        let slot_available = self.active_publishers_in_room() < MAX_CHANNEL_PUBLISHERS;
        if self.screen.restart {
            handle.restart_screen_share(request.clone(), self.can_stream, slot_available);
        } else {
            handle.publish_screen(
                self.screen.picker,
                request.clone(),
                self.can_stream,
                slot_available,
            );
        }
        self.screen.last_request = Some(request);
        self.screen.busy = true;
        self.screen.error.clear();
        cx.notify();
    }

    pub(super) fn stop_screen_share(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn apply_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        let quality = preset.apply(self.screen.quality());
        self.screen.fps = quality.fps;
        self.screen.bitrate_kbps = quality.bitrate_kbps;
        cx.notify();
    }

    /// Step through the six web values; the list is short and ordered.
    fn change_audio_bitrate(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.screen.audio_enabled || !self.screen.audio_available() {
            return;
        }
        let current = AUDIO_BITRATES
            .iter()
            .position(|rate| *rate == self.screen.audio_bitrate_kbps)
            .unwrap_or(2);
        let next = if forward {
            (current + 1).min(AUDIO_BITRATES.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.screen.audio_bitrate_kbps = AUDIO_BITRATES[next];
        cx.notify();
    }

    pub(super) fn toggle_system_audio(&mut self, cx: &mut Context<Self>) {
        if !self.screen.audio_available() {
            return;
        }
        self.screen.audio_enabled = !self.screen.audio_enabled;
        cx.notify();
    }

    /// Apply one runtime update. Returns whether the panel must repaint.
    pub(super) fn apply_screen_update(&mut self, update: VoiceUpdate) -> bool {
        match update {
            VoiceUpdate::ScreenShare(sharing) => {
                let previous = self.screen.sharing.phase;
                self.screen.sharing = sharing;
                let phase = self.screen.sharing.phase;
                if self.screen.sharing.restarting && self.screen.open {
                    // The stop of the quality change is committed; the card
                    // now shows the restart and the dialog must not allow a
                    // second action.
                    self.screen.open = false;
                    self.screen.restart = false;
                    self.screen.busy = false;
                }
                if phase == Phase::Idle || phase == Phase::Failed {
                    self.screen.restart = false;
                }
                if matches!(phase, Phase::Publishing | Phase::Live)
                    && self.screen.open
                    && (self.screen.busy || previous != phase)
                {
                    // Either this dialog committed the attempt (busy) or the
                    // phase really changed: the card in the room now owns the
                    // status and the stop action. A periodic stats update in
                    // the same phase must not close an uncommitted restart
                    // dialog, or its Start could never be reached.
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
                        self.screen.retain_selection(SourceKind::Monitor, &sources);
                        // The non-active kind still refreshes its cache; the
                        // picker reads it when the user switches tabs.
                        self.screen.monitors = sources;
                    }
                    SourceKind::Window if generation >= self.screen.window_generation => {
                        self.screen.window_generation = generation;
                        self.screen.window_loading = false;
                        self.screen.window_error = error.unwrap_or_default();
                        self.screen.retain_selection(SourceKind::Window, &sources);
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

    /// Element id of one source row, named by `kind + id` so a reorder keeps focus.
    pub(super) fn source_element_id(id: SourceId) -> gpui::ElementId {
        let name = match id.kind {
            SourceKind::Monitor => "share-source-monitor",
            SourceKind::Window => "share-source-window",
        };
        gpui::ElementId::from((name, id.id))
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
        let sound = self.screen.sound_label();
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
                    .debug_selector(|| "screen-on-air-quality".into())
                    .child(match stats {
                        Some(stats) => format!("{quality} · {stats}"),
                        None => format!("{quality} · {sound}"),
                    }),
            );
        if sharing.phase != Phase::Stopping {
            let mut actions = div()
                .absolute()
                .left(px(10.))
                .top(px(rect.h - 36.))
                .w(px((rect.w - 20.).max(0.)))
                .h(px(28.))
                .flex()
                .gap_1();
            // The dialog only applies changes while the stream is Live; in
            // Publishing the action would be inert, so it is not shown.
            if sharing.phase == Phase::Live {
                let quality_focus = self.screen_focus[2].clone();
                actions = actions.child(
                    div()
                        .id("screen-quality")
                        .debug_selector(|| "screen-quality".into())
                        .flex_1()
                        .h_full()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(c.line))
                        .text_color(rgb(c.fg))
                        .text_size(px(11.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .track_focus(&quality_focus)
                        .on_click(cx.listener(|panel, _, _, cx| panel.open_screen_settings(cx)))
                        .child("Качество"),
                );
            }
            let stop_focus = self.screen_focus[0].clone();
            actions = actions.child(
                div()
                    .id("screen-stop")
                    .debug_selector(|| "screen-stop".into())
                    .flex_1()
                    .h_full()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(c.danger))
                    .text_color(rgb(c.danger))
                    .text_size(px(11.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .track_focus(&stop_focus)
                    .on_click(cx.listener(|panel, _, _, cx| panel.stop_screen_share(cx)))
                    .child("Завершить"),
            );
            card = card.child(actions);
        }
        Some(card.into_any_element())
    }
}

mod dialog;
