//! Screen-sharing UI: the room toolbar action, the source picker and the
//! on-air card. All decisions belong to the media runtime; this file renders
//! its state and forwards explicit user intents.
//!
//! Only settings that the SDR publication really applies are shown: the four
//! web presets, resolutions up to the source's size, 30–180 FPS targets,
//! 0,5–16 Мбит/с, the six Opus bitrates and the confirmed codec. P2P, HDR
//! and the web «Содержание»
//! (degradation preference) are absent because the pinned engine or backend
//! cannot apply them.

use super::*;
use gpui::ScrollHandle;
use voxhold_media::publisher::{
    Codec, CodecIntent, CodecRole, CodecSnapshot, Phase, Preset, PublishRequest, Quality,
    QualityChange, QualityOutcome, Resolution, Sharing, AUDIO_BITRATES, MAX_CHANNEL_PUBLISHERS,
};
use voxhold_media::screen_capture::{
    self, PreviewFrame, ScreenSource, SourceId, SourceIdentity, SourceKind, ThumbnailError,
};
use voxhold_media::system_audio::{self, AudioDeviceInfo, AudioScope};
use voxhold_media::video_encoder::{
    AdapterEvidence, AdapterSelection, EncodeQuery, EncoderBackend, EncoderPlan,
    VideoEncoderPreference,
};

/// The backend has no runtime reconfigure; the dialog must warn honestly.
pub(crate) const RESTART_WARNING: &str =
    "Изменение разрешения, частоты кадров или звука перезапустит трансляцию: текущая сессия будет остановлена, зрители увидят новое подключение.";
/// A bitrate-only change reaches the running sender; the dialog says so
/// instead of promising a restart.
pub(crate) const LIVE_BITRATE_HINT: &str =
    "Изменится только битрейт: он применится в эфире без перезапуска и без отключения зрителей.";
/// A Start pressed before the endpoint probe answered; cleared by the answer.
pub(crate) const AUDIO_PENDING_REASON: &str = "Дождитесь проверки системного звука.";
/// The selected identity is gone from the current snapshot; the publish path
/// repeats this check, so a stale dialog never sends a request.
pub(crate) const SOURCE_GONE_REASON: &str =
    "Выбранный источник больше недоступен. Обновите список и выберите источник снова.";
/// The chosen monitor moved to another capturer number or its geometry
/// changed: the numeric id still exists but no longer proves the same screen.
pub(crate) const SOURCE_CHANGED_REASON: &str =
    "Выбранный экран изменился. Обновите список и выберите экран снова.";
/// The account socket always authenticates with `multi_streams: true`
/// (`crates/voxhold-api/src/lib.rs:638`), so the client capability is known.
/// It still does not allow publishing more than one stream in this stage.
pub(crate) const MULTI_STREAMS_SUPPORTED: bool = true;

/// Largest thumbnail width requested from the media layer. The media layer
/// clamps it again, so a caller cannot grow the cache.
pub(super) const SCREEN_THUMBNAIL_WIDTH: u32 = 480;
/// Largest thumbnail height. The pair is the `480×270` request rectangle.
pub(super) const SCREEN_THUMBNAIL_HEIGHT: u32 = 270;
/// Request width before the first picker frame knows the real card width and
/// Windows scale. It matches the normal two-column card in logical pixels, so
/// no test seam captures a 2 px placeholder.
pub(super) const SCREEN_THUMBNAIL_DEFAULT_WIDTH: u32 = 288;
/// Hard bound of the picker preview cache: at most 64 cards.
pub(super) const MAX_SCREEN_PREVIEWS: usize = 64;
/// Hard bound of all BGRA bytes kept for picker previews.
pub(super) const MAX_SCREEN_PREVIEW_BYTES: usize = 16 * 1024 * 1024;
/// A picture that does not fit the cache still gets an honest card note; the
/// note is dropped again when the card returns to the painted viewport.
pub(super) const PREVIEW_MEMORY_REASON: &str = "Предпросмотр недоступен: превышен лимит памяти.";

