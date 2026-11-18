//! Settings → live-call controls. Kept next to the panel so the focus,
//! selection ids and player state stay in `voice_panel.rs`.

use super::*;

impl VoicePanel {
    /// Device selection from the settings tab. The engine switches the live
    /// path and keeps the previous one if the new device fails.
    pub fn set_devices(&mut self, input: Option<String>, output: Option<String>) {
        self.controls.input = input;
        self.controls.output = output;
        self.device_error.clear();
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }

    /// Explicit input selection. Even the same saved id is forwarded once, so
    /// a microphone that was missing at join (and fell back to the system
    /// device) can be retried without rejoining. The request has its own slot
    /// and generation, so a later PTT/DSP update cannot displace it.
    pub fn select_input(&mut self, input: Option<String>) {
        self.controls.input = input.clone();
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

    /// Encoder bitrate from the settings tab.
    pub fn set_bitrate(&mut self, kbps: u32) {
        self.controls.bitrate_kbps = Some(kbps);
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }

    /// Microphone DSP from the settings tab. Ranges match the web sliders and
    /// are clamped again in the media crate.
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

    /// Room output gain from the settings tab (0–200 %).
    pub fn set_output_volume(&mut self, percent: i64) {
        self.controls.output_gain = Some(percent.clamp(0, 200) as f64 / 100.0);
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }
}
