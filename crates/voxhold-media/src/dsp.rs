//! PCM processing between capture and Opus.
//!
//! Ported from the local Go `media/src/dsp.rs` (blob
//! `c9d55ce09233d444473390b990651bf94ca5202b`, base `09c82b9`). The gate is a
//! port of the web `VoiceGateDSP` sample-clock gate. A block is one 10 ms frame
//! at the capture sample rate, so UI timers never decide what leaves the
//! encoder. Levels reported here are measured before the gate: the meter must
//! be able to show a quiet background while nothing is transmitted.

/// Web gate hysteresis around the configured threshold.
const HYSTERESIS: f32 = 0.02;
/// Web gate keeps the door open this long after the level drops.
const HOLD_SECONDS: f32 = 0.18;
const ATTACK_SECONDS: f32 = 0.003;
const RELEASE_SECONDS: f32 = 0.06;
/// Web meter curve: RMS is scaled so ordinary speech fills most of the bar.
const METER_GAIN: f32 = 4.5;

/// Wire values are `"auto"` and `"threshold"`; anything else is the web
/// default, `auto`. Keep this in sync with the settings model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoiseMode {
    #[default]
    Auto,
    Threshold,
}

impl NoiseMode {
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("threshold") {
            Self::Threshold
        } else {
            Self::Auto
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Threshold => "threshold",
        }
    }
}

/// DSP snapshot shared with the capture thread. Ranges match the web slider:
/// `gate_percent` 0..=100 and `input_gain` 0..=2 (0..200 %).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioParams {
    pub mode: NoiseMode,
    /// Bottom bar position in percent. Zero bypasses the software gate.
    pub gate_percent: u8,
    /// Microphone volume, 0..2 like the web slider (0..200%).
    pub input_gain: f32,
    pub echo_cancellation: bool,
    pub auto_gain_control: bool,
    pub muted: bool,
}

impl Default for AudioParams {
    fn default() -> Self {
        Self {
            mode: NoiseMode::Auto,
            gate_percent: 15,
            input_gain: 1.0,
            echo_cancellation: true,
            auto_gain_control: true,
            muted: false,
        }
    }
}

impl AudioParams {
    pub fn clamped(mut self) -> Self {
        self.gate_percent = self.gate_percent.min(100);
        self.input_gain = if self.input_gain.is_finite() {
            self.input_gain.clamp(0.0, 2.0)
        } else {
            1.0
        };
        self
    }

