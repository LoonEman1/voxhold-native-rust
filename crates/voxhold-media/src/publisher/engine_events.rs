//! Engine events that need a state-machine decision.
//!
//! The module lives in its own file so the publisher state machine stays
//! inside the repository file-size budget; the event names and their tags are
//! re-exported from `publisher`.

use crate::{Candidate, publisher::QualityOp};

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
        /// Every changed stage returned to its previous value.
        restored: bool,
        /// A changed stage could not be restored; the live limits are partially
        /// changed and the message must say so.
        partial: bool,
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
    Error(String),
    Closed,
}
