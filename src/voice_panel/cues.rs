//! Cue bookkeeping for the voice panel. The panel only records what happened;
//! the parent view drains the queue and owns the audio player, so this module
//! never touches a device and stays testable without one.

use super::*;
use crate::ui_sounds::{CallStage, CuePolicy, VoiceCue};

/// Phase policy plus the cues it recorded since the parent last drained them.
#[derive(Default)]
pub(super) struct CueQueue {
    policy: CuePolicy,
    pending: Vec<VoiceCue>,
}

impl VoicePanel {
    /// Start a fresh join attempt: recovery state from a previous session must
    /// not suppress its first join cue.
    pub(super) fn begin_attempt(&mut self) {
        self.cues.policy.reset();
        self.phase = CallPhase::Connecting;
    }

    /// Apply one real phase change and queue the cue it implies. Roster, frame,
    /// settings and participant updates never call this, so they stay silent.
    pub(super) fn transition_phase(&mut self, next: CallPhase) {
        let cue = self
            .cues
            .policy
            .transition(stage_of(self.phase), stage_of(next));
        self.phase = next;
        if let Some(cue) = cue {
            self.queue_cue(cue);
        }
    }

    /// A local mute/deafen toggle is exactly one cue; the caller decides
    /// whether the toggle is a user action that should be audible.
    pub(super) fn queue_cue(&mut self, cue: VoiceCue) {
        const LIMIT: usize = 8;
        if self.cues.pending.len() >= LIMIT {
            self.cues.pending.remove(0);
        }
        self.cues.pending.push(cue);
    }

    /// Cues recorded since the parent last drained them.
    pub(crate) fn take_cues(&mut self) -> Vec<VoiceCue> {
        std::mem::take(&mut self.cues.pending)
    }
}

fn stage_of(phase: CallPhase) -> CallStage {
    match phase {
        CallPhase::Idle => CallStage::Idle,
        CallPhase::Connecting => CallStage::Connecting,
        CallPhase::Connected => CallStage::Connected,
        CallPhase::Reconnecting => CallStage::Reconnecting,
        CallPhase::Failed => CallStage::Failed,
    }
}
