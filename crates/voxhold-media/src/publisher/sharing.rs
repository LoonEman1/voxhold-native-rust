//! The UI-facing publication snapshot. Kept in its own module so the state
//! machine stays inside the repository file-size budget.

use super::{Codec, Phase, Quality, QualityStatus, engine};

/// The UI-facing snapshot. The panel never guesses a phase itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Sharing {
    pub phase: Phase,
    pub attempt: u64,
    pub live: bool,
    pub quality: Option<Quality>,
    pub codec: Option<Codec>,
    /// The engine really added the system-audio track (and `stream.start`
    /// reported it), not merely that the user ticked the box.
    pub has_audio: bool,
    /// Actual scope of the opened system-audio capture, as the engine reports
    /// it (the real process/area). `None` when the track is not published.
    pub audio_scope: Option<String>,
    /// A confirmed stop+start is in flight. The UI must warn that the stream
    /// will restart instead of claiming a live change.
    pub restarting: bool,
    /// A restart preflight is validating the new capture while the live stream
    /// keeps running. Nothing was stopped yet.
    pub preflighting: bool,
    /// The last quality-change operation and its result. Unlike
    /// `preflighting`, this survives coalescing, so a fast failure still names
    /// the request it belongs to.
    pub quality_status: Option<QualityStatus>,
    pub status: String,
    pub error: Option<String>,
    pub stats: Option<engine::PublishStats>,
}

impl Default for Sharing {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            attempt: 0,
            live: false,
            quality: None,
            codec: None,
            has_audio: false,
            audio_scope: None,
            restarting: false,
            preflighting: false,
            quality_status: None,
            status: String::new(),
            error: None,
            stats: None,
        }
    }
}
