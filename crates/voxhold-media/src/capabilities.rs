//! Honest map of which audio preferences actually change the captured or
//! played signal in this build. The settings form renders these values instead
//! of fake switches.
//!
//! R07b introduced a software WinMM capture path with one APM, a software gate
//! and input gain. Those values are applied in that path. The platform ADM
//! fallback is a different chain: it forces its own NS/AGC and cannot honour a
//! custom gate or gain, so the settings tab asks [`capabilities_for`] for the
//! active capture chain instead of promising the software values everywhere.
//! Echo cancellation is reported as unavailable because the pinned engine has
//! no render reference; the ADM's own AEC is the unmeasured platform default.

/// How a preference relates to the real audio path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// The value is forwarded to the engine and changes the signal.
    Applied,
    /// The platform applies its own processing; the UI cannot toggle it.
    Automatic,
    /// Only part of the stored range has an effect (for example 0 % closes).
    Limited,
    /// Stored for later, but it does not affect the current transmission.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioCapabilities {
    /// Microphone gain 0–200 %. Applied in the software capture path.
    pub input_gain: Support,
    /// Software threshold gate. Applied in the software capture path.
    pub noise_gate: Support,
    /// Echo cancellation. No render reference exists for the software APM.
    pub echo_cancellation: Support,
    /// WebRTC APM automatic gain in the software path.
    pub auto_gain: Support,
    /// APM noise suppression in auto mode; threshold uses the software gate.
    pub noise_suppression: Support,
    /// Room output gain: 0 closes the tracks, 1–200 is not applied yet (W02c).
    pub output_gain: Support,
    /// Opus maximum bitrate selection.
    pub bitrate: Support,
    /// WinMM microphone level preview.
    pub microphone_preview: Support,
    /// WinMM interface/test tone.
    pub output_test: Support,
}

/// The capabilities of this build before a call reports its capture chain.
pub fn audio_capabilities() -> AudioCapabilities {
    AudioCapabilities {
        input_gain: Support::Applied,
        noise_gate: Support::Applied,
        echo_cancellation: Support::Unavailable,
        auto_gain: Support::Applied,
        noise_suppression: Support::Applied,
        output_gain: Support::Limited,
        bitrate: Support::Applied,
        microphone_preview: preview_support(),
        output_test: tone_support(),
    }
}

/// Capabilities of the chain that is actually open. `software` is the WinMM
/// 48 kHz path; `platform` is the ADM fallback, where NS/AGC are forced and the
/// stored gate/gain are not applied.
pub fn capabilities_for(capture: Option<&str>) -> AudioCapabilities {
    let mut capabilities = audio_capabilities();
    if capture == Some("platform") {
        capabilities.input_gain = Support::Limited;
        capabilities.noise_gate = Support::Unavailable;
        capabilities.echo_cancellation = Support::Automatic;
        capabilities.auto_gain = Support::Automatic;
        capabilities.noise_suppression = Support::Automatic;
    }
    capabilities
}

#[cfg(windows)]
fn preview_support() -> Support {
    Support::Applied
}

#[cfg(not(windows))]
fn preview_support() -> Support {
    Support::Unavailable
}

#[cfg(windows)]
fn tone_support() -> Support {
    Support::Applied
}

#[cfg(not(windows))]
fn tone_support() -> Support {
    Support::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_software_path_applies_gate_gain_and_apm() {
        let capabilities = audio_capabilities();
        assert_eq!(capabilities.input_gain, Support::Applied);
        assert_eq!(capabilities.noise_gate, Support::Applied);
        assert_eq!(capabilities.auto_gain, Support::Applied);
        assert_eq!(capabilities.noise_suppression, Support::Applied);
        assert_ne!(
            capabilities.echo_cancellation,
            Support::Applied,
            "no render reference exists"
        );
        assert_eq!(capabilities.output_gain, Support::Limited);
        assert_eq!(capabilities.bitrate, Support::Applied);
    }

    #[test]
    fn the_platform_fallback_is_not_described_as_software() {
        let platform = capabilities_for(Some("platform"));
        assert_eq!(platform.input_gain, Support::Limited);
        assert_eq!(platform.noise_gate, Support::Unavailable);
        assert_eq!(platform.echo_cancellation, Support::Automatic);
        assert_eq!(platform.auto_gain, Support::Automatic);
        assert_eq!(platform.noise_suppression, Support::Automatic);
        assert_eq!(platform.bitrate, Support::Applied);

        let software = capabilities_for(Some("software"));
        assert_eq!(software.input_gain, Support::Applied);
        assert_eq!(software.noise_gate, Support::Applied);
        assert_eq!(software.echo_cancellation, Support::Unavailable);
    }

    #[cfg(windows)]
    #[test]
    fn windows_has_the_native_preview_and_test_tone() {
        let capabilities = audio_capabilities();
        assert_eq!(capabilities.microphone_preview, Support::Applied);
        assert_eq!(capabilities.output_test, Support::Applied);
    }

    #[cfg(not(windows))]
    #[test]
    fn other_targets_do_not_offer_the_windows_preview() {
        let capabilities = audio_capabilities();
        assert_eq!(capabilities.microphone_preview, Support::Unavailable);
        assert_eq!(capabilities.output_test, Support::Unavailable);
    }
}