/// One cached card picture. `bytes` is the BGRA source size, so the budget
/// stays comparable across sessions; `width`/`height` are the delivered frame
/// dimensions and drive the explicit slot geometry.
pub(super) struct PreviewThumb {
    pub(super) image: Arc<RenderImage>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) bytes: usize,
}

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
    /// One codec choice of the picker: «Автоматически» or one of the four SDR
    /// codecs, in the website's order.
    Codec(CodecIntent),
    /// Processor or one hardware adapter. The automatic adapter is the
    /// Windows primary suitable one.
    EncoderBackend(EncoderBackend),
    Audio,
    /// One offered capture scope of the system-audio track.
    AudioScope(AudioScope),
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
    /// Current enumeration key of the chosen row. It is re-resolved when a
    /// snapshot moves the monitor to another id.
    pub(super) selected: Option<SourceId>,
    /// The surface the user really chose. A monitor is its Windows identity;
    /// a window is its `HWND` in [`SourceId::id`].
    pub(super) selected_identity: Option<SourceIdentity>,
    pending_preferences: Option<(PublishRequest, preferences::StreamPreferences)>,
    pub(super) resolution: Resolution,
    pub(super) fps: u32,
    pub(super) bitrate_kbps: u32,
    /// What the picker asks for. The request sent to the media layer carries
    /// the concrete codec resolved against the sender snapshot, never this
    /// intent.
    pub(super) codec: CodecIntent,
    /// Processor or hardware. CPU is the default; no vendor SDK is involved.
    pub(super) encoder_backend: EncoderBackend,
    /// Automatic primary adapter (default) or one explicit LUID.
    pub(super) encoder_adapter: AdapterSelection,
    encoder_probe: encoder::DialogEncoderProbe,
    pub(super) audio_enabled: bool,
    /// The scope the next request carries. Only meaningful when audio is on;
    /// the provided source kind decides the default choice.
    pub(super) audio_scope: AudioScope,
    pub(super) audio_bitrate_kbps: u32,
    pub(super) audio_availability: Option<Result<AudioDeviceInfo, String>>,
    /// The dialog was opened from the live card to change quality. Start then
    /// becomes a confirmed stop+start, never a live reconfigure.
    pub(super) restart: bool,
    pub(super) busy: bool,
    /// A quality-change preflight of this dialog is pending. Start sets it, the
    /// failure answer, a commit or closing the dialog clears it. It survives a
    /// stale state that clears `busy`, so closing always cancels the preflight.
    pub(super) preflight: bool,
    /// Next tag of a quality-change operation. It survives closing the dialog,
    /// so a delayed answer of an older operation can never match a newer one.
    pub(super) quality_op: u64,
    /// The concrete codec the dialog would send, injected by a test so the
    /// restart classification does not depend on the host encoders.
    #[cfg(test)]
    pub(super) test_codec: Option<Codec>,
    /// Sender availability per codec injected by a test, so the selector can
    /// paint a greyed item and its reason on a build whose probe finds every
    /// codec. `None` keeps the real process snapshot.
    #[cfg(test)]
    pub(super) test_codec_availability: Option<Vec<(Codec, Option<String>)>>,
    /// The operation whose result this dialog is waiting for. `busy` belongs
    /// to it; a result of another operation never clears it.
    pub(super) pending_quality_op: Option<u64>,
    pub(super) error: String,
    pub(super) sharing: Sharing,
    /// Room that owns the active publication. It is captured when the publish
    /// intent is sent and never follows the displayed room.
    pub(super) owner_room: i64,
    /// Highest attempt applied from the publisher. A late snapshot of an older
    /// attempt can neither clear the live image nor close its dialog.
    pub(super) applied_attempt: u64,
    /// Attempt (or picker id for an unconfirmed intent) whose stop was already
    /// sent, so a repeated click cannot produce a second `stream.stop`.
    pub(super) stop_sent: Option<u64>,
    pub(super) preview: Option<Arc<RenderImage>>,
    /// Picker thumbnails keyed by the stable surface identity, so a capturer
    /// reorder keeps the picture of the same display. Bounded by
    /// [`MAX_SCREEN_PREVIEWS`] and [`MAX_SCREEN_PREVIEW_BYTES`].
    pub(super) previews: HashMap<SourceIdentity, PreviewThumb>,
    /// Surfaces whose thumbnail attempt failed. The message is painted on the
    /// card; the selection and the publish request never change because of it.
    pub(super) preview_failures: HashMap<SourceIdentity, String>,
    /// Identities whose thumbnail is being captured, so a refresh never
    /// queues the same surface twice.
    pub(super) preview_pending: HashSet<SourceIdentity>,
    /// One preview batch is running for this picker. Only one batch may run at
    /// a time; a request made while it is set returns, and the running batch
    /// picks up newly visible rows at its next safe point and re-requests at
    /// its end.
    pub(super) preview_batching: bool,
    /// A background capture really runs right now. Unlike [`Self::preview_batching`]
    /// this latch is not released by a new epoch: replacing the preview
    /// generation must wait until the old capture returns, so two captures can
    /// never overlap after a DPI or width change.
    pub(super) preview_in_flight: bool,
    /// Session epoch of the open picker. Closing it, a stop, a restart or a new
    /// picker bumps this value; a late capture result is then ignored and no
    /// further source is started.
    pub(super) preview_epoch: u64,
    /// BGRA bytes currently owned by `previews`.
    pub(super) preview_bytes: usize,
    /// Insertion order of `previews`. The oldest picture that is neither
    /// visible nor in flight is evicted first when a card needs room.
    pub(super) preview_order: VecDeque<SourceIdentity>,
    /// Surfaces whose real frame did not fit even after eviction. Not a card
    /// failure: they are retried once a snapshot or tab describes them again.
    pub(super) preview_unfit: HashSet<SourceIdentity>,
    /// Pictures dropped by the count or byte bound, kept for diagnostics/tests.
    pub(super) preview_rejected: u32,
    /// Cards painted by the last frame, so a layout test can prove one card per
    /// source without relying on a single `debug_selector`.
    pub(super) preview_cards: usize,
    /// Requests the dialog deferred for the rows of the last painted frame.
    /// Test-only: it proves a scroll asks once, not once per repaint.
    #[cfg(test)]
    pub(super) preview_deferrals: u32,
    /// Pictures really handed to `Window::drop_image`. Test-only: it proves
    /// that a replaced or cleared preview left the sprite atlas instead of
    /// being only removed from a collection.
    #[cfg(test)]
    pub(super) released_images: u32,
    /// Image id that stood in `preview` when `release_image` last ran.
    /// Test-only: it proves the slot already pointed at the replacement when
    /// the old picture was handed to `Window::drop_image`.
    #[cfg(test)]
    pub(super) last_release_slot_image: Option<u64>,
    /// Native width requested for new thumbnails, derived from the card width
    /// and the current Windows scale. `0` before the first picker frame.
    pub(super) preview_width: u32,
    /// Columns of the currently painted grid. Focus scrolling maps a source
    /// index to its grid row with this value.
    pub(super) preview_columns: usize,
    /// Visible grid rows of the last frame, so captures claim only cards the
    /// user can actually see. `None` means the dialog has not measured its
    /// grid yet (first frame, a restart or an empty list): the batch then
    /// claims only the small bootstrap set.
    pub(super) visible_rows: Option<(usize, usize)>,
    /// Scroll state of the source grid; a focused row is scrolled into view
    /// before the next paint. Recreated for every new picker and tab.
    pub(super) list_scroll: ScrollHandle,
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
            selected_identity: None,
            pending_preferences: None,
            resolution: Resolution::P1080,
            fps: 30,
            bitrate_kbps: 6000,
            codec: CodecIntent::Auto,
            encoder_backend: EncoderBackend::Cpu,
            encoder_adapter: AdapterSelection::Auto,
            encoder_probe: encoder::DialogEncoderProbe::default(),
            audio_enabled: false,
            audio_scope: AudioScope::EverythingExceptSelf,
            audio_bitrate_kbps: 128,
            audio_availability: None,
            restart: false,
            busy: false,
            preflight: false,
            quality_op: 0,
            #[cfg(test)]
            test_codec: None,
            #[cfg(test)]
            test_codec_availability: None,
            pending_quality_op: None,
            error: String::new(),
            sharing: Sharing::default(),
            owner_room: 0,
            applied_attempt: 0,
            stop_sent: None,
            preview: None,
            previews: HashMap::new(),
            preview_failures: HashMap::new(),
            preview_pending: HashSet::new(),
            preview_batching: false,
            preview_in_flight: false,
            preview_epoch: 0,
            preview_bytes: 0,
            preview_order: VecDeque::new(),
            preview_unfit: HashSet::new(),
            preview_rejected: 0,
            preview_cards: 0,
            #[cfg(test)]
            preview_deferrals: 0,
            #[cfg(test)]
            released_images: 0,
            #[cfg(test)]
            last_release_slot_image: None,
            preview_width: 0,
            preview_columns: 2,
            visible_rows: None,
            list_scroll: ScrollHandle::new(),
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

    /// The confirmed live source of a restart dialog. It comes from the
    /// configuration the media layer accepted on air; neither a request this
    /// dialog sent nor an enumeration answer can define or inject it.
    pub(super) fn restart_source(&self) -> Option<&ScreenSource> {
        if !self.restart {
            return None;
        }
        self.sharing
            .confirmed
            .as_ref()
            .map(|confirmed| &confirmed.source)
    }

    /// Whether the restart still points at the identity the engine accepted.
    /// The snapshot is irrelevant: the live source may be absent from it.
    pub(super) fn restart_is_confirmed(&self) -> bool {
        self.restart_source().is_some_and(|source| {
            self.selected == Some(source.source_id()) && self.selected_identity == source.identity()
        })
    }

    /// The request a restart dialog would send: the confirmed live source with
    /// the quality, audio and codec fields shown by the dialog. Only a scope the
    /// user really picks in the dialog can widen a narrow live scope, and the
    /// codec is the one Start resolved, so a codec change is classified as a
    /// restart instead of quietly staying the live payload.
    pub(super) fn restart_request(&self) -> Option<PublishRequest> {
        let mut request = self.sharing.confirmed.as_ref()?.request();
        request.codec = self.resolved_codec().ok()?;
        request.encoder = self.encoder_plan(request.codec).ok()?;
        request.quality = self.quality().clamped();
        request.audio = self.audio_requested();
        request.audio_scope = self.effective_audio_scope(request.source.kind);
        Some(request)
    }

    /// What the dialog's current values would change relative to the confirmed
    /// live configuration. `None` outside a restart dialog or before that
    /// configuration is known.
    pub(super) fn restart_change(&self) -> Option<QualityChange> {
        let next = self.restart_request()?;
        self.restart_change_to(&next)
    }

    /// Classify the exact request the dialog would commit. Start resolves the
    /// codec once; comparing that value (not a second resolution) keeps the
    /// queued command and its hint in agreement.
    pub(super) fn restart_change_to(&self, request: &PublishRequest) -> Option<QualityChange> {
        if !self.restart {
            return None;
        }
        let live = self.sharing.confirmed.as_ref()?.request();
        Some(live.change_to(request))
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

    /// The settings the live attempt really applies, shown on the on-air card.
    /// The confirmed configuration wins over the in-flight restart request.
    pub(super) fn quality_label(&self) -> String {
        self.sharing
            .confirmed
            .as_ref()
            .map(|confirmed| confirmed.quality)
            .or(self.sharing.quality)
            .unwrap_or_else(|| self.quality())
            .describe()
    }

    /// Return the dialog's numeric quality controls to the configuration the
    /// media layer confirmed. A refused change must show the values that are
    /// still on air, never the request that failed.
    pub(super) fn restore_confirmed_quality(&mut self) {
        let Some(confirmed) = self.sharing.confirmed.as_ref() else {
            return;
        };
        self.resolution = confirmed.quality.resolution;
        self.fps = confirmed.quality.fps;
        self.bitrate_kbps = confirmed.quality.bitrate_kbps;
        self.audio_bitrate_kbps = confirmed.quality.audio_bitrate_kbps;
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
    /// The actual scope comes from the engine, not from the picker choice.
    pub(super) fn sound_label(&self) -> String {
        if !self.sharing.has_audio {
            return "Без звука".into();
        }
        let max = self
            .sharing
            .confirmed
            .as_ref()
            .map(|confirmed| confirmed.quality.audio_bitrate_kbps)
            .or(self
                .sharing
                .quality
                .map(|quality| quality.audio_bitrate_kbps))
            .unwrap_or(self.audio_bitrate_kbps);
        let rate = match self.sharing.stats {
            Some(stats) if stats.audio_kbps > 0.0 => {
                format!("Звук ~{:.0} из до {} кбит/с", stats.audio_kbps, max)
            }
            _ => format!("Звук до {max} кбит/с"),
        };
        match self.sharing.audio_scope.as_deref() {
            Some(scope) if !scope.is_empty() => format!("{rate} · {scope}"),
            _ => rate,
        }
    }

    /// The honest hint of the live quality dialog: a bitrate-only change is
    /// applied in place, everything else restarts the publication.
    pub(super) fn restart_warning(&self) -> Option<&'static str> {
        if !self.restart {
            return None;
        }
        match self.restart_change() {
            Some(QualityChange::LiveBitrate) => Some(LIVE_BITRATE_HINT),
            // Apply would send nothing: values equal the live payload.
            Some(QualityChange::Unchanged) => None,
            _ => Some(RESTART_WARNING),
        }
    }
}

