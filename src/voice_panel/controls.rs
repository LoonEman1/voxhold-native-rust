//! Settings → live-call controls. Kept next to the panel so the focus,
//! selection ids and player state stay in `voice_panel.rs`.

use super::*;
use crate::audio_capabilities::OutputGainPath;
use voxhold_media::mixer::PlayoutPhase;

impl VoicePanel {
    /// The folded capture decision of the last `refresh_capture`: true means
    /// the microphone is closed (local mute, deafen, push-to-talk release, a
    /// blocking modal or an unconfirmed join). The in-call meter renders the
    /// same silence the transmission sends.
    pub fn input_muted(&self) -> bool {
        self.controls.muted
    }

    /// Explicit input selection. Even the same saved id is forwarded once, so
    /// a microphone that was missing at join (and fell back to the system
    /// device) can be retried without rejoining. The request has its own slot
    /// and generation, so a later PTT/DSP update cannot displace it.
    pub fn select_input(&mut self, input: Option<String>) {
        self.controls.input = input.clone();
        self.device_error.clear();
        if let Some(handle) = &self.handle {
            self.input_generation = handle.select_input(input);
        }
    }

    /// Explicit output selection from the settings tab. This is forwarded even
    /// when the cached value looks unchanged, because a hot-unplug may have
    /// moved the engine to a fallback endpoint before its notice was applied.
    pub fn select_output(&mut self, output: Option<String>) {
        self.controls.output = output.clone();
        self.device_error.clear();
        if let Some(handle) = &self.handle {
            handle.select_output(output);
        }
    }

    /// Microphone DSP from the settings tab. Ranges match the web sliders and
    /// are clamped again in the media crate. The live settings path sends all
    /// values in one `apply_voice_prefs` snapshot; this helper keeps the
    /// individual R07b regression tests explicit.
    #[cfg(test)]
    pub fn set_dsp(
        &mut self,
        noise_mode: &str,
        noise_gate: i64,
        input_volume: i64,
        echo_cancellation: bool,
        auto_gain: bool,
    ) {
        self.controls.noise_mode = NoiseMode::parse(noise_mode);
        self.controls.noise_gate_percent = noise_gate.clamp(0, 100) as u8;
        self.controls.input_gain = input_volume.clamp(0, 200) as f32 / 100.0;
        self.controls.echo_cancellation = echo_cancellation;
        self.controls.auto_gain_control = auto_gain;
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }

