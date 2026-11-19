//! Persisted voice preferences and the settings-tab model. The media engine and
//! device capture stay in `voxhold-media`; this module only stores choices,
//! lists devices and owns the tab-lifetime preview/meter state, matching
//! `settings_voice.go` and `workspace-layout.json.voice.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Go `voiceBitrates`, in the frontend order. 160 is not offered.
pub const BITRATES: [i64; 6] = [24, 32, 48, 64, 96, 128];

/// Name `device_choices` reports for a saved endpoint that is no longer listed.
/// The voice form compares against this shared constant for the danger styling,
/// so the label and its tone cannot drift apart.
pub const UNAVAILABLE_DEVICE: &str = "Устройство недоступно";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoicePrefs {
    pub input: String,
    pub output: String,
    pub input_volume: i64,
    pub output_volume: i64,
    pub bitrate: i64,
    pub noise_gate: i64,
    pub sound_volume: i64,
    pub echo_cancellation: bool,
    pub auto_gain: bool,
    pub sounds: bool,
    pub noise_mode: String,
    pub input_mode: String,
    pub mute: String,
    pub deafen: String,
    pub push_to_talk: String,
    /// Per-user room volume by the stable user id (0–200 %). The frontend
    /// stores the same map in `voxhold.voice.userVolumes.v1`; a connection id
    /// is never a key, so a reconnect cannot move a volume to another person.
    pub user_volumes: BTreeMap<i64, i64>,
}

impl Default for VoicePrefs {
    fn default() -> Self {
        Self {
            input: String::new(),
            output: String::new(),
            input_volume: 100,
            output_volume: 100,
            bitrate: 64,
            noise_gate: 15,
            sound_volume: 60,
            echo_cancellation: true,
            auto_gain: true,
            sounds: true,
            noise_mode: "auto".into(),
            input_mode: "voice_activity".into(),
            mute: "Ctrl+Shift+M".into(),
            deafen: "Ctrl+Shift+D".into(),
            push_to_talk: "Space".into(),
            user_volumes: BTreeMap::new(),
        }
    }
}

/// One normalization point for values that arrive from disk, from the form or
/// from a reset. Ranges, the bitrate whitelist, mode names and empty shortcut
/// strings are all decided here so `from_json` and `save` cannot disagree.
impl VoicePrefs {
    pub fn normalize(&mut self) {
        self.input_volume = self.input_volume.clamp(0, 200);
        self.output_volume = self.output_volume.clamp(0, 200);
        self.sound_volume = self.sound_volume.clamp(0, 100);
        self.noise_gate = self.noise_gate.clamp(0, 100);
        if !BITRATES.contains(&self.bitrate) {
            self.bitrate = 64;
        }
        self.noise_mode = match self.noise_mode.as_str() {
            "threshold" => "threshold",
            _ => "auto",
        }
        .into();
        self.input_mode = match self.input_mode.as_str() {
            "push_to_talk" => "push_to_talk",
            _ => "voice_activity",
        }
        .into();
        normalize_shortcut(&mut self.mute, "Ctrl+Shift+M");
        normalize_shortcut(&mut self.deafen, "Ctrl+Shift+D");
        normalize_shortcut(&mut self.push_to_talk, "Space");
        self.user_volumes.retain(|user, _| *user > 0);
        for volume in self.user_volumes.values_mut() {
            *volume = (*volume).clamp(0, 200);
        }
    }

