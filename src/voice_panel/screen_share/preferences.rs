//! Per-installation stream preferences beside the workspace layout.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(super) struct StreamPreferences {
    quality: Quality,
    audio_enabled: bool,
    audio_scope: AudioScope,
}

impl Default for StreamPreferences {
    fn default() -> Self {
        Self {
            quality: Quality::default(),
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
        let mut prefs = StreamPreferences::read_picker(&self.screen);
        if self
            .screen
            .sharing
            .confirmed
            .as_ref()
            .is_some_and(|live| live.quality.resolution == prefs.quality.resolution)
        {
            // A smaller source may be on air at a fallback resolution. An
            // FPS or bitrate edit must keep the saved resolution preference.
            prefs.quality.resolution = self.stream_preferences.0.prefs.get().quality.resolution;
        }
        self.screen.pending_preferences = Some((request, prefs));
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
            audio_enabled: true,
            audio_scope: AudioScope::Device,
        };
        let writes = PreferenceWrites::default();
        writes.requested.store(2, Ordering::Release);
        writes.save(2, &path, latest);
        writes.save(1, &path, StreamPreferences::default());
        assert_eq!(StreamPreferences::load(Some(&path)), latest);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
    }

    fn tempfile_for_test() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("voxhold-stream-prefs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        dir
    }
}