/// The exact line the on-air card paints under its title. `stats` carries the
/// measured counters once the engine sampled them, `sound` is the fallback
/// label (which already names the scope). The engine's real scope is appended
/// exactly once, so the measured-stats card names the capture instead of hiding
/// it behind the counters. The codec in the line is the confirmed one — the
/// encoded codec from the stats when present, otherwise the configuration the
/// media layer accepted — never the picker's current desire.
pub(in crate::voice_panel) fn screen_on_air_line(
    sharing: &Sharing,
    quality: &str,
    sound: &str,
    stats: Option<&str>,
) -> String {
    let scope = sharing
        .audio_scope
        .as_deref()
        .filter(|scope| sharing.has_audio && !scope.is_empty());
    let confirmed_codec = sharing
        .stats
        .and_then(|stats| stats.confirmed_codec)
        .or_else(|| sharing.confirmed.as_ref().map(|confirmed| confirmed.codec));
    let mut parts = vec![quality.to_owned()];
    if let Some(codec) = confirmed_codec {
        parts.push(codec.label().to_owned());
    }
    // «GPU» is not a checkbox echo: the engine reported the open hardware
    // session and encoded frames were measured. Before that the line stays
    // silent about the backend.
    if let Some(hardware) = sharing
        .hardware
        .as_ref()
        .filter(|_| confirmed_codec.is_some())
    {
        // An adapter read from the open MFT is a fact. A driver that exposes no
        // adapter attribute leaves only the plan the session was asked to open,
        // and the line says so instead of presenting it as proof.
        let evidence = match hardware.adapter_evidence {
            AdapterEvidence::MftAttribute => "",
            AdapterEvidence::PlanEcho => " (адаптер из плана)",
        };
        parts.push(format!(
            "GPU {} · драйвер 0x{:x}{evidence}",
            hardware.adapter, hardware.driver_version
        ));
    }
    match (stats, scope) {
        (Some(stats), Some(scope)) => {
            parts.push(stats.to_owned());
            parts.push(scope.to_owned());
        }
        (Some(stats), None) => parts.push(stats.to_owned()),
        (None, Some(scope)) if !sound.contains(scope) => {
            parts.push(sound.to_owned());
            parts.push(scope.to_owned());
        }
        (None, _) => parts.push(sound.to_owned()),
    }
    parts.join(" · ")
}

