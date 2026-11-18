//! Settings → live-call controls. Kept next to the panel so the focus,
//! selection ids and player state stay in `voice_panel.rs`.

use super::*;

impl VoicePanel {
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
    ) {
        self.controls.input = input;
        self.controls.output = output;
        self.controls.bitrate_kbps = Some(bitrate);
        self.controls.output_gain = Some(output_volume.clamp(0, 200) as f64 / 100.0);
        self.controls.input_gain = input_volume.clamp(0, 200) as f32 / 100.0;
        self.controls.noise_mode = NoiseMode::parse(noise_mode);
        self.controls.noise_gate_percent = noise_gate.clamp(0, 100) as u8;
        self.controls.echo_cancellation = echo_cancellation;
        self.controls.auto_gain_control = auto_gain;
        self.refresh_capture();
    }
}