    pub fn from_json(value: &Value) -> Self {
        let number = |key: &str, fallback: i64| value[key].as_i64().unwrap_or(fallback);
        let mut prefs = Self {
            input: value["Input"].as_str().unwrap_or_default().to_owned(),
            output: value["Output"].as_str().unwrap_or_default().to_owned(),
            input_volume: number("InputVolume", 100),
            output_volume: number("OutputVolume", 100),
            bitrate: number("Bitrate", 64),
            noise_gate: number("NoiseGate", 15),
            sound_volume: number("SoundVolume", 60),
            echo_cancellation: value["EchoCancellation"].as_bool().unwrap_or(true),
            auto_gain: value["AutoGain"].as_bool().unwrap_or(true),
            sounds: value["Sounds"].as_bool().unwrap_or(true),
            noise_mode: value["NoiseMode"].as_str().unwrap_or("auto").to_owned(),
            input_mode: value["InputMode"]
                .as_str()
                .unwrap_or("voice_activity")
                .to_owned(),
            mute: value["Mute"].as_str().unwrap_or_default().to_owned(),
            deafen: value["Deafen"].as_str().unwrap_or_default().to_owned(),
            push_to_talk: value["PushToTalk"].as_str().unwrap_or_default().to_owned(),
            user_volumes: value["UserVolumes"]
                .as_object()
                .map(|volumes| {
                    volumes
                        .iter()
                        .filter_map(|(key, value)| {
                            Some((key.parse::<i64>().ok()?, value.as_i64()?))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        prefs.normalize();
        prefs
    }

    pub fn to_json(&self) -> Value {
        json!({
            "Input": self.input,
            "Output": self.output,
            "InputVolume": self.input_volume,
            "OutputVolume": self.output_volume,
            "Bitrate": self.bitrate,
            "NoiseGate": self.noise_gate,
            "SoundVolume": self.sound_volume,
            "EchoCancellation": self.echo_cancellation,
            "AutoGain": self.auto_gain,
            "Sounds": self.sounds,
            "NoiseMode": self.noise_mode,
            "InputMode": self.input_mode,
            "Mute": self.mute,
            "Deafen": self.deafen,
            "PushToTalk": self.push_to_talk,
            "UserVolumes": self
                .user_volumes
                .iter()
                .map(|(user, volume)| (user.to_string(), json!(volume)))
                .collect::<serde_json::Map<_, _>>(),
        })
    }

    /// Duplicate shortcuts are rejected by the Go form. Comparison ignores
    /// modifier spelling and order, so `Ctrl+Shift+M` and `Shift+Control+M`
    /// are the same binding.
    pub fn shortcuts_valid(&self) -> bool {
        let mute = canonical_shortcut(&self.mute);
        let deafen = canonical_shortcut(&self.deafen);
        let push_to_talk = canonical_shortcut(&self.push_to_talk);
        !mute.is_empty()
            && !deafen.is_empty()
            && !push_to_talk.is_empty()
            && mute != deafen
            && mute != push_to_talk
            && deafen != push_to_talk
    }

    /// Form `SetNoiseMode`. Switching modes must not touch the other mode's
    /// stored values (the gate, in this case), so both survive a round trip.
    pub fn set_noise_mode(&mut self, index: usize) {
        self.noise_mode = if index == 1 {
            "threshold".into()
        } else {
            "auto".into()
        };
    }

    /// Form `SetInputMode`. The push-to-talk combination is a separate field
    /// and stays stored while voice activity is active.
    pub fn set_input_mode(&mut self, index: usize) {
        self.input_mode = if index == 1 {
            "push_to_talk".into()
        } else {
            "voice_activity".into()
        };
    }
}

fn normalize_shortcut(value: &mut String, fallback: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        *value = fallback.to_owned();
    } else if trimmed.len() != value.len() {
        *value = trimmed.to_owned();
    }
}

/// One spelling for a shortcut: `ctrl+alt+shift+super+key`, modifiers sorted.
/// The form stores what the user pressed; conflict detection must not treat
/// `Control+M` and `Ctrl+m` as different bindings.
pub fn canonical_shortcut(text: &str) -> String {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut platform = false;
    let mut key = String::new();
    for part in text
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "super" | "win" | "cmd" | "meta" | "platform" => platform = true,
            _ => key = part.to_lowercase(),
        }
    }
    if key.is_empty() {
        return String::new();
    }
    let mut modifiers = Vec::new();
    if ctrl {
        modifiers.push("ctrl");
    }
    if alt {
        modifiers.push("alt");
    }
    if shift {
        modifiers.push("shift");
    }
    if platform {
        modifiers.push("super");
    }
    modifiers.push(&key);
    modifiers.join("+")
}

/// Go `shortcutLabel`: `Space` reads as `Пробел` and components are spaced.
pub fn shortcut_label(text: &str) -> String {
    text.split('+')
        .map(|part| match part.trim() {
            "Space" | "space" => "Пробел".to_owned(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

pub fn path_from_layout(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".voice.json");
    PathBuf::from(value)
}

impl VoicePrefs {
    pub fn load(path: Option<&Path>) -> Self {
        path.and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|value| Self::from_json(&value))
            .unwrap_or_default()
    }

    /// Atomic write: the normalized document goes to a sibling temp file and
    /// is renamed over the target. A failed write leaves the previous file in
    /// place and removes the temp file.
    pub fn save(&self, path: Option<&Path>) -> bool {
        let Some(path) = path else { return true };
        let mut prefs = self.clone();
        prefs.normalize();
        let text = prefs.to_json().to_string();
        write_atomic(path, &text)
    }
}

fn write_atomic(path: &Path, text: &str) -> bool {
    let Some(name) = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    let written = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return false;
    }
    true
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub input: bool,
}

/// Preference file, device list and the tab-lifetime preview state. The
/// preview handle only exists while the voice tab is open outside a call.
pub struct VoiceSettings {
    pub prefs: VoicePrefs,
    pub path: Option<PathBuf>,
    pub devices: Vec<Device>,
    pub devices_loading: bool,
    pub devices_error: String,
    pub saved_error: String,
    /// WinMM level preview owned by the open voice tab.
    #[cfg(feature = "voxhold-media")]
    pub preview: Option<voxhold_media::preview::LevelPreview>,
    /// Smoothed level shown by the meter (0..=1) after the input gain.
    pub preview_level: f32,
    pub preview_error: String,
    /// Invalidates a preview start that was resolved on the background
    /// executor and arrives after the tab closed or the device changed.
    pub preview_epoch: u64,
    /// A background `start_level` is in flight; a tick must not queue another.
    pub preview_pending: bool,
    /// Real device notice (hot-unplug) or an in-call test explanation.
    pub device_notice: String,
    /// Shortcut currently being recorded: 0 mute, 1 deafen, 2 push-to-talk.
    pub recording_shortcut: Option<usize>,
    /// The voice tab's «Подробнее» block. Collapsed by default; it only hides
    /// explanation rows, every control stays reachable outside it.
    pub diagnostics_open: bool,
    pub tone_pending: bool,
    pub tone_error: String,
    /// Bumped whenever the meter task must stop or restart.
    pub meter_epoch: u64,
    /// Monotonic token of the newest requested save plus the write lock. A
    /// snapshot is written only while its token is still the newest, so a late
    /// writer can never resurrect an older value.
    pub store: VoiceStore,
    /// Atomic writes that actually reached disk. Rapid slider steps must
    /// collapse into one; the coalescing regression reads this counter.
    pub writes: u64,
    /// Token of the newest write that reached the disk. A newer token means a
    /// save is still queued and `flush`/`Drop` must write it.
    pub saved_generation: u64,
    /// Device error already turned into `device_notice`, so a meter tick can
    /// tell "still broken" from "the engine recovered".
    pub device_error_shown: String,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            prefs: VoicePrefs::default(),
            path: None,
            devices: Vec::new(),
            devices_loading: false,
            devices_error: String::new(),
            saved_error: String::new(),
            #[cfg(feature = "voxhold-media")]
            preview: None,
            preview_level: 0.0,
            preview_error: String::new(),
            preview_epoch: 0,
            preview_pending: false,
            device_notice: String::new(),
            recording_shortcut: None,
            diagnostics_open: false,
            tone_pending: false,
            tone_error: String::new(),
            meter_epoch: 0,
            store: VoiceStore::default(),
            writes: 0,
            saved_generation: 0,
            device_error_shown: String::new(),
        }
    }
}

impl VoiceSettings {
    pub fn with_path(path: Option<PathBuf>) -> Self {
        let mut settings = Self::default();
        settings.prefs = VoicePrefs::load(path.as_deref());
        settings.path = path;
        settings
    }

