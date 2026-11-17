//! Honest map of which audio preferences actually change the captured or
//! played signal in this build. The settings form renders these values instead
//! of fake switches.
//!
//! R07b (the PCM capture → APM → gate → gain path) is not in `dev` yet. The
//! pinned `libwebrtc` 0.4.0 platform ADM applies its own AEC/NS/AGC and offers
//! no per-connection PCM hook; `builtin_*` switches only describe Windows
//! hardware, where they are false. So echo cancellation and gain control are
//! reported as automatic, not as user-toggled features.

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
    /// Extra microphone gain 1–200 %; needs the R07b PCM path.
    pub input_gain: Support,
    /// Noise gate threshold; needs the R07b PCM path.
    pub noise_gate: Support,
    pub echo_cancellation: Support,
    pub auto_gain: Support,
    pub noise_suppression: Support,
    /// Room output gain: 0 closes the tracks, 1–200 is not applied yet.
    pub output_gain: Support,
    /// Opus maximum bitrate selection.
    pub bitrate: Support,
    /// WinMM microphone level preview.
    pub microphone_preview: Support,
    /// WinMM interface/test tone.
    pub output_test: Support,
}

/// The capabilities of this build. Windows gets the WinMM preview/tone port;
/// every other target reports them unavailable.
pub fn audio_capabilities() -> AudioCapabilities {
    AudioCapabilities {
        input_gain: Support::Unavailable,
        noise_gate: Support::Unavailable,
        echo_cancellation: Support::Automatic,
        auto_gain: Support::Automatic,
        noise_suppression: Support::Automatic,
        output_gain: Support::Limited,
        bitrate: Support::Applied,
        microphone_preview: preview_support(),
        output_test: tone_support(),
    }
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
    fn the_map_tells_the_truth_about_every_switch() {
        let capabilities = audio_capabilities();
        assert_eq!(capabilities.bitrate, Support::Applied);
        assert_eq!(capabilities.output_gain, Support::Limited);
        assert_eq!(capabilities.echo_cancellation, Support::Automatic);
        assert_eq!(capabilities.auto_gain, Support::Automatic);
        assert_eq!(capabilities.noise_suppression, Support::Automatic);
        assert_ne!(capabilities.input_gain, Support::Applied);
        assert_ne!(capabilities.noise_gate, Support::Applied);
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