    pub fn threshold(&self) -> f32 {
        f32::from(self.gate_percent) / 100.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessingKey {
    pub noise_suppression: bool,
    pub echo_cancellation: bool,
    pub auto_gain_control: bool,
}

impl ProcessingKey {
    pub fn from_params(params: &AudioParams) -> Self {
        Self {
            noise_suppression: params.mode == NoiseMode::Auto,
            echo_cancellation: params.echo_cancellation,
            auto_gain_control: params.auto_gain_control,
        }
    }
}

/// Stateful noise gate. `process` is called once per 10 ms block and returns
/// the pre-gate meter level in 0..1.
#[derive(Default)]
pub struct NoiseGate {
    gain: f32,
    hold_samples: u64,
}

impl NoiseGate {
    pub fn process(&mut self, samples: &mut [i16], sample_rate: u32, threshold: f32) -> f32 {
        let level = meter_level(samples);
        if threshold <= 0.0 {
            // Zero threshold is a true bypass: no attack ramp, the first
            // sample is already at full level.
            return level;
        }
        let open = level > threshold + HYSTERESIS;
        let hold = (sample_rate as f32 * HOLD_SECONDS).round() as u64;
        // Below one hysteresis band there is no negative edge to reach. Use a
        // proportional close threshold so a 1–2 % bar can still release.
        let close = if threshold > HYSTERESIS {
            threshold - HYSTERESIS
        } else {
            threshold * 0.5
        };
        if open {
            self.hold_samples = hold;
        } else if level < close {
            self.hold_samples = self.hold_samples.saturating_sub(samples.len() as u64);
        }
        let target = if open || self.hold_samples > 0 {
            1.0
        } else {
            0.0
        };
        let seconds = if target > 0.0 {
            ATTACK_SECONDS
        } else {
            RELEASE_SECONDS
        };
        let coefficient = 1.0 - (-1.0 / (sample_rate as f32 * seconds)).exp();
        for sample in samples.iter_mut() {
            self.gain += (target - self.gain) * coefficient;
            let value = (f32::from(*sample) / 32_768.0) * self.gain;
            *sample = to_pcm(value);
        }
        level
    }

    /// Used by tests, mute and a capture restart, so the next block starts
    /// closed instead of carrying the previous hold.
    pub fn reset(&mut self) {
        self.gain = 0.0;
        self.hold_samples = 0;
    }
}

/// Applies microphone gain after the gate and returns the pre-clip peak.
pub fn apply_gain(samples: &mut [i16], gain: f32) -> f32 {
    let gain = if gain.is_finite() {
        gain.clamp(0.0, 2.0)
    } else {
        1.0
    };
    let mut peak = 0.0_f32;
    for sample in samples.iter_mut() {
        let value = (f32::from(*sample) / 32_768.0) * gain;
        peak = peak.max(value.abs());
        *sample = to_pcm(value);
    }
    peak
}

pub fn silence(samples: &mut [i16]) {
    samples.fill(0);
}

pub fn meter_level(samples: &[i16]) -> f32 {
    dbfs(samples).map_or(0.0, |db| {
        let rms = 10.0_f32.powf(db / 20.0);
        (rms * METER_GAIN).min(1.0)
    })
}

pub fn dbfs(samples: &[i16]) -> Option<f32> {
    if samples.is_empty() {
        return None;
    }
    let mut energy = 0.0_f64;
    for sample in samples {
        let value = f64::from(*sample) / 32_768.0;
        energy += value * value;
    }
    let rms = (energy / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        return None;
    }
    Some((20.0 * rms.log10()) as f32)
}

fn to_pcm(value: f32) -> i16 {
    (value.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

#[cfg(test)]
fn xorshift_noise(state: &mut u32) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    // Roughly -46 dBFS RMS, below the default 15% bar.
    ((*state as f32 / u32::MAX as f32) - 0.5) * 0.014
}

/// 48 kHz mono control sequence: silence, sub-threshold noise, a 440 Hz tone,
/// and the same noise again. Used by the gate and Opus loopback tests.
#[cfg(test)]
pub fn fixture(sample_rate: u32) -> Vec<i16> {
    let frames = |seconds: f32| (sample_rate as f32 * seconds) as usize;
    let mut samples = Vec::with_capacity(frames(3.5));
    let mut noise = 0x2545_f491_u32;
    for _ in 0..frames(0.5) {
        samples.push(0);
    }
    for _ in 0..frames(1.0) {
        samples.push(to_pcm(xorshift_noise(&mut noise)));
    }
    for index in 0..frames(1.0) {
        let t = index as f32 / sample_rate as f32;
        samples.push(to_pcm(
            0.25 * (2.0 * std::f32::consts::PI * 440.0 * t).sin(),
        ));
    }
    for _ in 0..frames(1.0) {
        samples.push(to_pcm(xorshift_noise(&mut noise)));
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const BLOCK: usize = (RATE / 100) as usize;

    fn gate_run(threshold_percent: u8, total_seconds: f32) -> (Vec<i16>, Vec<f32>) {
        let params = AudioParams {
            mode: NoiseMode::Threshold,
            gate_percent: threshold_percent,
            ..AudioParams::default()
        };
        let mut gate = NoiseGate::default();
        let samples = fixture(RATE);
        let blocks = (sample_rate_seconds(total_seconds) / BLOCK).min(samples.len() / BLOCK);
        let mut output = Vec::with_capacity(blocks * BLOCK);
        let mut levels = Vec::with_capacity(blocks);
        for index in 0..blocks {
            let mut block = samples[index * BLOCK..(index + 1) * BLOCK].to_vec();
            levels.push(gate.process(&mut block, RATE, params.threshold()));
            output.extend_from_slice(&block);
        }
        (output, levels)
    }

    fn sample_rate_seconds(seconds: f32) -> usize {
        (RATE as f32 * seconds) as usize
    }

    fn rms_db(samples: &[i16]) -> f32 {
        dbfs(samples).unwrap_or(-120.0)
    }

    fn section(samples: &[i16], from: f32, to: f32) -> Vec<i16> {
        let start = (RATE as f32 * from) as usize;
        let end = (RATE as f32 * to) as usize;
        samples[start..end].to_vec()
    }

    #[test]
    fn zero_threshold_passes_the_first_sample_unattenuated() {
        let mut gate = NoiseGate::default();
        let input = vec![10_000_i16; BLOCK];
        let mut block = input.clone();
        gate.process(&mut block, RATE, 0.0);
        assert_eq!(block, input, "0 % must bypass the attack ramp");
    }

    #[test]
    fn low_threshold_releases_after_speech() {
        for threshold in [0.01_f32, 0.02] {
            let mut gate = NoiseGate::default();
            let speech = vec![8_000_i16; BLOCK];
            // Meter level is below 0.5 % and below the 1–2 % bar.
            let background = vec![20_i16; BLOCK];
            for _ in 0..10 {
                let mut block = speech.clone();
                gate.process(&mut block, RATE, threshold);
            }
            for _ in 0..90 {
                let mut block = background.clone();
                gate.process(&mut block, RATE, threshold);
            }
            let mut block = background;
            gate.process(&mut block, RATE, threshold);
            assert!(
                block.iter().all(|sample| *sample == 0),
                "a {threshold} bar must close after the hold"
            );
        }
    }

    #[test]
    fn fixture_sections_have_the_expected_levels() {
        let samples = fixture(RATE);
        assert!(rms_db(&section(&samples, 0.0, 0.5)) < -100.0);
        assert!((-55.0..-40.0).contains(&rms_db(&section(&samples, 0.5, 1.5))));
        assert!((-20.0..-10.0).contains(&rms_db(&section(&samples, 1.5, 2.5))));
        assert!((-55.0..-40.0).contains(&rms_db(&section(&samples, 2.5, 3.5))));
    }

    #[test]
    fn threshold_gate_opens_on_speech_and_closes_on_noise() {
        let (output, levels) = gate_run(15, 3.5);
        let noise_before = rms_db(&section(&output, 0.8, 1.4));
        let speech = rms_db(&section(&output, 1.8, 2.2));
        let noise_after = rms_db(&section(&output, 3.1, 3.4));
        assert!(
            noise_before < -80.0,
            "pre-speech noise leaked: {noise_before}"
        );
        assert!(speech > -20.0, "speech was gated: {speech}");
        assert!(
            noise_after < -80.0,
            "post-speech noise leaked: {noise_after}"
        );
        assert!(levels[120] < 0.15, "noise meter must stay below the bar");
        assert!(levels[200] > 0.15, "speech meter must cross the bar");
    }

    #[test]
    fn zero_threshold_bypasses_the_gate() {
        let (output, _) = gate_run(0, 1.5);
        let noise = section(&output, 0.6, 1.4);
        let input = section(&fixture(RATE), 0.6, 1.4);
        assert!((rms_db(&noise) - rms_db(&input)).abs() < 0.5);
    }

    #[test]
    fn hundred_threshold_keeps_quiet_background_closed() {
        let (output, levels) = gate_run(100, 1.5);
        assert!(levels.iter().all(|level| *level <= 1.0));
        assert!(rms_db(&section(&output, 0.5, 1.4)) < -80.0);
    }

    #[test]
    fn level_inside_hysteresis_band_does_not_open_the_gate() {
        let mut gate = NoiseGate::default();
        let params = AudioParams {
            mode: NoiseMode::Threshold,
            gate_percent: 15,
            ..AudioParams::default()
        };
        let mut block = vec![0_i16; BLOCK];
        for sample in &mut block {
            // Level = min(1, RMS * 4.5), so this sits at the 15% bar.
            *sample = 1_090;
        }
        for _ in 0..20 {
            let mut copy = block.clone();
            gate.process(&mut copy, RATE, params.threshold());
            assert_eq!(copy, vec![0_i16; BLOCK]);
        }
    }

    #[test]
    fn hold_keeps_the_tail_of_speech_and_then_releases() {
        let mut gate = NoiseGate::default();
        let params = AudioParams {
            mode: NoiseMode::Threshold,
            gate_percent: 15,
            ..AudioParams::default()
        };
        let speech = vec![8_000_i16; BLOCK];
        let background = vec![200_i16; BLOCK];
        for _ in 0..10 {
            let mut block = speech.clone();
            gate.process(&mut block, RATE, params.threshold());
        }
        let mut tail = background.clone();
        gate.process(&mut tail, RATE, params.threshold());
        assert!(
            tail.iter().any(|sample| *sample != 0),
            "hold dropped the tail immediately"
        );
        for _ in 0..90 {
            let mut block = background.clone();
            gate.process(&mut block, RATE, params.threshold());
        }
        let mut block = background;
        gate.process(&mut block, RATE, params.threshold());
        assert!(
            block.iter().all(|sample| *sample == 0),
            "gate did not close after the hold"
        );
    }

    #[test]
    fn gain_and_mute_apply_after_the_gate() {
        let mut samples = vec![10_000_i16; BLOCK];
        let peak = apply_gain(&mut samples, 2.0);
        assert!((peak - 0.6104).abs() < 0.01);
        assert!(
            samples
                .iter()
                .all(|sample| (19_990..=20_010).contains(sample)),
            "{:?}",
            &samples[..4]
        );
        silence(&mut samples);
        assert!(samples.iter().all(|sample| *sample == 0));
    }

    #[test]
    fn processing_key_follows_noise_mode() {
        let mut params = AudioParams::default();
        assert!(ProcessingKey::from_params(&params).noise_suppression);
        params.mode = NoiseMode::Threshold;
        assert!(!ProcessingKey::from_params(&params).noise_suppression);
        params.auto_gain_control = false;
        assert!(!ProcessingKey::from_params(&params).auto_gain_control);
    }

    #[test]
    fn thresholds_are_clamped_to_the_slider() {
        let params = AudioParams {
            gate_percent: 200,
            input_gain: 9.0,
            ..AudioParams::default()
        }
        .clamped();
        assert_eq!(params.gate_percent, 100);
        assert_eq!(params.input_gain, 2.0);
        assert_eq!(
            NoiseMode::parse("THRESHOLD"),
            NoiseMode::Threshold,
            "wire value is case-insensitive"
        );
        assert_eq!(NoiseMode::parse("bogus"), NoiseMode::Auto);
    }

    /// Publishes the fixture numbers used in the task report and proves that
    /// one APM with NS enabled attenuates the sub-threshold noise section.
    #[test]
    fn dsp_report_and_auto_noise_reduction() {
        use libwebrtc::native::apm::AudioProcessingModule;
        use std::time::Instant;

        let input = fixture(RATE);
        let mut apm = AudioProcessingModule::new(false, false, true, true);
        let mut processed = Vec::with_capacity(input.len());
        let mut block = [0_i16; BLOCK];
        for chunk in input.chunks(BLOCK) {
            if chunk.len() < BLOCK {
                break;
            }
            block.copy_from_slice(chunk);
            apm.process_stream(&mut block, RATE as i32, 1).unwrap();
            processed.extend_from_slice(&block);
        }
        let sections = [
            ("silence", 0.1, 0.4),
            ("noise 1", 0.6, 1.4),
            ("tone", 1.6, 2.4),
            ("noise 2", 2.6, 3.4),
        ];
        for (name, from, to) in sections {
            let a = section(&input, from, to);
            let b = section(&processed, from, to);
            println!(
                "{name:8} input {:6.1} dBFS  auto-NS {:6.1} dBFS",
                rms_db(&a),
                rms_db(&b)
            );
        }
        assert!(rms_db(&section(&processed, 0.6, 1.4)) < -55.0);
        assert!(rms_db(&section(&processed, 2.6, 3.4)) < -55.0);

        let started = Instant::now();
        let mut gate = NoiseGate::default();
        let params = AudioParams {
            mode: NoiseMode::Threshold,
            gate_percent: 15,
            input_gain: 1.0,
            ..AudioParams::default()
        };
        let rounds = 10;
        for _ in 0..rounds {
            for chunk in input.chunks(BLOCK) {
                if chunk.len() < BLOCK {
                    break;
                }
                block.copy_from_slice(chunk);
                apm.process_stream(&mut block, RATE as i32, 1).unwrap();
                gate.process(&mut block, RATE, params.threshold());
                apply_gain(&mut block, params.input_gain);
            }
        }
        let audio_seconds = rounds as f32 * input.len() as f32 / RATE as f32;
        println!(
            "APM+gate {:?} for {audio_seconds:.1} s of audio ({:.3}% of one core)",
            started.elapsed(),
            started.elapsed().as_secs_f64() / f64::from(audio_seconds) * 100.0
        );
    }
}
