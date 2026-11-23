//! Per-installation stream preferences beside the workspace layout.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use super::*;

/// The codec the previous picker left behind. The wire value is the website's
/// `StreamPreferences.codec` (`auto` or one of the SDR codec names), so the same
/// file stays readable by both clients. Unknown or missing values fall back to
/// `Auto` instead of discarding the other preferences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum CodecPreference {
    #[default]
    Auto,
    Explicit(Codec),
}

impl CodecPreference {
    fn wire(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Explicit(codec) => codec.wire(),
        }
    }

    fn parse(value: &str) -> Self {
        Codec::parse(value).map_or(Self::Auto, Self::Explicit)
    }

    fn from_intent(intent: CodecIntent) -> Self {
        match intent {
            CodecIntent::Auto => Self::Auto,
            CodecIntent::Explicit(codec) => Self::Explicit(codec),
        }
    }
}

impl serde::Serialize for CodecPreference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire())
    }
}

impl<'de> serde::Deserialize<'de> for CodecPreference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self::parse(&value))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(super) struct StreamPreferences {
    quality: Quality,
    codec: CodecPreference,
    audio_enabled: bool,
    audio_scope: AudioScope,
}

impl Default for StreamPreferences {
    fn default() -> Self {
        Self {
            quality: Quality::default(),
            codec: CodecPreference::Auto,
            audio_enabled: false,
            audio_scope: AudioScope::EverythingExceptSelf,
        }
    }
}

impl StreamPreferences {
    fn load(path: Option<&Path>) -> Self {
        let mut prefs: Self = path
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        prefs.quality = prefs.quality.clamped();
        prefs
    }

    fn read_picker(picker: &ScreenShareUi) -> Self {
        Self {
            quality: picker.quality().clamped(),
            codec: CodecPreference::from_intent(picker.codec),
            audio_enabled: picker.audio_enabled,
            audio_scope: picker.audio_scope,
        }
    }

    fn apply(self, picker: &mut ScreenShareUi) {
        picker.resolution = self.quality.resolution;
        picker.fps = self.quality.fps;
        picker.bitrate_kbps = self.quality.bitrate_kbps;
        picker.audio_bitrate_kbps = self.quality.audio_bitrate_kbps;
        picker.audio_enabled = self.audio_enabled;
        picker.audio_scope = self.audio_scope;
        picker.audio_scope = picker.effective_audio_scope(picker.kind);
        // A saved explicit codec is restored only while this build can encode
        // it. Otherwise the picker falls back to Auto and says why, instead of
        // restoring an inert item the user would have to fix on every start.
        picker.codec = match self.codec {
            CodecPreference::Auto => CodecIntent::Auto,
            CodecPreference::Explicit(codec) => match picker.codec_availability(codec) {
                Ok(()) => CodecIntent::Explicit(codec),
                Err(reason) => {
                    let reason = reason.trim_end_matches('.');
                    picker.error = format!(
                        "Сохранённый кодек {} недоступен: {reason}. Выбран «Автоматически».",
                        codec.label()
                    );
                    CodecIntent::Auto
                }
            },
        };
    }
}

#[derive(Clone, Default)]
pub(crate) struct StreamPrefsStore(Rc<StoredPreferences>);

#[derive(Default)]
struct StoredPreferences {
    path: Option<PathBuf>,
    prefs: Cell<StreamPreferences>,
    writes: Arc<PreferenceWrites>,
}

#[derive(Default)]
struct PreferenceWrites {
    requested: AtomicU64,
    saved: Mutex<u64>,
}

impl PreferenceWrites {
    fn save(&self, generation: u64, path: &Path, prefs: StreamPreferences) {
        let mut saved = self.saved.lock().unwrap_or_else(|error| error.into_inner());
        if generation <= *saved || generation != self.requested.load(Ordering::Acquire) {
            return;
        }
        let result = serde_json::to_vec_pretty(&prefs)
            .map_err(std::io::Error::other)
            .and_then(|bytes| crate::instance_prefs::write_atomic(path, &bytes));
        match result {
            Ok(()) => *saved = generation,
            Err(error) => eprintln!(
                "Не удалось сохранить настройки трансляции: {}",
                error.kind()
            ),
        }
    }
}

impl StreamPrefsStore {
    pub(crate) fn with_layout(layout: Option<&Path>) -> Self {
        let path = layout.map(|path| {
            let mut name = path.as_os_str().to_owned();
            name.push(".stream.json");
            PathBuf::from(name)
        });
        let prefs = Cell::new(StreamPreferences::load(path.as_deref()));
        Self(Rc::new(StoredPreferences {
            path,
            prefs,
            writes: Arc::default(),
        }))
    }

    fn set(&self, prefs: StreamPreferences, cx: &mut Context<VoicePanel>) {
        if self.0.prefs.replace(prefs) == prefs {
            return;
        }
        let Some(path) = self.0.path.clone() else {
            return;
        };
        let generation = self.0.writes.requested.fetch_add(1, Ordering::AcqRel) + 1;
        let writes = self.0.writes.clone();
        cx.background_spawn(async move { writes.save(generation, &path, prefs) })
            .detach();
    }
}

impl Drop for StoredPreferences {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            // A queued write can still be pending when the window exits.
            let generation = self.writes.requested.load(Ordering::Acquire);
            self.writes.save(generation, path, self.prefs.get());
        }
    }
}

