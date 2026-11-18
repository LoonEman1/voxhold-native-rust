//! Honest map of which audio preferences actually change the captured or
//! played signal in this build. The settings form renders these values instead
//! of fake switches.
//!
//! R07b introduced a software WinMM capture path with one APM, a software gate
//! and input gain. Those values are applied in that path. The platform ADM
//! fallback is a different chain: its own NS/AGC are not ours to toggle and it
//! cannot honour a custom gate or gain, so the settings tab asks
//! [`capabilities_for`] for the active capture chain instead of promising the
//! software values everywhere.
//!
//! Echo cancellation is reported as unavailable in both chains. The pinned
//! engine has no render reference for the software APM, and nothing measures
//! whether the ADM's own canceller is active, so the UI must not claim it.

/// How a preference relates to the real audio path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// The value is forwarded to the engine and changes the signal.
    Applied,
    /// The platform applies its own processing; the UI cannot toggle it and
    /// the build does not measure the result.
    Automatic,
    /// Stored for later, but it does not affect the current transmission.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioCapabilities {
    /// Microphone gain 0–200 %. Applied in the software capture path.
    pub input_gain: Support,
    /// Software threshold gate. Applied in the software capture path.
    pub noise_gate: Support,
    /// Echo cancellation. Unavailable: no render reference exists for the
    /// software APM and the ADM's own canceller is not measured.
    pub echo_cancellation: Support,
    /// WebRTC APM automatic gain in the software path; the ADM applies its own
    /// unmeasured AGC in the platform fallback.
    pub auto_gain: Support,
    /// APM noise suppression in auto mode; threshold uses the software gate.
    pub noise_suppression: Support,
    /// Room output gain. Applied to the participant tracks in every capture
    /// path: 0 closes them, 1–200 scales them. Per-participant volumes, the
    /// limiter and the voice/system/preview mix arrive with W02c.
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
        output_gain: Support::Applied,
        bitrate: Support::Applied,
        microphone_preview: preview_support(),
        output_test: tone_support(),
    }
}

/// Capabilities of the chain that is actually open. `software` is the WinMM
/// 48 kHz path; `platform` is the ADM fallback, where NS/AGC belong to the ADM
/// and the stored gate/gain are not applied. Neither chain reports echo
/// cancellation as available: the software APM has no render reference and the
/// ADM canceller is not measured.
pub fn capabilities_for(capture: Option<&str>) -> AudioCapabilities {
    let mut capabilities = audio_capabilities();
    if capture == Some("platform") {
        capabilities.input_gain = Support::Unavailable;
        capabilities.noise_gate = Support::Unavailable;
        capabilities.echo_cancellation = Support::Unavailable;
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
        assert_eq!(
            capabilities.output_gain,
            Support::Applied,
            "the controller scales every participant track by the output volume"
        );
        assert_eq!(capabilities.bitrate, Support::Applied);
    }

    #[test]
    fn the_platform_fallback_is_not_described_as_software() {
        let platform = capabilities_for(Some("platform"));
        assert_eq!(
            platform.input_gain,
            Support::Unavailable,
            "the ADM ignores the stored input gain"
        );
        assert_eq!(platform.noise_gate, Support::Unavailable);
        assert_eq!(
            platform.echo_cancellation,
            Support::Unavailable,
            "the ADM canceller is not measured"
        );
        assert_eq!(platform.auto_gain, Support::Automatic);
        assert_eq!(platform.noise_suppression, Support::Automatic);
        assert_eq!(platform.output_gain, Support::Applied);
        assert_eq!(platform.bitrate, Support::Applied);

        let software = capabilities_for(Some("software"));
        assert_eq!(software.input_gain, Support::Applied);
        assert_eq!(software.noise_gate, Support::Applied);
        assert_eq!(software.echo_cancellation, Support::Unavailable);
    }

    #[test]
    fn echo_cancellation_is_never_promised_in_any_capture_path() {
        for capture in [None, Some("software"), Some("platform"), Some("bogus")] {
            assert_ne!(
                capabilities_for(capture).echo_cancellation,
                Support::Applied,
                "AEC needs a measured render reference, got {capture:?}"
            );
        }
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