    /// Synchronous save used by the preference tests. The live tab uses
    /// `Voxhold::persist_voice`, which debounces and runs off the UI thread.
    #[cfg(test)]
    pub fn persist(&mut self) {
        self.saved_error.clear();
        if self.prefs.save(self.path.as_deref()) {
            self.writes = self.writes.wrapping_add(1);
        } else {
            self.saved_error = "Не удалось сохранить настройки звука.".into();
        }
    }

    /// Write the newest queued snapshot immediately and return whether this
    /// call wrote. The debounced task cannot run while the view is going away,
    /// so the close path flushes here; taking the next token also cancels a
    /// timer that is still pending. `false` means nothing was pending.
    pub fn flush(&mut self) -> bool {
        if self.store.is_newest(self.saved_generation) {
            return false;
        }
        let generation = self.store.next();
        let written = self
            .store
            .save(generation, &self.prefs, self.path.as_deref());
        if written == Some(true) {
            self.saved_generation = generation;
            self.writes = self.writes.wrapping_add(1);
        }
        written.unwrap_or(false)
    }
}

impl Drop for VoiceSettings {
    /// The view owns this value, so its drop is the application's last chance
    /// to persist a value whose 150 ms debounce has not fired yet.
    fn drop(&mut self) {
        self.flush();
    }
}

/// The write pipeline for the voice preferences: a monotonic token shared with
/// the background writer plus the lock that serializes the atomic renames.
/// Cloneable, because the UI hands a copy to the executor.
#[derive(Clone, Default)]
pub struct VoiceStore {
    token: std::sync::Arc<std::sync::atomic::AtomicU64>,
    lock: std::sync::Arc<std::sync::Mutex<()>>,
}

impl VoiceStore {
    /// Take the token of a new save request.
    pub fn next(&self) -> u64 {
        self.token.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }

    pub fn is_newest(&self, generation: u64) -> bool {
        self.token.load(std::sync::atomic::Ordering::SeqCst) == generation
    }

    /// Write a queued snapshot only while `generation` is still the newest
    /// request. The token check and the rename are one critical section, so two
    /// background saves cannot reorder on disk; `None` means a newer edit
    /// superseded this one and the file must stay untouched. Blocking: call it
    /// from the background executor.
    pub fn save(&self, generation: u64, prefs: &VoicePrefs, path: Option<&Path>) -> Option<bool> {
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        if !self.is_newest(generation) {
            return None;
        }
        Some(prefs.save(path))
    }
}

/// Migrate a legacy WinMM `index:name` input preference to the stable endpoint
/// id. Only a unique name/prefix is migrated; duplicate names are left for the
/// user to choose again. Endpoint ids and empty values are unchanged.
pub fn migrate_legacy_input(input: &str, devices: &[Device]) -> Option<String> {
    if input.is_empty() {
        return None;
    }
    let (index, name) = input.split_once(':')?;
    if index.parse::<u32>().is_err() || name.is_empty() {
        return None;
    }
    if devices
        .iter()
        .any(|device| device.input && device.id == input)
    {
        return None;
    }
    let exact: Vec<_> = devices
        .iter()
        .filter(|device| device.input && device.name == name)
        .collect();
    if let [device] = exact.as_slice() {
        return Some(device.id.clone());
    }
    if !exact.is_empty() {
        return None;
    }
    let prefix: Vec<_> = devices
        .iter()
        .filter(|device| device.input && device.name.starts_with(name))
        .collect();
    match prefix.as_slice() {
        [device] => Some(device.id.clone()),
        _ => None,
    }
}

/// Migrate either legacy form of a saved input: the WinMM `index:name` value
/// or the older PnP interface id. New Core Audio endpoint ids pass through
/// unchanged.
pub fn migrate_saved_input(input: &str, devices: &[Device]) -> Option<String> {
    migrate_legacy_input(input, devices).or_else(|| {
        #[cfg(feature = "voxhold-media")]
        {
            voxhold_media::devices::canonical_input_id(input).filter(|id| id != input)
        }
        #[cfg(not(feature = "voxhold-media"))]
        {
            None
        }
    })
}

/// Device labels with the synthetic "system default" and an unavailable
/// selected id, exactly like `deviceChoice` in Go.
pub fn device_choices(
    prefs: &VoicePrefs,
    devices: &[Device],
    input: bool,
) -> (Vec<String>, Vec<String>, String) {
    let mut names = vec!["Системное по умолчанию".to_owned()];
    let mut ids = vec![String::new()];
    let selected = if input { &prefs.input } else { &prefs.output };
    let mut current = names[0].clone();
    for device in devices.iter().filter(|device| device.input == input) {
        if device.id == *selected {
            current = device.name.clone();
        }
        ids.push(device.id.clone());
        names.push(device.name.clone());
    }
    if !selected.is_empty() && !ids.contains(selected) {
        current = UNAVAILABLE_DEVICE.to_owned();
    }
    (names, ids, current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "voxhold-voice-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory.join(name)
    }

    #[test]
    fn defaults_match_go_and_round_trip_through_json_and_file() {
        let prefs = VoicePrefs::default();
        assert_eq!(BITRATES, [24, 32, 48, 64, 96, 128]);
        assert_eq!(prefs.bitrate, 64);
        assert_eq!(prefs.noise_gate, 15);
        assert!(prefs.shortcuts_valid());
        assert_eq!(VoicePrefs::from_json(&prefs.to_json()), prefs);

        let path = scratch("prefs.voice.json");
        let mut saved = prefs.clone();
        saved.input = "1:Microphone".into();
        saved.output = "0:Speakers".into();
        saved.input_volume = 140;
        saved.output_volume = 80;
        saved.bitrate = 96;
        saved.noise_gate = 32;
        saved.noise_mode = "threshold".into();
        saved.input_mode = "push_to_talk".into();
        assert!(saved.save(Some(&path)));
        let loaded = VoicePrefs::load(Some(&path));
        assert_eq!(loaded, saved, "the file round trip keeps every field");
        assert!(!path.with_file_name(".prefs.voice.json.tmp").exists());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn clamps_bitrates_and_rejects_unknown_modes() {
        let clamped = VoicePrefs::from_json(&json!({
            "InputVolume": 999, "OutputVolume": -3, "SoundVolume": 400,
            "Bitrate": 160, "NoiseGate": 101, "NoiseMode": "bogus",
            "InputMode": "bogus", "Mute": "", "Deafen": "   ",
            "PushToTalk": ""
        }));
        assert_eq!(clamped.input_volume, 200);
        assert_eq!(clamped.output_volume, 0);
        assert_eq!(clamped.sound_volume, 100);
        assert_eq!(clamped.bitrate, 64, "160 is no longer a preset");
        assert_eq!(clamped.noise_gate, 100);
        assert_eq!(clamped.noise_mode, "auto");
        assert_eq!(clamped.input_mode, "voice_activity");
        assert_eq!(clamped.mute, "Ctrl+Shift+M");
        assert_eq!(clamped.deafen, "Ctrl+Shift+D");
        assert_eq!(clamped.push_to_talk, "Space");
        assert_eq!(VoicePrefs::from_json(&json!({"Bitrate": 128})).bitrate, 128);
    }

    #[test]
    fn both_mode_values_survive_switching() {
        // The form switches modes through these setters and then persists the
        // document; the stored gate and the push-to-talk key must stay with
        // their mode instead of being reset by the switch.
        let mut prefs = VoicePrefs::default();
        prefs.set_input_mode(1);
        prefs.set_noise_mode(1);
        prefs.noise_gate = 47;
        prefs.push_to_talk = "Ctrl+Space".into();

        let mut reopened = VoicePrefs::from_json(&prefs.to_json());
        assert_eq!(reopened.input_mode, "push_to_talk");
        assert_eq!(reopened.noise_mode, "threshold");

        reopened.set_noise_mode(0);
        assert_eq!(reopened.noise_gate, 47, "auto mode keeps the saved gate");
        reopened.set_input_mode(0);
        assert_eq!(
            reopened.push_to_talk, "Ctrl+Space",
            "voice activity keeps the push-to-talk key"
        );

        reopened.set_noise_mode(1);
        reopened.set_input_mode(1);
        assert_eq!(reopened.noise_mode, "threshold");
        assert_eq!(reopened.input_mode, "push_to_talk");
        assert_eq!(reopened.noise_gate, 47);
        assert_eq!(reopened.push_to_talk, "Ctrl+Space");
        assert_eq!(
            VoicePrefs::from_json(&reopened.to_json()),
            reopened,
            "both modes and their values round trip"
        );
    }

    #[test]
    fn reset_clears_input_and_output_like_go() {
        let mut prefs = VoicePrefs::default();
        prefs.input = "1:Microphone".into();
        prefs.output = "0:Speakers".into();
        let reset = VoicePrefs::default();
        assert!(reset.input.is_empty() && reset.output.is_empty());
        assert_eq!(reset, VoicePrefs::from_json(&reset.to_json()));
    }

    #[test]
    fn an_atomic_rewrite_leaves_no_temp_file() {
        let path = scratch("rewrite.voice.json");
        let mut prefs = VoicePrefs::default();
        prefs.input_volume = 120;
        assert!(prefs.save(Some(&path)));
        prefs.input_volume = 40;
        assert!(prefs.save(Some(&path)));
        assert_eq!(VoicePrefs::load(Some(&path)).input_volume, 40);
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["rewrite.voice.json".to_owned()]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_failed_write_reports_false_and_keeps_the_previous_file() {
        let path = scratch("keep.voice.json");
        let mut prefs = VoicePrefs::default();
        prefs.sound_volume = 30;
        assert!(prefs.save(Some(&path)));
        // Block the sibling temp path with a directory so the atomic write
        // cannot start while the previous document stays untouched.
        let blocker = path.with_file_name(".keep.voice.json.tmp");
        std::fs::create_dir_all(&blocker).unwrap();
        prefs.sound_volume = 90;
        assert!(!prefs.save(Some(&path)));
        assert_eq!(VoicePrefs::load(Some(&path)).sound_volume, 30);
        let mut settings = VoiceSettings::with_path(Some(path.clone()));
        settings.prefs.sound_volume = 90;
        settings.persist();
        assert!(!settings.saved_error.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_superseded_snapshot_never_reaches_the_file() {
        let path = scratch("ordering.voice.json");
        let mut settings = VoiceSettings::with_path(Some(path.clone()));
        let generation = settings.store.next();
        let mut newest = VoicePrefs::default();
        newest.input_volume = 140;
        assert_eq!(
            settings.store.save(generation, &newest, Some(&path)),
            Some(true)
        );
        settings.saved_generation = generation;
        // A later edit takes the next token before the older write reaches the
        // disk; the older snapshot must not touch the file any more.
        settings.store.next();
        assert!(!settings.store.is_newest(generation));
        let mut stale = VoicePrefs::default();
        stale.input_volume = 10;
        assert_eq!(
            settings.store.save(generation, &stale, Some(&path)),
            None,
            "the superseded snapshot is dropped"
        );
        assert_eq!(VoicePrefs::load(Some(&path)).input_volume, 140);
        // The test-specific token bump above is not a pending save: the drop
        // flush would write the (default) snapshot of this fixture otherwise.
        settings.prefs = newest.clone();
        drop(settings);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_queued_save_is_flushed_when_the_settings_are_dropped() {
        let path = scratch("flush.voice.json");
        {
            let mut settings = VoiceSettings::with_path(Some(path.clone()));
            settings.prefs.input_volume = 170;
            // A persist_voice call took a newer token but the 150 ms debounce
            // has not fired; nothing has reached the disk yet.
            settings.store.next();
            assert!(!path.exists(), "the debounced write has not run");
        }
        assert_eq!(
            VoicePrefs::load(Some(&path)).input_volume,
            170,
            "dropping the settings (and the view with them) flushes the value"
        );
        // A second drop of an already saved value does not touch the file.
        let mut settings = VoiceSettings::with_path(Some(path.clone()));
        assert!(!settings.flush(), "nothing is pending after a load");
        drop(settings);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn broken_json_falls_back_to_defaults() {
        let path = scratch("broken.voice.json");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(VoicePrefs::load(Some(&path)), VoicePrefs::default());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn duplicate_shortcuts_are_rejected() {
        let mut prefs = VoicePrefs::default();
        prefs.mute = prefs.deafen.clone();
        assert!(!prefs.shortcuts_valid());
    }

    #[test]
    fn shortcut_conflicts_ignore_modifier_spelling_and_order() {
        let mut prefs = VoicePrefs::default();
        prefs.mute = "Control+Shift+m".into();
        prefs.deafen = "Shift+Ctrl+M".into();
        assert!(
            !prefs.shortcuts_valid(),
            "the same binding in another spelling is still a conflict"
        );
        prefs.deafen = "Ctrl+Shift+D".into();
        assert!(prefs.shortcuts_valid());
        assert_eq!(canonical_shortcut("Win+M"), "super+m");
        assert_eq!(canonical_shortcut("M"), "m");
        assert_eq!(canonical_shortcut("Ctrl+"), "");
        assert_eq!(shortcut_label("Ctrl+Shift+Space"), "Ctrl + Shift + Пробел");
    }

    #[test]
    fn user_volumes_are_keyed_by_user_id_and_bounded() {
        let mut prefs = VoicePrefs::default();
        prefs.user_volumes.insert(7, 150);
        prefs.user_volumes.insert(8, 999);
        prefs.user_volumes.insert(0, 50);
        prefs.user_volumes.insert(-3, 50);
        let loaded = VoicePrefs::from_json(&prefs.to_json());
        assert_eq!(loaded.user_volumes.get(&7), Some(&150));
        assert_eq!(
            loaded.user_volumes.get(&8),
            Some(&200),
            "the stored value is normalized"
        );
        assert!(!loaded.user_volumes.contains_key(&0));
        assert!(!loaded.user_volumes.contains_key(&-3));
        // A malformed map cannot invent a user or a volume.
        let broken = VoicePrefs::from_json(&json!({
            "UserVolumes": {"alice": 40, "9": "loud", "10": 140}
        }));
        assert_eq!(broken.user_volumes.get(&10), Some(&140));
        assert_eq!(broken.user_volumes.len(), 1);
    }

    #[test]
    fn legacy_winmm_inputs_migrate_only_when_unambiguous() {
        let devices = vec![
            Device {
                id: "endpoint-0".into(),
                name: "USB Mic".into(),
                input: true,
            },
            Device {
                id: "endpoint-1".into(),
                name: "USB Mic".into(),
                input: true,
            },
            Device {
                id: "endpoint-2".into(),
                name: "Headset".into(),
                input: true,
            },
        ];
        assert_eq!(
            migrate_legacy_input("0:Headset", &devices),
            Some("endpoint-2".into())
        );
        assert_eq!(
            migrate_legacy_input("0:USB Mic", &devices),
            None,
            "identical names require an explicit reselect"
        );
        assert_eq!(
            migrate_legacy_input("endpoint-0", &devices),
            None,
            "new endpoint ids are not rewritten"
        );
    }

    #[test]
    fn device_choices_keep_default_first_and_report_missing() {
        let mut prefs = VoicePrefs::default();
        prefs.input = "gone".into();
        let devices = vec![
            Device {
                id: "mic-1".into(),
                name: "Microphone".into(),
                input: true,
            },
            Device {
                id: "spk-1".into(),
                name: "Speakers".into(),
                input: false,
            },
        ];
        let (names, ids, select) = device_choices(&prefs, &devices, true);
        assert_eq!(names, vec!["Системное по умолчанию", "Microphone"]);
        assert_eq!(ids, vec!["", "mic-1"]);
        assert_eq!(select, UNAVAILABLE_DEVICE);
        // The published contract stays the exact Go/frontend sentence.
        assert_eq!(UNAVAILABLE_DEVICE, "Устройство недоступно");
        assert_eq!(device_choices(&prefs, &devices, false).0[1], "Speakers");
    }
}
