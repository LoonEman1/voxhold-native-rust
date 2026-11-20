//! Local interface cues for the voice room: the pure tone specifications and
//! the transition policy that decides when they play.
//!
//! Port of `Voxhold-frontend/src/services/uiSounds.ts` at `ed7cb00`: rising
//! pairs mean "on/in" and falling pairs mean "off/out". All six voice cues are
//! sine tones with a short attack and an exponential release; the renderer in
//! `voxhold-media::preview` applies the master gain and the saved volume.
//!
//! This module has no audio device and no UI dependency, so the policy is
//! tested without a window, a player or a server.

/// A cue the local player hears for their own voice connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceCue {
    SelfJoin,
    SelfLeave,
    Mute,
    Unmute,
    Deafen,
    Undeafen,
}

/// One tone in the frontend's Web Audio terms. `at` and `duration` are seconds
/// from the start of the cue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneSpec {
    pub frequency: f64,
    pub at: f64,
    pub duration: f64,
}

/// The gain and tones of one cue. `gain` is the frontend master gain; volume is
/// applied on top by the renderer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CueSpec {
    pub gain: f64,
    pub tones: &'static [ToneSpec],
}

const SELF_JOIN_TONES: [ToneSpec; 2] = [
    ToneSpec {
        frequency: 587.33,
        at: 0.0,
        duration: 0.12,
    },
    ToneSpec {
        frequency: 880.0,
        at: 0.1,
        duration: 0.2,
    },
];
const SELF_LEAVE_TONES: [ToneSpec; 2] = [
    ToneSpec {
        frequency: 880.0,
        at: 0.0,
        duration: 0.12,
    },
    ToneSpec {
        frequency: 587.33,
        at: 0.1,
        duration: 0.22,
    },
];
const MUTE_TONES: [ToneSpec; 2] = [
    ToneSpec {
        frequency: 523.25,
        at: 0.0,
        duration: 0.06,
    },
    ToneSpec {
        frequency: 392.0,
        at: 0.05,
        duration: 0.1,
    },
];
const UNMUTE_TONES: [ToneSpec; 2] = [
    ToneSpec {
        frequency: 392.0,
        at: 0.0,
        duration: 0.06,
    },
    ToneSpec {
        frequency: 523.25,
        at: 0.05,
        duration: 0.1,
    },
];
const DEAFEN_TONES: [ToneSpec; 2] = [
    ToneSpec {
        frequency: 440.0,
        at: 0.0,
        duration: 0.07,
    },
    ToneSpec {
        frequency: 293.66,
        at: 0.06,
        duration: 0.15,
    },
];
const UNDEAFEN_TONES: [ToneSpec; 2] = [
    ToneSpec {
        frequency: 293.66,
        at: 0.0,
        duration: 0.07,
    },
    ToneSpec {
        frequency: 440.0,
        at: 0.06,
        duration: 0.15,
    },
];

impl VoiceCue {
    /// Exact frontend `UI_SOUND_SPECS` entry for this cue.
    pub fn spec(self) -> CueSpec {
        match self {
            Self::SelfJoin => CueSpec {
                gain: 0.45,
                tones: &SELF_JOIN_TONES,
            },
            Self::SelfLeave => CueSpec {
                gain: 0.45,
                tones: &SELF_LEAVE_TONES,
            },
            Self::Mute => CueSpec {
                gain: 0.35,
                tones: &MUTE_TONES,
            },
            Self::Unmute => CueSpec {
                gain: 0.35,
                tones: &UNMUTE_TONES,
            },
            Self::Deafen => CueSpec {
                gain: 0.35,
                tones: &DEAFEN_TONES,
            },
            Self::Undeafen => CueSpec {
                gain: 0.35,
                tones: &UNDEAFEN_TONES,
            },
        }
    }
}

/// Call phases as the policy sees them. Mirrors `voice_room::CallPhase`
/// without depending on the media-gated UI module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallStage {
    Idle,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

/// Phase-transition memory. `Reconnecting` arms the reconnect dedupe, and the
/// following `Connected` consumes it, so a recovery never plays a second join
/// cue even when the runtime emits another `Connecting` in between.
#[derive(Default)]
pub struct CuePolicy {
    recovering: bool,
}

impl CuePolicy {
    /// A fresh attempt starts with no pending recovery state.
    pub fn reset(&mut self) {
        self.recovering = false;
    }

    /// Cue for one applied phase transition. Repeated `Idle`/`Connected` states
    /// and roster/frame updates never produce a cue.
    pub fn transition(&mut self, from: CallStage, to: CallStage) -> Option<VoiceCue> {
        match to {
            CallStage::Reconnecting => {
                self.recovering = true;
                None
            }
            CallStage::Connected => {
                let recovering = self.recovering;
                self.recovering = false;
                (from != CallStage::Connected && !recovering).then_some(VoiceCue::SelfJoin)
            }
            CallStage::Idle | CallStage::Failed => {
                self.recovering = false;
                matches!(
                    from,
                    CallStage::Connecting | CallStage::Connected | CallStage::Reconnecting
                )
                .then_some(VoiceCue::SelfLeave)
            }
            CallStage::Connecting => None,
        }
    }
}