    /// One audio snapshot from the settings tab: devices, bitrate, both gains
    /// and the microphone DSP. It is the only State write for a settings
    /// change. Explicit input selection has its own generation and is sent by
    /// the caller with `select_input`; local mute and push-to-talk stay
    /// untouched.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_voice_prefs(
        &mut self,
        input: Option<String>,
        output: Option<String>,
        bitrate: u32,
        input_volume: i64,
        output_volume: i64,
        noise_mode: &str,
        noise_gate: i64,
        echo_cancellation: bool,
        auto_gain: bool,
        user_volumes: std::collections::BTreeMap<i64, i64>,
    ) {
        self.controls.input = input;
        self.controls.output = output;
        self.controls.bitrate_kbps = Some(bitrate);
        // Keep the stored percentage even while the ADM plays: the engine gets
        // `None` in that path, and the value is restored when the software
        // mixer returns.
        self.saved_output_gain = Some(output_volume.clamp(0, 200) as f64 / 100.0);
        self.controls.output_gain = self.output_gain_for_path();
        self.controls.input_gain = input_volume.clamp(0, 200) as f32 / 100.0;
        self.controls.noise_mode = NoiseMode::parse(noise_mode);
        self.controls.noise_gate_percent = noise_gate.clamp(0, 100) as u8;
        self.controls.echo_cancellation = echo_cancellation;
        self.controls.auto_gain_control = auto_gain;
        self.user_volumes = user_volumes
            .into_iter()
            .filter(|(user, _)| *user > 0)
            .map(|(user, volume)| (user, volume.clamp(0, 200)))
            .collect();
        self.controls.user_volumes = self.user_volumes.clone();
        self.refresh_capture();
    }

    /// Which path owns room playback right now, as far as the numeric gain is
    /// concerned. The capture chain is not the answer: a software microphone
    /// can be played by the ADM, and a platform microphone can coexist with
    /// the running software mixer.
    pub fn output_gain_path(&self) -> OutputGainPath {
        let Some(audio) = self.applied_audio.as_ref() else {
            // No path reported yet (outside a call or before the first
            // `Audio` event): the software mixer is the default chain and the
            // stored percentage is what the next join will use.
            return OutputGainPath::Software;
        };
        if self.power_paused {
            // Suspend released the sink; neither the mixer nor the ADM
            // plays the room until the resume handler reopens it.
            return OutputGainPath::Unavailable;
        }
        if let Some(stats) = &self.playout {
            if stats.phase == PlayoutPhase::AdmFallback {
                // The recovery budget was spent; the platform ADM owns the
                // room and only the real mute/deafen applies.
                return OutputGainPath::Adm;
            }
            if stats.running {
                return OutputGainPath::Software;
            }
            // The mixer opened before and is now in its bounded
            // Opening/Unstable retry. The `software_mixer` flag in the stale
            // `Audio` report is false there, but the ADM does not play the
            // room either: the numeric gain is only temporarily unavailable.
            return OutputGainPath::Unavailable;
        }
        if audio.software_mixer {
            // A live flag without a playout report yet: the mixer is starting.
            OutputGainPath::Software
        } else {
            // The session never opened a software mixer; the ADM plays.
            OutputGainPath::Adm
        }
    }

    /// The room percentage that reaches the engine. The ADM has no numeric bus
    /// gain, so it gets `None` and only the real mute/deafen flag applies. A
    /// transiently unavailable mixer keeps the stored value, which the mixer
    /// reads when it reopens.
    fn output_gain_for_path(&self) -> Option<f64> {
        match self.output_gain_path() {
            OutputGainPath::Adm => None,
            OutputGainPath::Software | OutputGainPath::Unavailable => self.saved_output_gain,
        }
    }

    /// Whether the stored room percentage is applied by the active path. The
    /// settings slider and its sentence use the same decision as the sent
    /// controls, so they cannot promise an effect the path ignores.
    pub fn output_gain_applied(&self) -> bool {
        self.output_gain_path().applies_numeric_gain()
    }

    /// Re-evaluate the room gain after the engine reported another playback
    /// path and forward the snapshot if it changed. This is what restores the
    /// stored percentage when the software mixer returns without reopening
    /// the settings tab.
    pub(super) fn sync_output_gain(&mut self) -> bool {
        let gain = self.output_gain_for_path();
        if self.controls.output_gain == gain {
            return false;
        }
        self.controls.output_gain = gain;
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
        true
    }

    /// The call ended or is being rebuilt: the saved percentage must be ready
    /// for the next join instead of staying cleared from the ADM path.
    pub(super) fn restore_output_gain(&mut self) {
        self.controls.output_gain = self.saved_output_gain;
    }

    /// Per-user loudness chosen in the settings tab. The map is keyed by the
    /// stable user id; the media controller resolves it through the current
    /// track owner, so it cannot move to another participant on reconnect.
    pub fn user_volume(&self, user_id: i64) -> i64 {
        self.user_volumes.get(&user_id).copied().unwrap_or(100)
    }

    /// Roster of the active call with display names, for the per-user volume
    /// rows. Empty when the call is not active.
    pub fn call_participants(&self) -> Vec<(i64, String)> {
        if !self.call_active() || !self.access_confirmed {
            return Vec::new();
        }
        let mut seen = std::collections::HashSet::new();
        self.participants
            .iter()
            .filter(|participant| seen.insert(participant.user_id))
            .map(|participant| (participant.user_id, self.member_name(participant.user_id)))
            .collect()
    }

    /// Measured post-mix level and counters of the software mixer, when the
    /// room session runs on it.
    pub fn playout_stats(&self) -> Option<voxhold_media::mixer::PlayoutStats> {
        self.playout
    }

    /// Whether the numeric participant gain is applied by the software mixer
    /// right now. The applied capture snapshot alone is not enough: the sink can
    /// stop after it was reported, so the live playout report must agree.
    pub fn software_mixer(&self) -> bool {
        self.applied_audio
            .as_ref()
            .is_some_and(|audio| audio.software_mixer)
            && self.playout.as_ref().is_none_or(|stats| stats.running)
    }

    /// System suspend/resume. A suspend releases the hold so a suspended
    /// system never keeps the microphone open; a resume forwards the request
    /// to the media worker, which reopens the sink and re-checks the devices.
    pub fn power(&mut self, resumed: bool, cx: &mut Context<Self>) {
        self.power_paused = !resumed;
        self.release_push_to_talk();
        if let Some(handle) = &self.handle {
            handle.power(resumed);
        }
        self.refresh_capture();
        cx.notify();
    }

    #[cfg(test)]
    pub fn test_power_paused(&self) -> bool {
        self.power_paused
    }
}
