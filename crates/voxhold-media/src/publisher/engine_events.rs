//! Engine events that need a state-machine decision.
//!
//! The module lives in its own file so the publisher state machine stays
//! inside the repository file-size budget; the event names and their tags are
//! re-exported from `publisher`.

use crate::{Candidate, publisher::QualityOp, video_encoder::HardwareEncoderIdentity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    Connected,
    FirstFrame,
}

/// What happened to one live-bitrate operation that reached the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BitrateResult {
    /// The senders and the peer accepted the limit and their readback matches
    /// it. Only now is the new configuration confirmed.
    Applied,
    /// At least one stage refused the limit or reported another readback.
    Failed {
        message: String,
        /// Every changed stage returned to its previous value. `false` means a
        /// partial change whose message names the actual readback limits.
        restored: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Answer(String),
    Ice(Candidate),
    State(Connection),
    Stats(crate::publisher::engine::PublishStats),
    /// The result of one live change. It carries both tags that decide whether
    /// the caller may still accept it: the publication attempt and the quality
    /// operation.
    Bitrate {
        attempt: u64,
        op: QualityOp,
        result: BitrateResult,
    },
    /// The hardware encoder session of this attempt died after the publication
    /// started. `cpu_available` only says that «Продолжить на процессоре» is a
    /// possible next step; the publisher never switches by itself.
    EncoderFailed {
        message: String,
        cpu_available: bool,
    },
    /// The hardware session is really open on this adapter. The publisher
    /// compares this identity with the attempt's plan before any GPU status is
    /// shown; the confirmation also needs delivered encoded frames.
    EncoderOpened(HardwareEncoderIdentity),
    Error(String),
    Closed,
}
