//! Persisted voice preferences and the settings-tab model. The media engine and
//! device capture stay in `voxhold-media`; this module only stores choices and
//! lists devices, matching `settings_voice.go` and `workspace-layout.json.voice.json`.

use std::path::PathBuf;

use serde_json::{json, Value};

pub const BITRATES: [i64; 7] = [24, 32, 48, 64, 96, 128, 160];

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
        }
    }
}

impl VoicePrefs {
    pub fn from_json(value: &Value) -> Self {
        let mut prefs = Self::default();
        let text = |key: &str, fallback: &str| {
            value[key]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or(fallback)
                .to_owned()
        };
        let number = |key: &str, fallback: i64| value[key].as_i64().unwrap_or(fallback);
        prefs.input = value["Input"].as_str().unwrap_or_default().to_owned();
        prefs.output = value["Output"].as_str().unwrap_or_default().to_owned();
        prefs.input_volume = number("InputVolume", 100).clamp(0, 200);
        prefs.output_volume = number("OutputVolume", 100).clamp(0, 200);
        prefs.bitrate = number("Bitrate", 64);
        if !BITRATES.contains(&prefs.bitrate) {
            prefs.bitrate = 64;
        }
        prefs.noise_gate = number("NoiseGate", 15).clamp(0, 100);
        prefs.sound_volume = number("SoundVolume", 60).clamp(0, 100);
        prefs.echo_cancellation = value["EchoCancellation"].as_bool().unwrap_or(true);
        prefs.auto_gain = value["AutoGain"].as_bool().unwrap_or(true);
        prefs.sounds = value["Sounds"].as_bool().unwrap_or(true);
        prefs.noise_mode = match value["NoiseMode"].as_str() {
            Some("threshold") => "threshold".into(),
            _ => "auto".into(),
        };
        prefs.input_mode = match value["InputMode"].as_str() {
            Some("push_to_talk") => "push_to_talk".into(),
            _ => "voice_activity".into(),
        };
        prefs.mute = text("Mute", "Ctrl+Shift+M");
        prefs.deafen = text("Deafen", "Ctrl+Shift+D");
        prefs.push_to_talk = text("PushToTalk", "Space");
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
        })
    }

    /// Duplicate shortcuts are rejected by the Go form.
    pub fn shortcuts_valid(&self) -> bool {
        self.mute != self.deafen
            && self.mute != self.push_to_talk
            && self.deafen != self.push_to_talk
    }
}

pub fn path_from_layout(path: &std::path::Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".voice.json");
    PathBuf::from(value)
}

impl VoicePrefs {
    pub fn load(path: Option<&std::path::Path>) -> Self {
        path.and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|value| Self::from_json(&value))
            .unwrap_or_default()
    }

    pub fn save(&self, path: Option<&std::path::Path>) -> bool {
        let Some(path) = path else { return true };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, self.to_json().to_string()).is_ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub input: bool,
}

#[derive(Default)]
pub struct VoiceSettings {
    pub prefs: VoicePrefs,
    pub path: Option<PathBuf>,
    pub devices: Vec<Device>,
    pub devices_loading: bool,
    pub devices_error: String,
    pub saved_error: String,
}

impl VoiceSettings {
    pub fn with_path(path: Option<PathBuf>) -> Self {
        Self {
            prefs: VoicePrefs::load(path.as_deref()),
            path,
            devices: Vec::new(),
            devices_loading: false,
            devices_error: String::new(),
            saved_error: String::new(),
        }
    }

    pub fn persist(&mut self) {
        self.saved_error.clear();
        if !self.prefs.save(self.path.as_deref()) {
            self.saved_error = "Не удалось сохранить настройки звука.".into();
        }
    }
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
        current = "Устройство недоступно".to_owned();
    }
    (names, ids, current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip_match_go() {
        let prefs = VoicePrefs::default();
        assert_eq!(prefs.bitrate, 64);
        assert_eq!(prefs.noise_gate, 15);
        assert!(prefs.shortcuts_valid());
        let parsed = VoicePrefs::from_json(&prefs.to_json());
        assert_eq!(parsed, prefs);

        let clamped = VoicePrefs::from_json(&json!({
            "InputVolume": 999, "OutputVolume": -3, "SoundVolume": 400,
            "Bitrate": 999, "NoiseGate": 101, "NoiseMode": "bogus",
            "InputMode": "bogus"
        }));
        assert_eq!(clamped.input_volume, 200);
        assert_eq!(clamped.output_volume, 0);
        assert_eq!(clamped.sound_volume, 100);
        assert_eq!(clamped.bitrate, 64);
        assert_eq!(clamped.noise_gate, 100);
        assert_eq!(clamped.noise_mode, "auto");
        assert_eq!(clamped.input_mode, "voice_activity");
    }

    #[test]
    fn duplicate_shortcuts_are_rejected() {
        let mut prefs = VoicePrefs::default();
        prefs.mute = prefs.deafen.clone();
        assert!(!prefs.shortcuts_valid());
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
        assert_eq!(select, "Устройство недоступно");
        assert_eq!(device_choices(&prefs, &devices, false).0[1], "Speakers");
    }
}
