//! Quality values of the screen publication: the video settings, the Opus
//! bitrate and the four web presets. Kept in its own module so the publisher
//! state machine stays within the repository file-size budget.

use super::PublishRequest;

/// Resolution limits for an SDR publication. Capture preserves the source
/// aspect ratio and never scales above its actual frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Resolution {
    #[default]
    P720,
    P1080,
    P1440,
    P2160,
    Native,
}

impl Resolution {
    pub const OPTIONS: [Self; 5] = [
        Self::P720,
        Self::P1080,
        Self::P1440,
        Self::P2160,
        Self::Native,
    ];

    pub fn size(self) -> (u32, u32) {
        match self {
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
            Self::P1440 => (2560, 1440),
            Self::P2160 => (3840, 2160),
            Self::Native => (4096, 2304),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::P720 => "1280 × 720",
            Self::P1080 => "1920 × 1080",
            Self::P1440 => "2560 × 1440",
            Self::P2160 => "3840 × 2160",
            Self::Native => "Как у источника",
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "2160p",
            Self::Native => "native",
        }
    }

    pub fn available_for(self, source: Option<(u32, u32)>) -> bool {
        if self == Self::Native {
            return true;
        }
        let Some((width, height)) = source.filter(|&(width, height)| width > 0 && height > 0)
        else {
            return matches!(self, Self::P720 | Self::P1080);
        };
        let (long, short) = self.size();
        width.max(height) >= long && width.min(height) >= short
    }

    /// Choose a smaller preset for a smaller source without changing the saved
    /// preference. Native is the only option for a source smaller than 720p.
    pub fn for_source(self, source: Option<(u32, u32)>) -> Self {
        if self.available_for(source) {
            return self;
        }
        Self::OPTIONS
            .into_iter()
            .rev()
            .find(|&resolution| resolution != Self::Native && resolution.available_for(source))
            .unwrap_or(Self::Native)
    }

    fn capture_target(self, width: u32, height: u32) -> (u32, u32) {
        let (long, short) = self.size();
        if width > 0 && height > width {
            (short, long)
        } else {
            (long, short)
        }
    }

    pub fn capture_size(self, width: u32, height: u32) -> (u32, u32) {
        let (max_width, max_height) = self.capture_target(width, height);
        if width < 2 || height < 2 {
            (max_width, max_height)
        } else {
            crate::screen_capture::fit_even(width, height, max_width, max_height)
        }
    }
}

/// Capture rate targets offered by the native picker.
pub const MAX_SCREEN_FPS: u32 = 180;
pub const SCREEN_FRAME_RATES: [u32; 6] = [30, 60, 90, 120, 144, MAX_SCREEN_FPS];

/// Opus bitrate choices of the web dialog. The backend accepts 32–510 kbit/s,
/// but the client shows the same six values as the web form.
pub const AUDIO_BITRATES: [u32; 6] = [64, 96, 128, 192, 256, 320];

/// Round a requested Opus bitrate to the closest value the UI offers, then
/// clamp it into the web range 64–320 kbit/s.
pub fn normalize_audio_bitrate(value: u32) -> u32 {
    let clamped = value.clamp(64, 320);
    AUDIO_BITRATES
        .into_iter()
        .min_by_key(|candidate| candidate.abs_diff(clamped))
        .unwrap_or(128)
}

/// What the picker sends to the engine: an SDR capture limit, a frame-rate
/// target, and bounded video and audio bitrates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Quality {
    pub resolution: Resolution,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Opus maximum bitrate. It is applied only when the publication really
    /// adds the system-audio track; it is never a promise that the source is
    /// audible.
    pub audio_bitrate_kbps: u32,
}

impl Default for Quality {
    fn default() -> Self {
        // The web `StreamPreferences` default: 1080p, 30 FPS, 6,0 Mbit/s and
        // 128 kbit/s of system audio.
        Self {
            resolution: Resolution::P1080,
            fps: 30,
            bitrate_kbps: 6000,
            audio_bitrate_kbps: 128,
        }
    }
}

impl Quality {
    pub fn clamped(self) -> Self {
        Self {
            fps: SCREEN_FRAME_RATES
                .into_iter()
                .rev()
                .find(|&fps| fps <= self.fps)
                .unwrap_or(SCREEN_FRAME_RATES[0]),
            bitrate_kbps: (self.bitrate_kbps.clamp(500, 16_000) / 100) * 100,
            audio_bitrate_kbps: normalize_audio_bitrate(self.audio_bitrate_kbps),
            ..self
        }
    }

    /// `1280 × 720 · 30 FPS · 4,0 Мбит/с`
    pub fn describe(self) -> String {
        format!(
            "{} · {} FPS · {},{:01} Мбит/с",
            self.resolution.label(),
            self.fps,
            self.bitrate_kbps / 1000,
            (self.bitrate_kbps % 1000) / 100
        )
    }

    /// Honest audio line of the applied settings, separate from the video one.
    pub fn describe_audio(self) -> String {
        format!("аудио до {} кбит/с", self.audio_bitrate_kbps)
    }
}