impl VoicePanel {
    pub fn screen_share_open(&self) -> bool {
        self.screen.open
    }

    /// Whether a Start was pressed and the worker has not answered yet.
    #[cfg(test)]
    pub fn screen_share_busy(&self) -> bool {
        self.screen.busy
    }

    /// Whether this client owns an attempt. The action shows a status then.
    pub(crate) fn share_active(&self) -> bool {
        matches!(
            self.screen.sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping
        )
    }

    /// Why the «Экран» action is disabled. During a share the permanent
    /// "already running" text would hide the publication state instead.
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
        self.stream_gate(self.room)
    }

    /// Start gate of one room. A new publication uses the displayed room; the
    /// restart of a live publication uses its owner room, so a channel without
    /// `Stream` that is only being viewed cannot block the settings of the
    /// stream that is already on air.
    pub(super) fn stream_gate(&self, room: i64) -> Result<(), &'static str> {
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
        match self.rooms.get(&room) {
            Some(access) if access.readable && !access.e2ee => {}
            _ => return Err("Нет доступа к голосовому каналу."),
        }
        if self.call_channel == 0 || self.call_channel != room {
            return Err("Сначала подключитесь к голосовому каналу.");
        }
        if self.phase != CallPhase::Connected {
            return Err("Голосовое соединение ещё не установлено.");
        }
        if !self.stream_permission_for(room) {
            return Err("Нет права запускать трансляции в этом канале.");
        }
        if screen_capture::capability_cached().is_err() {
            return Err("Захват экрана недоступен на этом устройстве.");
        }
        if !CodecSnapshot::current().sender_available() {
            return Err("В этой сборке нет доступного SDR-кодека для публикации.");
        }
        if matches!(
            self.screen.sharing.phase,
            Phase::Publishing | Phase::Live | Phase::Stopping
        ) {
            return Err("Трансляция уже идёт.");
        }
        let slot_full = if room == self.room {
            self.active_publishers_in_room() >= MAX_CHANNEL_PUBLISHERS
        } else {
            !self.publish_slot_available()
        };
        if slot_full {
            return Err("В канале уже 8 трансляций. Дождитесь завершения одной из них.");
        }
        Ok(())
    }

    pub(super) fn open_screen_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.share_gate().is_err() {
            return;
        }
        self.release_push_to_talk();
        self.screen.open = true;
        self.screen.restart = false;
        self.screen.busy = false;
        self.screen.preflight = false;
        self.screen.pending_quality_op = None;
        self.screen.error.clear();
        self.screen.selected = None;
        self.screen.selected_identity = None;
        self.screen.kind = SourceKind::Monitor;
        // A new picker starts with no rows: no identity from the previous
        // answer or a live source is selectable while the lists load.
        self.screen.monitors.clear();
        self.screen.windows.clear();
        self.screen.monitor_error.clear();
        self.screen.window_error.clear();
        // A new epoch: a capture of the previous dialog is ignored, and the
        // pictures it owned leave the sprite atlas.
        self.screen.clear_previews(window);
        // A reopened picker starts at the top of its grid: the scroll handle
        // and the rows of the previous dialog are dropped, not reused.
        self.screen.reset_preview_viewport();
        // Restore the local preferences, then probe this session's endpoint
        // before letting the user start the stream. Restored preferences own
        // the resolution, FPS, bitrate, codec and system-sound choice; the
        // probe answer must not leak in, and Start stays locked until it
        // settles. A saved codec this build cannot encode was already turned
        // into Auto with its reason by the restore.
        self.load_stream_preferences();
        self.screen.audio_availability = None;
        self.cancel_encoder_probe();
        self.begin_encoder_probe(cx);
        self.begin_audio_probe(cx);
        self.screen.focus_placed = false;
        if let Some(handle) = &self.handle {
            self.screen.picker = handle.begin_screen_picker();
        }
        self.sync_screen_access();
        self.request_screen_sources(cx);
        self.request_screen_previews(window, cx);
        cx.notify();
    }

    /// Open the quality dialog from the live card. The source is locked; the
    /// backend only applies the change through a stop and a new start.
    pub(super) fn open_screen_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen.sharing.phase != Phase::Live {
            return;
        }
        // Without a configuration the media layer really accepted on air there
        // is nothing safe to restart; a sent-but-unconfirmed request is not a
        // substitute. The live card still owns the stop action.
        let Some(confirmed) = self.screen.sharing.confirmed.clone() else {
            return;
        };
        // The dialog really opens from here, so the push-to-talk key is free for
        // its own use. A refused open leaves the voice state untouched.
        self.release_push_to_talk();
        self.screen.resolution = confirmed.quality.resolution;
        self.screen.fps = confirmed.quality.fps;
        self.screen.bitrate_kbps = confirmed.quality.bitrate_kbps;
        self.screen.audio_bitrate_kbps = confirmed.quality.audio_bitrate_kbps;
        // A restart keeps the codec that is really on air; changing it is a
        // separate explicit choice.
        self.screen.codec = CodecIntent::Explicit(confirmed.codec);
        self.screen.audio_enabled = confirmed.audio;
        // The dialog starts from the scope the live attempt accepted; opening
        // it never widens a narrow scope.
        self.screen.audio_scope = confirmed.audio_scope;
        self.screen.encoder_backend = confirmed.encoder.backend;
        self.cancel_encoder_probe();
        self.begin_audio_probe(cx);
        self.screen.restart = true;
        // The restart dialog is bound to the confirmed live source here, and
        // only here: the identity stays in `confirmed`, never in the
        // enumeration caches, so the current snapshot may be empty or stale.
        self.screen.bind_confirmed_live_source();
        // A restart has no picker: release every thumbnail and bump the epoch
        // so an earlier picker capture cannot land in this dialog.
        self.screen.clear_previews(window);
        self.screen.open = true;
        self.begin_encoder_probe(cx);
        self.screen.busy = false;
        self.screen.preflight = false;
        self.screen.pending_quality_op = None;
        self.screen.error.clear();
        self.screen.focus_placed = false;
        self.sync_screen_access();
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
    /// the viewer alone. After Start (Escape, backdrop or a repeated close) it
    /// is a stop intent: the attempt already committed, so exactly one
    /// `stream.stop` is sent and the capture is released.
    pub(super) fn close_screen_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.screen.open {
            return;
        }
        // A live bitrate change is still being applied by the senders. Closing
        // would hide it while it can still change the on-air limit (and a
        // stop/cancel does not belong to this operation), so Escape, the
        // backdrop and Cancel wait for the engine's Applied/Failed answer.
        let applying = self.screen.pending_quality_op.is_some_and(|op| {
            self.screen
                .sharing
                .quality_status
                .as_ref()
                .is_some_and(|status| status.op == op && status.outcome == QualityOutcome::Applying)
        });
        if applying {
            return;
        }
        let restart_open = self.screen.restart;
        // A preflight is pending as soon as this dialog asked for a restart,
        // even before the worker's `preflighting` update lands or after a
        // stale sample cleared `busy`. Closing cancels it and keeps Live.
        let preflight_pending =
            restart_open && (self.screen.preflight || self.screen.sharing.preflighting);
        let restart_busy = restart_open && (self.screen.busy || preflight_pending);
        // Closing without a committed change leaves the live stream untouched.
        // A restart only commits once the publisher confirmed the stop; while
        // its preflight runs, closing cancels it instead.
        let committed = if restart_open {
            self.screen.sharing.restarting && self.screen.busy
        } else {
            self.screen.busy || self.share_active()
        };
        let pending_op = self.screen.pending_quality_op;
        self.screen.open = false;
        self.cancel_encoder_probe();
        self.screen.restart = false;
        self.screen.busy = false;
        self.screen.preflight = false;
        self.screen.pending_quality_op = None;
        self.screen.error.clear();
        self.screen.selected = None;
        self.screen.selected_identity = None;
        // No row of this dialog may survive into the next picker. The
        // confirmed live identity lives in `sharing.confirmed`, not in these
        // caches.
        self.screen.monitors.clear();
        self.screen.windows.clear();
        // Closing cancels the preview batch: the epoch moves, so a late answer
        // is ignored, and the pictures/titles are released immediately.
        self.screen.clear_previews(window);
        self.screen.focus_placed = false;
        let stop_now = committed && self.latch_screen_stop();
        if let Some(handle) = &self.handle {
            if stop_now {
                handle.stop_screen_share();
            } else if restart_busy {
                if let Some(op) = pending_op {
                    handle.cancel_screen_restart(op);
                }
            } else if !restart_open {
                // A restart dialog never opened a picker: nothing to cancel.
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

    pub(super) fn refresh_screen_sources(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        // A refresh retries a failed thumbnail of the visible category; cached
        // pictures and running attempts are kept, and an absent source is
        // still requested only after its new answer describes it.
        let kind = self.screen.kind;
        self.screen
            .preview_failures
            .retain(|id, _| selection::identity_kind(id) != kind);
        self.request_screen_previews(window, cx);
        cx.notify();
    }

    pub(super) fn select_screen_source(&mut self, id: SourceId, cx: &mut Context<Self>) {
        // A restart is bound to the confirmed live source: no cached row,
        // clicked or reached by Enter, may move it elsewhere.
        if self.screen.restart {
            return;
        }
        // A row may only select a source the current snapshot really offers,
        // and a monitor must carry a confirmed display identity.
        if id.kind != self.screen.kind {
            return;
        }
        let Some(source) = self.screen.source_for(id) else {
            return;
        };
        let Some(identity) = source.identity() else {
            self.screen.error = SOURCE_CHANGED_REASON.into();
            cx.notify();
            return;
        };
        self.screen.selected = Some(id);
        self.screen.selected_identity = Some(identity);
        self.screen.error.clear();
        self.begin_encoder_probe(cx);
        cx.notify();
    }

    /// Switch the picker category; a selection of the other one is dropped.
    pub(super) fn change_share_kind(
        &mut self,
        kind: SourceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A restart is source-locked, so the category cannot change.
        if self.screen.restart {
            return;
        }
        if self.screen.kind == kind {
            return;
        }
        self.screen.kind = kind;
        // The other tab starts at the top of its own list: the rows and the
        // scroll offset of the previous grid must not choose its captures.
        self.screen.reset_preview_viewport();
        if self.screen.selected.is_some_and(|id| id.kind != kind) {
            self.screen.selected = None;
            self.screen.selected_identity = None;
        }
        // The safe default follows the source kind; an explicit whole-device
        // choice survives a tab switch.
        if self.screen.audio_scope != AudioScope::Device {
            self.screen.reset_audio_scope(kind);
        }
        // Only the visible category is captured. A batch for the hidden one
        // stops at its next check; its pending ids are dropped so switching
        // back requests them again instead of treating them as in flight.
        self.screen
            .preview_pending
            .retain(|id| selection::identity_kind(id) == kind);
        // A surface whose frame could not fit before gets another chance in
        // the category the user really asked for now.
        self.screen
            .preview_unfit
            .retain(|id| selection::identity_kind(id) != kind);
        self.begin_encoder_probe(cx);
        self.request_screen_previews(window, cx);
        cx.notify();
    }

    /// Resolve the dialog's codec intent against the sender probe. The refusal
    /// is the ready user-facing reason the dialog paints and Start reports.
    pub(super) fn codec_resolution(&self) -> Result<Codec, String> {
        #[cfg(test)]
        if let Some(reason) = &self.test_codec_error {
            return Err(reason.clone());
        }
        self.screen.resolved_codec()
    }

    /// Sender availability of one codec exactly as the selector paints it. The
    /// injected refusal that makes `codec_resolution` fail describes the same
    /// build here, so the dialog can never claim «no SDR codec» while painting
    /// every item as selectable.
    pub(super) fn codec_availability(&self, codec: Codec) -> Result<(), String> {
        #[cfg(test)]
        if let Some(reason) = &self.test_codec_error {
            return Err(reason.clone());
        }
        self.screen.codec_availability(codec)
    }

    /// Start waits for the endpoint probe: committing earlier would publish
    /// video without the requested system audio. A normal Start needs its id
    /// in the snapshot; a restart needs the confirmed live identity.
    pub(super) fn screen_can_start(&self) -> bool {
        let source_ready = if self.screen.restart {
            self.screen.restart_is_confirmed()
        } else {
            self.screen.selection_is_current()
        };
        source_ready
            && !self.screen.busy
            && !self.screen.preflight
            && !self.screen.sharing.preflighting
            && self.codec_resolution().is_ok()
            && self.screen.audio_availability.is_some()
            && (!self.screen.restart || self.screen.sharing.phase == Phase::Live)
    }

    pub(super) fn change_bitrate(&mut self, delta: i64, cx: &mut Context<Self>) {
        let next = (self.screen.bitrate_kbps as i64 + delta).clamp(500, 16_000);
        self.screen.bitrate_kbps = ((next / 100) * 100) as u32;
        cx.notify();
    }

    /// Choose a codec in the picker or the live dialog. An explicit item this
    /// build cannot encode is inert: the greyed button keeps its reason and
    /// Start stays blocked instead of silently substituting another codec.
    /// A changed codec reaches Start as a restart/preflight through
    /// `restart_change_to`, never as a live bitrate reconfiguration.
    pub(super) fn select_screen_codec(&mut self, intent: CodecIntent, cx: &mut Context<Self>) {
        if let CodecIntent::Explicit(codec) = intent {
            if self.codec_availability(codec).is_err() {
                return;
            }
        }
        if self.screen.codec == intent {
            return;
        }
        self.screen.codec = intent;
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
        if self.screen.audio_enabled {
            // Enabling always starts from a scope the kind really offers.
            let scope = self.screen.effective_audio_scope(self.screen.kind);
            self.screen.audio_scope = scope;
        }
        cx.notify();
    }

    /// Forget every screen-share surface: leaving the call or a revoked
    /// session must not keep a card, a picker or a stale preview.
    pub(super) fn reset_screen(&mut self, window: &mut Window) {
        // A new picker must wait for a capture already running in the old
        // epoch; its result will be discarded after it releases the latch.
        let mut old = std::mem::take(&mut self.screen);
        self.screen.preview_epoch = old.preview_epoch.wrapping_add(1);
        self.screen.preview_in_flight = old.preview_in_flight;
        old.release_all_images(window);
        #[cfg(test)]
        {
            self.screen.released_images = old.released_images;
        }
        // The owner identity belongs to the session that just ended; keeping it
        // would gate the next displayed room with a stream that no longer runs.
        self.publish_access = None;
    }

    fn share_focus(&self, index: usize) -> FocusHandle {
        // A ring larger than the handle range reuses the last handle; the
        // visible rows stay reachable and no index overlaps the toolbar.
        self.picker_focus[index.min(self.picker_focus.len() - 1)].clone()
    }

    /// Element id of one source row, named by the stable surface identity so
    /// a reorder keeps focus on the same display.
    pub(super) fn source_element_id(source: &ScreenSource) -> gpui::ElementId {
        let name = match source.kind {
            SourceKind::Monitor => "share-source-monitor",
            SourceKind::Window => "share-source-window",
        };
        gpui::ElementId::from((name, source.focus_key()))
    }
}

impl VoicePanel {
    /// The «Экран» button inside the room toolbar. The reason of a disabled
    /// state is rendered next to the toolbar by the room body. The button keeps
    /// the web `CallControls` shape: a vertical icon above the label.
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
            .w(px(voice_room::TOOLBAR_BUTTON_WIDTH))
            .h(px(voice_room::TOOLBAR_BUTTON_HEIGHT))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(3.))
            .rounded(px(9.))
            .bg(rgb(if active { c.danger_bg } else { c.surface }))
            .border_1()
            .border_color(rgb(if active { c.danger } else { c.line }))
            .text_color(rgb(color))
            .when(enabled, |button| button.cursor_pointer())
            .track_focus(&self.screen_focus[1])
            .on_click(cx.listener(|panel, _, window, cx| {
                if panel.share_gate().is_ok() {
                    panel.open_screen_share(window, cx);
                }
            }))
            .child(super::view::vector_icon(
                "icons/monitor.svg",
                16.,
                rgb(color),
            ))
            .child(
                div()
                    .text_size(px(9.))
                    .line_height(px(10.))
                    .truncate()
                    .child("Экран"),
            )
            .into_any_element()
    }
}

mod audio_scope;
mod card;
mod codec;
mod dialog;
mod owner;
mod picker;
mod preferences;
pub(crate) use preferences::StreamPrefsStore;
mod encoder;
mod preview;
mod selection;
mod start;
mod update;