impl VoicePanel {
    pub(crate) fn set_stream_preferences(&mut self, store: StreamPrefsStore) {
        self.stream_preferences = store;
    }

    pub(super) fn load_stream_preferences(&mut self) {
        self.screen.pending_preferences = None;
        self.stream_preferences
            .0
            .prefs
            .get()
            .apply(&mut self.screen);
    }

    pub(super) fn remember_stream_preferences(&mut self, cx: &mut Context<Self>) {
        self.stream_preferences
            .set(StreamPreferences::read_picker(&self.screen), cx);
    }

    pub(super) fn stage_live_preferences(&mut self, request: PublishRequest) {
        let prefs = self.live_preferences();
        self.screen.pending_preferences = Some((request, prefs));
    }

    pub(super) fn remember_live_preferences(&mut self, cx: &mut Context<Self>) {
        let prefs = self.live_preferences();
        self.stream_preferences.set(prefs, cx);
    }

    /// Preferences read from the live dialog. A smaller source may be on air at
    /// a fallback resolution: an FPS, bitrate or codec edit must keep the saved
    /// resolution preference instead of lowering it to the applied one.
    fn live_preferences(&self) -> StreamPreferences {
        let mut prefs = StreamPreferences::read_picker(&self.screen);
        if self
            .screen
            .sharing
            .confirmed
            .as_ref()
            .is_some_and(|live| live.quality.resolution == prefs.quality.resolution)
        {
            prefs.quality.resolution = self.stream_preferences.0.prefs.get().quality.resolution;
        }
        prefs
    }

    pub(in crate::voice_panel) fn save_confirmed_stream_preferences(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let Some((request, _)) = &self.screen.pending_preferences else {
            return;
        };
        if self.screen.sharing.phase == Phase::Live
            && self
                .screen
                .sharing
                .confirmed
                .as_ref()
                .is_some_and(|confirmed| {
                    request.source.identity() == confirmed.source.identity()
                        && request.change_to(&confirmed.request()) == QualityChange::Unchanged
                })
        {
            if let Some((_, prefs)) = self.screen.pending_preferences.take() {
                self.stream_preferences.set(prefs, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_preferences_round_trip_and_older_writes_cannot_overwrite_them() {
        let dir = tempfile_for_test();
        let path = dir.join("layout.json.stream.json");
        let latest = StreamPreferences {
            quality: Quality {
                resolution: Resolution::P2160,
                fps: 180,
                bitrate_kbps: 14_000,
                audio_bitrate_kbps: 256,
            },
            codec: CodecPreference::Explicit(Codec::Av1),
            audio_enabled: true,
            audio_scope: AudioScope::Device,
        };
        let writes = PreferenceWrites::default();
        writes.requested.store(2, Ordering::Release);
        writes.save(2, &path, latest);
        writes.save(1, &path, StreamPreferences::default());
        assert_eq!(StreamPreferences::load(Some(&path)), latest);
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(stored.contains("\"codec\": \"av1\""), "{stored}");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn a_selected_codec_is_remembered_and_restored() {
        let mut picker = ScreenShareUi::default();
        picker.codec = CodecIntent::Explicit(Codec::Vp9);
        let prefs = StreamPreferences::read_picker(&picker);
        assert_eq!(prefs.codec, CodecPreference::Explicit(Codec::Vp9));
        let json = serde_json::to_string(&prefs).unwrap();
        assert!(json.contains("\"codec\":\"vp9\""), "{json}");

        let mut reopened = ScreenShareUi::default();
        reopened.test_codec_availability = Some(vec![(Codec::Vp9, None)]);
        prefs.apply(&mut reopened);
        assert_eq!(reopened.codec, CodecIntent::Explicit(Codec::Vp9));
        assert!(reopened.error.is_empty());
    }

    #[test]
    fn a_missing_or_unknown_saved_codec_falls_back_to_auto() {
        // The previous file format had no codec field at all.
        let older: StreamPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(older.codec, CodecPreference::Auto);
        // A value this build does not know must not discard the whole file.
        let unknown: StreamPreferences = serde_json::from_str(r#"{"codec":"hevc"}"#).unwrap();
        assert_eq!(unknown.codec, CodecPreference::Auto);
    }

    #[test]
    fn an_unavailable_saved_codec_restores_auto_with_the_reason() {
        const REASON: &str = "Кодировщик VP9 отсутствует в этой сборке WebRTC.";
        let prefs = StreamPreferences {
            codec: CodecPreference::Explicit(Codec::Vp9),
            ..Default::default()
        };
        let mut picker = ScreenShareUi::default();
        picker.test_codec_availability =
            Some(vec![(Codec::H264, None), (Codec::Vp9, Some(REASON.into()))]);
        prefs.apply(&mut picker);
        assert_eq!(
            picker.codec,
            CodecIntent::Auto,
            "an unencodable saved codec is not restored as an inert choice"
        );
        assert!(picker.error.contains("VP9"), "{}", picker.error);
        assert!(
            picker.error.contains(REASON.trim_end_matches('.')),
            "the reason must reach the dialog: {}",
            picker.error
        );
    }

    fn tempfile_for_test() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("voxhold-stream-prefs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        dir
    }
}
