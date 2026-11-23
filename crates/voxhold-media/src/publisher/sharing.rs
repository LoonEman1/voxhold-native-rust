//! The UI-facing publication snapshot. Kept in its own module so the state
//! machine stays inside the repository file-size budget.

use super::{Codec, Phase, PublishRequest, Quality, QualityStatus, engine};
use crate::screen_capture::ScreenSource;
use crate::system_audio::AudioScope;

/// The configuration of the attempt the media layer really accepted and put on
/// air. It is filled from the publisher's own request when that attempt turns
/// `Live`, and a live bitrate change re-fills it from the request the sender
/// really received. A request the picker only sent never becomes confirmed: a
/// preflight, an in-flight restart and the rollback step itself leave this
/// configuration alone, and only the next attempt's `Live` transition replaces
/// it. A refused or cancelled quality change therefore cannot replace the live
/// settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Confirmed {
    /// The attempt that owns this configuration.
    pub attempt: u64,
    /// The accepted source; the restart dialog stays bound to its identity.
    pub source: ScreenSource,
    pub quality: Quality,
    pub codec: Codec,
    /// The encoder that was really on air: the CPU default or the confirmed
    /// adapter and its generation. The UI never guesses the backend from the
    /// request.
    pub encoder: crate::video_encoder::EncoderPlan,
    /// The accepted system-audio flag of the attempt.
    pub audio: bool,
    /// The typed capture scope carried by the request this configuration was
    /// built from. It is local to the engine; the wire contract has no scope
    /// field. The `Live` transition fills it, and a live bitrate change that
    /// the sender really applied re-fills it from that request; a preflight, an
    /// in-flight restart and the rollback step do not rewrite it.
    pub audio_scope: AudioScope,
}

impl Confirmed {
    pub(super) fn from_request(attempt: u64, request: &PublishRequest) -> Self {
        Self {
            attempt,
            source: request.source.clone(),
            quality: request.quality,
            codec: request.codec,
            encoder: request.encoder.clone(),
            audio: request.audio,
            audio_scope: request.audio_scope,
        }
    }

    /// The request a restart of this confirmed attempt would carry.
    pub fn request(&self) -> PublishRequest {
        PublishRequest {
            source: self.source.clone(),
            quality: self.quality,
            codec: self.codec,
            audio: self.audio,
            audio_scope: self.audio_scope,
            encoder: self.encoder.clone(),
        }
    }
}

/// The UI-facing snapshot. The panel never guesses a phase itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Sharing {
    pub phase: Phase,
    pub attempt: u64,
    pub live: bool,
    /// The request of the current attempt. It stays at the confirmed values
    /// while a restart or a live bitrate change is only being applied; only
    /// `confirmed` describes what is on air.
    pub quality: Option<Quality>,
    pub codec: Option<Codec>,
    /// The encoder of the current attempt (CPU or the confirmed hardware
    /// encoder). It follows the request, so the dialog can paint what Start
    /// would use; the on-air value stays in `confirmed`.
    pub encoder: Option<crate::video_encoder::EncoderPlan>,
    /// The accepted configuration of the live attempt. It stays at the last
    /// confirmed values during a preflight, a restart or a rollback; only a
    /// live bitrate change applied in the air or the next `Live` attempt
    /// replaces it.
    pub confirmed: Option<Confirmed>,
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
    /// The GPU encoder died after the publication started and the same codec
    /// can still be encoded by the pinned CPU build. The dialog shows one
    /// explicit message and the «Продолжить на процессоре» action; nothing is
    /// substituted automatically.
    pub cpu_fallback: bool,
    /// The open hardware session that matched the attempt. `Some` is the local
    /// provider confirmation; the dialog additionally needs delivered encoded
    /// frames before it names the stream «GPU».
    pub hardware: Option<crate::video_encoder::HardwareEncoderIdentity>,
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
            encoder: None,
            confirmed: None,
            has_audio: false,
            audio_scope: None,
            restarting: false,
            preflighting: false,
            quality_status: None,
            cpu_fallback: false,
            hardware: None,
            status: String::new(),
            error: None,
            stats: None,
        }
    }
}
