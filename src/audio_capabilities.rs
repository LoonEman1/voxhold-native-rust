//! Capability map for the voice settings tab. Without the media crate the
//! conservative answer is "nothing is applied", so the form still builds and
//! no switch promises an effect it cannot have.

#[cfg(feature = "voxhold-media")]
pub use voxhold_media::capabilities::*;

#[cfg(not(feature = "voxhold-media"))]
mod fallback {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Support {
        Applied,
        Automatic,
        Unavailable,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct AudioCapabilities {
        pub input_gain: Support,
        pub noise_gate: Support,
        pub echo_cancellation: Support,
        pub auto_gain: Support,
        pub noise_suppression: Support,
        pub output_gain: Support,
        pub bitrate: Support,
        pub microphone_preview: Support,
        pub output_test: Support,
    }

    pub fn audio_capabilities() -> AudioCapabilities {
        AudioCapabilities {
            input_gain: Support::Unavailable,
            noise_gate: Support::Unavailable,
            echo_cancellation: Support::Unavailable,
            auto_gain: Support::Unavailable,
            noise_suppression: Support::Unavailable,
            output_gain: Support::Unavailable,
            bitrate: Support::Unavailable,
            microphone_preview: Support::Unavailable,
            output_test: Support::Unavailable,
        }
    }

    pub fn capabilities_for(_capture: Option<&str>) -> AudioCapabilities {
        audio_capabilities()
    }
}

#[cfg(not(feature = "voxhold-media"))]
pub use fallback::*;
