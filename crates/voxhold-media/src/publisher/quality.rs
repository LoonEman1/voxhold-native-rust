//! Quality values of the screen publication: the video settings, the Opus
//! bitrate and the four web presets. Kept in its own module so the publisher
//! state machine stays within the repository file-size budget.

use super::Resolution;

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

/// What the picker sends to the engine. Values are clamped to the web ranges:
/// 0,5–16 Мбит/с in 0,1 steps, 30/60 FPS and the six Opus bitrate choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
            fps: if self.fps >= 60 { 60 } else { 30 },
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