/// One-click combinations of the web `StreamSettingsDialog`. They change only
/// the frame rate and the video bitrate, exactly like the web presets; the
/// resolution depends on the publisher's screen and is left as chosen. There
/// is no degradation-preference control because `RtpEncodingParameters` of the
/// pinned engine has no such field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Desktop,
    Games,
    Films,
    Custom,
}

impl Preset {
    pub fn all() -> [Preset; 4] {
        [Self::Desktop, Self::Games, Self::Films, Self::Custom]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Рабочий стол",
            Self::Games => "Игры",
            Self::Films => "Фильмы",
            Self::Custom => "Своё",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Desktop => "Чёткий текст, 30 FPS",
            Self::Games => "Плавность, 60 FPS",
            Self::Films => "Тёмные сцены, 30 FPS",
            Self::Custom => "Настройки вручную",
        }
    }

    /// The frame rate and video bitrate this preset applies. `Custom` keeps
    /// the current values.
    pub fn values(self) -> Option<(u32, u32)> {
        match self {
            Self::Desktop => Some((30, 6000)),
            Self::Games => Some((60, 12_000)),
            Self::Films => Some((30, 8000)),
            Self::Custom => None,
        }
    }

    pub fn apply(self, quality: Quality) -> Quality {
        match self.values() {
            Some((fps, bitrate_kbps)) => Quality {
                fps,
                bitrate_kbps,
                ..quality
            },
            None => quality,
        }
    }

    pub fn detect(quality: Quality) -> Preset {
        [Self::Desktop, Self::Games, Self::Films]
            .into_iter()
            .find(|preset| preset.values() == Some((quality.fps, quality.bitrate_kbps)))
            .unwrap_or(Self::Custom)
    }
}

/// What a new request would change relative to the live one.
///
/// The pinned backend has no runtime reconfigure and rejects a second
/// `stream.start` while the client already publishes, so only the bitrates can
/// reach the running sender. They are applied through the existing RTP
/// `set_parameters`/`set_bitrate` path. A source, resolution, frame-rate or
/// audio-scope change needs a new capture and therefore a confirmed stop+start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualityChange {
    /// The request is the live one; nothing has to be sent anywhere.
    Unchanged,
    /// Only the video or the published-audio bitrate changed.
    LiveBitrate,
    /// A new capture is required: source, resolution, frame rate, audio scope,
    /// the audio flag or the codec changed.
    Restart,
}

/// The tag of one quality-change operation. The dialog increments it for every
/// Apply; the worker reports the result under the same tag. Coalescing the
/// snapshots can then never detach a fast failure from the request it answers.
pub type QualityOp = u64;

/// The last result of one quality-change operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityStatus {
    pub op: QualityOp,
    pub outcome: QualityOutcome,
}

/// What happened to the operation `QualityStatus::op`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QualityOutcome {
    /// The worker accepted the operation and its preflight is running. The
    /// live publication is untouched.
    Preflighting,
    /// The operation stopped before any restart. The live publication keeps
    /// running; the message belongs to the dialog.
    Failed(String),
    /// The user or the session cancelled the operation; nothing was stopped.
    Cancelled,
    /// The confirmed stop of a restart was sent. The new attempt starts after
    /// its acknowledgement.
    Committed,
    /// A live-bitrate command reached the engine worker and its senders are
    /// being updated. The previous confirmed configuration still describes
    /// what is on air; the dialog stays busy and open.
    Applying,
    /// The engine verified the new limits on every active sender and the
    /// peer, so the confirmed configuration follows the change.
    Applied,
}

impl QualityStatus {
    pub fn new(op: QualityOp, outcome: QualityOutcome) -> Self {
        Self { op, outcome }
    }
}

impl PublishRequest {
    pub(super) fn capture_target(&self) -> (u32, u32) {
        self.quality
            .resolution
            .capture_target(self.source.width, self.source.height)
    }

    pub(super) fn capture_size(&self) -> (u32, u32) {
        self.quality
            .resolution
            .capture_size(self.source.width, self.source.height)
    }

    /// Classify the difference against the live request. The source identity,
    /// not its display title, decides whether the same capture can continue.
    pub fn change_to(&self, next: &Self) -> QualityChange {
        if self.source.source_id() != next.source.source_id()
            || self.codec != next.codec
            || self.audio != next.audio
            || self.audio_scope != next.audio_scope
            || self.quality.resolution != next.quality.resolution
            || self.quality.fps != next.quality.fps
        {
            return QualityChange::Restart;
        }
        // An audio bitrate that is not published cannot be observed, so it is
        // not a live change either; the dialog stays off that control when the
        // track is disabled.
        let audio_bitrate_changed =
            self.audio && self.quality.audio_bitrate_kbps != next.quality.audio_bitrate_kbps;
        if self.quality.bitrate_kbps != next.quality.bitrate_kbps || audio_bitrate_changed {
            return QualityChange::LiveBitrate;
        }
        QualityChange::Unchanged
    }
}