/// Cue of a local mute toggle, computed from the state before the toggle.
pub fn mute_toggle_cue(was_muted: bool) -> VoiceCue {
    if was_muted {
        VoiceCue::Unmute
    } else {
        VoiceCue::Mute
    }
}

/// Cue of a local deafen toggle, computed from the state before the toggle.
pub fn deafen_toggle_cue(was_deafened: bool) -> VoiceCue {
    if was_deafened {
        VoiceCue::Undeafen
    } else {
        VoiceCue::Deafen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(policy: &mut CuePolicy, stages: &[CallStage]) -> Vec<VoiceCue> {
        let mut cues = Vec::new();
        let mut from = CallStage::Idle;
        for stage in stages {
            if let Some(cue) = policy.transition(from, *stage) {
                cues.push(cue);
            }
            from = *stage;
        }
        cues
    }

    #[test]
    fn the_first_connected_transition_joins_once() {
        let mut policy = CuePolicy::default();
        assert_eq!(
            run(&mut policy, &[CallStage::Connecting, CallStage::Connected]),
            vec![VoiceCue::SelfJoin]
        );
        // Repeated Connected and roster/frame updates add nothing.
        assert_eq!(
            policy.transition(CallStage::Connected, CallStage::Connected),
            None
        );
        assert_eq!(
            policy.transition(CallStage::Connected, CallStage::Connected),
            None
        );
    }

    #[test]
    fn a_reconnect_never_replays_the_join_cue() {
        let mut policy = CuePolicy::default();
        let mut cues = run(&mut policy, &[CallStage::Connecting, CallStage::Connected]);
        // The runtime emits Reconnecting and then another Connecting before the
        // confirmed Connected; both orders must stay silent.
        cues.extend(run(
            &mut policy,
            &[CallStage::Reconnecting, CallStage::Connected],
        ));
        cues.extend(run(
            &mut policy,
            &[
                CallStage::Reconnecting,
                CallStage::Connecting,
                CallStage::Connected,
            ],
        ));
        assert_eq!(cues, vec![VoiceCue::SelfJoin]);
    }

    #[test]
    fn leaving_a_connected_call_plays_one_leave_cue() {
        let mut policy = CuePolicy::default();
        let cues = run(
            &mut policy,
            &[
                CallStage::Connecting,
                CallStage::Connected,
                CallStage::Idle,
                CallStage::Idle,
            ],
        );
        assert_eq!(cues, vec![VoiceCue::SelfJoin, VoiceCue::SelfLeave]);
    }

    #[test]
    fn a_failed_attempt_leaves_once_and_repeated_states_stay_silent() {
        let mut policy = CuePolicy::default();
        assert_eq!(
            run(
                &mut policy,
                &[CallStage::Connecting, CallStage::Failed, CallStage::Failed],
            ),
            vec![VoiceCue::SelfLeave]
        );
        let mut reconnecting = CuePolicy::default();
        assert_eq!(
            run(
                &mut reconnecting,
                &[
                    CallStage::Connecting,
                    CallStage::Connected,
                    CallStage::Reconnecting,
                    CallStage::Failed,
                ],
            ),
            vec![VoiceCue::SelfJoin, VoiceCue::SelfLeave]
        );
    }

    #[test]
    fn an_idle_start_and_a_reset_do_not_emit_a_leave_cue() {
        let mut policy = CuePolicy::default();
        assert_eq!(run(&mut policy, &[CallStage::Idle]), Vec::<VoiceCue>::new());
        policy.reset();
        assert_eq!(
            run(&mut policy, &[CallStage::Failed]),
            Vec::<VoiceCue>::new()
        );
    }

    #[test]
    fn toggles_announce_the_state_they_switch_to() {
        assert_eq!(mute_toggle_cue(false), VoiceCue::Mute);
        assert_eq!(mute_toggle_cue(true), VoiceCue::Unmute);
        assert_eq!(deafen_toggle_cue(false), VoiceCue::Deafen);
        assert_eq!(deafen_toggle_cue(true), VoiceCue::Undeafen);
    }

    #[test]
    fn specs_match_the_frontend_gains_and_tone_pairs() {
        let join = VoiceCue::SelfJoin.spec();
        assert_eq!(join.gain, 0.45);
        assert_eq!(join.tones.len(), 2);
        assert_eq!(join.tones[0].frequency, 587.33);
        assert_eq!(join.tones[1].frequency, 880.0);
        assert_eq!(join.tones[1].at, 0.1);

        let leave = VoiceCue::SelfLeave.spec();
        assert_eq!(leave.tones[0].frequency, 880.0);
        assert_eq!(leave.tones[1].frequency, 587.33);

        for cue in [
            VoiceCue::Mute,
            VoiceCue::Unmute,
            VoiceCue::Deafen,
            VoiceCue::Undeafen,
        ] {
            let spec = cue.spec();
            assert_eq!(spec.gain, 0.35);
            assert_eq!(spec.tones.len(), 2);
            assert!(spec.tones.iter().all(|tone| tone.duration > 0.0));
        }
    }
}
