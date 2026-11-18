//! Headless voice-session state and signaling helpers. The application owns the
//! socket and drives this controller on its network task.
use crate::{Candidate, Command, Controls, Event};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Participant {
    pub connection_id: String,
    pub user_id: i64,
    pub server_id: i64,
    pub channel_id: i64,
    #[serde(default)]
    pub self_mute: bool,
    #[serde(default)]
    pub self_deaf: bool,
    #[serde(default)]
    pub server_mute: bool,
    #[serde(default)]
    pub reconnecting: bool,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Publication {
    pub server_id: i64,
    pub channel_id: i64,
    pub publisher_user_id: i64,
    pub publisher_connection_id: String,
    pub mode: String,
    pub codec: String,
    pub has_audio: bool,
    pub viewer_count: i64,
    pub started_at_ms: i64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    pub kind: &'static str,
    pub data: Value,
    pub request_id: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Action {
    Send(Signal),
    Media(Command),
    Notice(&'static str),
    Reconnect,
    Closed(&'static str),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppliedAudio {
    /// `software` for the WinMM 48 kHz path or `platform` for ADM fallback.
    pub capture: &'static str,
    /// Effective noise mode. Platform fallback always reports `auto`.
    pub noise_mode: &'static str,
    pub echo_cancellation: bool,
    pub auto_gain_control: bool,
    /// The room is played by the software mixer, so the participant gain,
    /// limiter and post-mix meter are real.
    pub software_mixer: bool,
}

pub struct VoiceController {
    pub server_id: i64,
    pub channel_id: i64,
    pub resume_connection_id: Option<String>,
    pub participants: Vec<Participant>,
    pub local_mutes: HashSet<String>,
    pub speaking: HashMap<String, bool>,
    pub joined: bool,
    pub controls: Controls,
    pub self_muted: bool,
    pub server_muted: bool,
    /// Pre-gate local microphone level of the real capture track.
    pub local_level: f64,
    /// Once the software capture reports its pre-gate meter, the post-gate
    /// `MediaSource` level must not overwrite it with a different scale.
    input_level_seen: bool,
    /// Whole 10 ms blocks discarded by the bounded PCM queue or the source.
    pub input_dropped: u64,
    pub input_muted: bool,
    /// Last applied capture chain reported by the media engine.
    pub applied_audio: Option<AppliedAudio>,
    /// Last measured post-mix playout level and counters.
    pub playout: Option<crate::mixer::PlayoutStats>,
    owners: HashMap<String, String>,
    /// Last known mid for every receiver track. Renegotiation replaces the
    /// mid -> owner map; without this link an existing receiver whose
    /// `on_track` is not repeated would lose its owner and its gain.
    track_mids: HashMap<String, String>,
    tracks: HashMap<String, String>,
}
impl VoiceController {
    pub fn new(server_id: i64, channel_id: i64, controls: Controls, self_muted: bool) -> Self {
        Self {
            server_id,
            channel_id,
            resume_connection_id: None,
            participants: Vec::new(),
            local_mutes: HashSet::new(),
            speaking: HashMap::new(),
            joined: false,
            controls,
            self_muted,
            server_muted: false,
            local_level: 0.0,
            input_level_seen: false,
            input_dropped: 0,
            input_muted: true,
            applied_audio: None,
            playout: None,
            owners: HashMap::new(),
            track_mids: HashMap::new(),
            tracks: HashMap::new(),
        }
    }
    fn signal(kind: &'static str, data: Value) -> Action {
        Action::Send(Signal {
            kind,
            data,
            request_id: None,
        })
    }
    /// The media state this session should run with, including server mute.
    pub fn effective(&self) -> Controls {
        let mut c = self.controls.clone();
        c.muted |= self.server_muted;
        c
    }
    pub fn update_controls(&mut self, mut controls: Controls, self_muted: bool) -> Vec<Action> {
        let volume_changed = controls.output_gain != self.controls.output_gain
            || controls.user_volumes != self.controls.user_volumes;
        // The input device is owned by the explicit `SelectInput` request. A
        // generic mute/DSP update that was queued before the user picked a
        // different device must not revert that choice.
        controls.input = self.controls.input.clone();
        self.controls = controls;
        self.self_muted = self_muted;
        // Capture stays closed until the server confirmed `voice.joined`; the
        // panel may update mute/deafen/PTT while the join is still pending.
        let mut a = if self.joined {
            vec![Action::Media(Command::State(self.effective()))]
        } else {
            Vec::new()
        };
        if volume_changed && self.joined {
            for (track, owner) in &self.tracks {
                let (gain, muted) = self.source_for(owner);
                a.push(Action::Media(Command::Volume {
                    track_id: track.clone(),
                    gain,
                    muted,
                }));
            }
        }
        if self.joined {
            a.push(Self::signal(
                "voice.state_update",
                json!({"self_mute":self_muted,"self_deaf":self.controls.deafened}),
            ));
        }
        a
    }
    pub fn set_local_mute(&mut self, connection: &str, muted: bool) -> Vec<Action> {
        if muted {
            self.local_mutes.insert(connection.into());
        } else {
            self.local_mutes.remove(connection);
        }
        self.tracks
            .iter()
            .filter(|(_, owner)| owner.as_str() == connection)
            .map(|(track, owner)| {
                let (gain, muted) = self.source_for(owner);
                Action::Media(Command::Volume {
                    track_id: track.clone(),
                    gain,
                    muted,
                })
            })
            .collect()
    }

    /// Audible gain and the separate mute flag of one participant. The
    /// percentage is stored by the stable user id and resolved through the
    /// current roster entry for `owner`; a connection id that now belongs to
    /// somebody else never inherits the previous user's volume.
    fn source_for(&self, owner: &str) -> (f64, bool) {
        let muted = self.controls.deafened || self.local_mutes.contains(owner);
        (self.user_gain(owner), muted)
    }

    fn user_gain(&self, owner: &str) -> f64 {
        let user = self
            .participants
            .iter()
            .find(|participant| participant.connection_id == owner)
            .map(|participant| participant.user_id);
        let volume = user
            .and_then(|id| self.controls.user_volumes.get(&id).copied())
            .unwrap_or(100);
        (volume.clamp(0, 200) as f64) / 100.0
    }
    pub fn media_event(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Answer { sdp } if self.joined => {
                vec![Self::signal("voice.webrtc_answer", json!({"sdp":sdp}))]
            }
            Event::Ice(c) if self.joined => vec![Self::signal(
                "voice.ice_candidate",
                json!({"candidate":c.candidate,"sdp_mid":c.sdp_mid,"sdp_mline_index":c.sdp_mline_index,"username_fragment":c.username_fragment}),
            )],
            Event::Track {
                track_id,
                mid: Some(mid),
                ..
            } => {
                self.track_mids.insert(track_id.clone(), mid.clone());
                if let Some(owner) = self.owners.get(&mid) {
                    self.tracks.insert(track_id.clone(), owner.clone());
                    let (gain, muted) = self.source_for(owner);
                    return vec![Action::Media(Command::Volume {
                        track_id,
                        gain,
                        muted,
                    })];
                }
                vec![]
            }
            Event::Level {
                track_id: Some(id),
                level,
            } => {
                if let Some(owner) = self.tracks.get(&id) {
                    self.speaking.insert(owner.clone(), level >= 0.018);
                }
                vec![]
            }
            Event::Level {
                track_id: None,
                level,
            } => {
                // The post-gate `MediaSource` level is a fallback for the
                // platform path. The software path already reports its
                // pre-gate meter, and mixing the two scales makes the room
                // indicator jump to zero on every speech-to-silence edge.
                if !self.input_level_seen {
                    self.local_level = level;
                }
                vec![]
            }
            Event::InputLevel {
                level,
                muted,
                dropped,
            } => {
                // Pre-gate software meter: it must show a quiet background
                // while the gate transmits nothing.
                self.local_level = level;
                self.input_level_seen = true;
                self.input_muted = muted;
                self.input_dropped = dropped;
                vec![]
            }
            Event::Audio {
                capture,
                noise_mode,
                echo_cancellation,
                auto_gain_control,
                software_mixer,
            } => {
                if capture == "platform" {
                    // The platform path has no InputLevel; let MediaSource level
                    // drive the meter again instead of freezing the last
                    // software value.
                    self.input_level_seen = false;
                }
                self.applied_audio = Some(AppliedAudio {
                    capture,
                    noise_mode,
                    echo_cancellation,
                    auto_gain_control,
                    software_mixer,
                });
                vec![]
            }
            Event::Playout(stats) => {
                self.playout = Some(stats);
                vec![]
            }
            Event::State {
                state: "failed" | "closed",
            } => vec![Action::Reconnect],
            Event::DeviceError { message } => vec![Action::Notice(message)],
            Event::Error { message } => vec![Action::Closed(message)],
            _ => vec![],
        }
    }
    /// Connections that currently own an audible remote track. The room only
    /// offers "mute locally" for these participants.
    pub fn connections_with_audio(&self) -> HashSet<String> {
        self.tracks.values().cloned().collect()
    }
    pub fn on_signal(&mut self, kind: &str, data: &Value) -> Vec<Action> {
        match kind {
            "ready" => {
                let mut d = json!({"server_id":self.server_id,"channel_id":self.channel_id,"self_mute":self.self_muted,"self_deaf":self.controls.deafened});
                if let Some(id) = &self.resume_connection_id {
                    d["resume_connection_id"] = json!(id);
                }
                vec![Self::signal("voice.join", d)]
            }
            "voice.joined" => {
                let Ok(p) = serde_json::from_value::<Participant>(data["participant"].clone())
                else {
                    return vec![Action::Closed("Некорректный ответ сервера")];
                };
                if p.channel_id != self.channel_id {
                    return vec![Action::Closed("Некорректный ответ сервера")];
                }
                if data["e2ee_required"].as_bool().unwrap_or(false) {
                    return vec![Action::Closed(
                        "Для голосового канала требуется E2EE, которое пока недоступно",
                    )];
                }
                self.joined = true;
                self.input_level_seen = false;
                self.server_muted = p.server_mute;
                self.resume_connection_id = Some(p.connection_id);
                self.participants =
                    serde_json::from_value(data["participants"].clone()).unwrap_or_default();
                vec![
                    Action::Media(Command::State(self.effective())),
                    Self::signal(
                        "voice.state_update",
                        json!({"self_mute":self.self_muted,"self_deaf":self.controls.deafened}),
                    ),
                ]
            }
            "voice.webrtc_offer" if self.joined => {
                if let Some(sdp) = data["sdp"].as_str().filter(|s| !s.is_empty()) {
                    let owners = track_owners(sdp);
                    let known: Vec<_> = self
                        .track_mids
                        .iter()
                        .map(|(track, mid)| (track.clone(), mid.clone()))
                        .collect();
                    for (track, mid) in known {
                        if let Some(owner) = owners.get(&mid) {
                            self.tracks.insert(track, owner.clone());
                        } else {
                            self.tracks.remove(&track);
                        }
                    }
                    self.owners = owners;
                    let mut actions = vec![Action::Media(Command::Offer { sdp: sdp.into() })];
                    // Apply the configured gain again to receivers that
                    // survived the renegotiation without another on_track.
                    for (track, owner) in &self.tracks {
                        let (gain, muted) = self.source_for(owner);
                        actions.push(Action::Media(Command::Volume {
                            track_id: track.clone(),
                            gain,
                            muted,
                        }));
                    }
                    actions
                } else {
                    vec![Action::Closed("Некорректное предложение WebRTC")]
                }
            }
            "voice.webrtc_offer" => vec![Action::Closed("Предложение получено до входа в канал")],
            "voice.ice_candidate" if self.joined => vec![Action::Media(Command::Ice(Candidate {
                candidate: data["candidate"].as_str().unwrap_or_default().into(),
                sdp_mid: data["sdp_mid"].as_str().map(str::to_owned),
                sdp_mline_index: data["sdp_mline_index"]
                    .as_u64()
                    .and_then(|v| u16::try_from(v).ok()),
                username_fragment: data["username_fragment"].as_str().map(str::to_owned),
            }))],
            "voice.participant_joined" | "voice.state_updated" | "voice.participant_left" => {
                if let Ok(p) = serde_json::from_value::<Participant>(data.clone()) {
                    if p.channel_id == self.channel_id {
                        let mut out = Vec::new();
                        if kind == "voice.state_updated"
                            && self.resume_connection_id.as_deref() == Some(&p.connection_id)
                            && self.server_muted != p.server_mute
                        {
                            self.server_muted = p.server_mute;
                            out.push(Action::Media(Command::State(self.effective())));
                        }
                        self.participants
                            .retain(|old| old.connection_id != p.connection_id);
                        if kind == "voice.participant_left" {
                            self.local_mutes.remove(&p.connection_id);
                            self.speaking.remove(&p.connection_id);
                        } else {
                            self.participants.push(p);
                        }
                        return out;
                    }
                }
                vec![]
            }
            "voice.webrtc_closed" => {
                self.joined = false;
                match data["reason"].as_str().unwrap_or_default() {
                    "voice session moved to another connection"
                    | "voice channel encryption changed"
                    | "voice e2ee device was revoked"
                    | "voice permission was revoked"
                    | "channel access was revoked" => {
                        vec![Action::Closed("Голосовое соединение завершено сервером")]
                    }
                    _ => vec![Action::Reconnect],
                }
            }
            "voice.left" => {
                self.joined = false;
                vec![Action::Closed("Голосовое соединение завершено сервером")]
            }
            "error" => vec![Action::Closed("Сервер отклонил голосовое соединение")],
            _ => vec![],
        }
    }
}
/// Only unambiguous, active SFU audio labels are mapped to participants.
pub fn track_owners(sdp: &str) -> HashMap<String, String> {
    let mut owners = HashMap::new();
    let normalized = sdp.replace("\r\n", "\n");
    for section in normalized.split("\nm=") {
        let mut lines = section.trim_start_matches("m=").lines();
        let Some(first) = lines.next() else { continue };
        let fields: Vec<_> = first.split_whitespace().collect();
        if fields.len() < 2 || fields[0] != "audio" || fields[1] == "0" {
            continue;
        }
        let (mut mid, mut label, mut ambiguous, mut inactive) = ("", "", false, false);
        for line in lines.map(str::trim) {
            if line == "a=inactive" || line == "a=recvonly" {
                inactive = true;
            }
            if let Some(v) = line.strip_prefix("a=mid:") {
                mid = v;
            }
            if line.starts_with("a=msid:")
                || (line.starts_with("a=ssrc:") && line.contains(" msid:"))
            {
                let parts: Vec<_> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Some(v) = parts.last().unwrap().strip_prefix("audio-") {
                        if !label.is_empty() && label != v {
                            ambiguous = true;
                        }
                        label = v;
                    }
                }
            }
        }
        if !inactive && !ambiguous && !mid.is_empty() && !label.is_empty() {
            owners.insert(mid.into(), label.into());
        }
    }
    owners
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ambiguous_and_inactive_labels() {
        let s = "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=msid:x audio-a\r\na=msid:x audio-b\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:1\r\na=recvonly\r\na=msid:x audio-c\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:2\r\na=msid:x audio-d\r\n";
        let owners = track_owners(s);
        assert_eq!(owners.len(), 1);
        assert_eq!(owners["2"], "d");
    }

    #[test]
    fn controls_before_a_confirmed_join_never_reach_the_microphone() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        let actions = controller.update_controls(
            Controls {
                muted: false,
                deafened: false,
                ..Controls::default()
            },
            false,
        );
        assert!(
            actions.is_empty(),
            "the panel may toggle controls while connecting; the engine stays closed"
        );
    }

    #[test]
    fn a_server_mute_is_folded_into_the_media_state() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.joined = true;
        controller.server_muted = true;
        let state = controller.effective();
        assert!(state.muted);
        controller.server_muted = false;
        assert!(!controller.effective().muted);
    }

    #[test]
    fn a_renegotiation_keeps_receiver_owners_and_reapplies_gain() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.joined = true;
        controller.participants = vec![Participant {
            connection_id: "alice".into(),
            user_id: 7,
            ..Participant::default()
        }];
        controller.owners.insert("0".into(), "alice".into());
        let track = controller.media_event(Event::Track {
            track_id: "track-alice".into(),
            stream_id: None,
            mid: Some("0".into()),
        });
        assert!(matches!(
            track.first(),
            Some(Action::Media(Command::Volume { gain, .. })) if (*gain - 1.0).abs() < f64::EPSILON
        ));
        let mut volumes = std::collections::BTreeMap::new();
        volumes.insert(7, 50);
        controller.update_controls(
            Controls {
                user_volumes: volumes,
                ..Controls::default()
            },
            false,
        );
        let actions = controller.on_signal(
            "voice.webrtc_offer",
            &json!({
                "sdp": "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=msid:x audio-alice\r\n"
            }),
        );
        assert!(
            actions.iter().any(|action| matches!(
                action,
                Action::Media(Command::Volume { track_id, gain, .. })
                    if track_id == "track-alice" && (*gain - 0.5).abs() < f64::EPSILON
            )),
            "the surviving receiver keeps its owner and numeric gain: {actions:?}"
        );
        assert!(matches!(
            actions.first(),
            Some(Action::Media(Command::Offer { .. }))
        ));
    }

    #[test]
    fn input_level_reports_the_pre_gate_meter_and_dropped_blocks() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.media_event(Event::InputLevel {
            level: 0.42,
            muted: false,
            dropped: 7,
        });
        assert!((controller.local_level - 0.42).abs() < f64::EPSILON);
        assert!(!controller.input_muted);
        assert_eq!(controller.input_dropped, 7);
    }

    #[test]
    fn input_level_is_not_overwritten_by_the_post_gate_source_level() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.media_event(Event::InputLevel {
            level: 0.2,
            muted: false,
            dropped: 0,
        });
        controller.media_event(Event::Level {
            track_id: None,
            level: 0.0,
        });
        assert!(
            (controller.local_level - 0.2).abs() < f64::EPSILON,
            "the pre-gate meter must keep its own scale"
        );
        // Without a software meter (platform fallback) the source level is
        // still a useful fallback.
        let mut platform = VoiceController::new(1, 2, Controls::default(), false);
        platform.media_event(Event::Level {
            track_id: None,
            level: 0.4,
        });
        assert!((platform.local_level - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn platform_fallback_restores_the_source_level_meter() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.media_event(Event::InputLevel {
            level: 0.2,
            muted: false,
            dropped: 0,
        });
        controller.media_event(Event::Audio {
            capture: "platform",
            noise_mode: "auto",
            echo_cancellation: true,
            auto_gain_control: true,
            software_mixer: true,
        });
        controller.media_event(Event::Level {
            track_id: None,
            level: 0.4,
        });
        assert!((controller.local_level - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn applied_audio_carries_the_actual_fallback_mode() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.media_event(Event::Audio {
            capture: "platform",
            noise_mode: "auto",
            echo_cancellation: true,
            auto_gain_control: true,
            software_mixer: false,
        });
        let applied = controller.applied_audio.as_ref().unwrap();
        assert_eq!(applied.capture, "platform");
        assert_eq!(applied.noise_mode, "auto");
        assert!(applied.echo_cancellation);
        assert!(
            !applied.software_mixer,
            "the mixer flag is the applied path, not a guess"
        );
    }

    #[test]
    fn controls_to_audio_params_keep_wire_ranges() {
        let controls = Controls {
            noise_mode: crate::dsp::NoiseMode::parse("threshold"),
            noise_gate_percent: 250,
            input_gain: 9.0,
            echo_cancellation: false,
            auto_gain_control: false,
            muted: false,
            deafened: true,
            ..Controls::default()
        };
        let params = controls.audio_params();
        assert_eq!(params.mode, crate::dsp::NoiseMode::Threshold);
        assert_eq!(params.gate_percent, 100);
        assert!((params.input_gain - 2.0).abs() < f32::EPSILON);
        assert!(!params.echo_cancellation);
        assert!(!params.auto_gain_control);
        assert!(params.muted, "deafen opens no microphone");
    }

    #[test]
    fn state_action_carries_every_dsp_field() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.joined = true;
        let controls = Controls {
            noise_mode: crate::dsp::NoiseMode::Threshold,
            noise_gate_percent: 42,
            input_gain: 1.25,
            echo_cancellation: false,
            auto_gain_control: true,
            ..Controls::default()
        };
        let actions = controller.update_controls(controls, false);
        let Some(Action::Media(Command::State(state))) = actions.first() else {
            panic!("expected a State media command: {actions:?}");
        };
        assert_eq!(state.noise_mode, crate::dsp::NoiseMode::Threshold);
        assert_eq!(state.noise_gate_percent, 42);
        assert!((state.input_gain - 1.25).abs() < f32::EPSILON);
        assert!(!state.echo_cancellation);
        assert!(state.auto_gain_control);
    }

    #[test]
    fn a_control_update_keeps_the_selected_input_for_the_next_state() {
        let mut controller = VoiceController::new(
            1,
            2,
            Controls {
                input: Some("mic".into()),
                ..Controls::default()
            },
            false,
        );
        controller.joined = true;
        // A stale mute/DSP snapshot still carries the previous device.
        let actions = controller.update_controls(
            Controls {
                muted: true,
                input: Some("stale".into()),
                ..Controls::default()
            },
            false,
        );
        assert!(matches!(
            actions.first(),
            Some(Action::Media(Command::State(state)))
                if state.input.as_deref() == Some("mic")
        ));
        assert_eq!(controller.controls.input.as_deref(), Some("mic"));
    }

    #[test]
    fn output_gain_scales_a_participant_and_a_local_mute_wins() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.joined = true;
        controller.participants = vec![Participant {
            connection_id: "alice".into(),
            user_id: 7,
            ..Participant::default()
        }];
        let mut volumes = std::collections::BTreeMap::new();
        volumes.insert(7, 50);
        controller.update_controls(
            Controls {
                user_volumes: volumes,
                ..Controls::default()
            },
            false,
        );
        controller.owners.insert("0".into(), "alice".into());
        let actions = controller.media_event(Event::Track {
            track_id: "track".into(),
            stream_id: None,
            mid: Some("0".into()),
        });
        assert!(matches!(
            actions.first(),
            Some(Action::Media(Command::Volume { gain, muted, .. }))
                if (*gain - 0.5).abs() < f64::EPSILON && !muted
        ));
        // A local mute is a separate flag: the stored 50 % stays in the gain.
        let muted = controller.set_local_mute("alice", true);
        assert!(matches!(
            muted.first(),
            Some(Action::Media(Command::Volume { gain, muted, .. }))
                if (*gain - 0.5).abs() < f64::EPSILON && *muted
        ));
        let restored = controller.set_local_mute("alice", false);
        assert!(matches!(
            restored.first(),
            Some(Action::Media(Command::Volume { gain, muted, .. }))
                if (*gain - 0.5).abs() < f64::EPSILON && !muted
        ));
    }

    /// The per-user slider stores 0–200 % by user id; 150 % must reach that
    /// user's current track as 1.5 in the delivered command, not only in the
    /// snapshot.
    #[test]
    fn a_150_percent_user_volume_reaches_the_participant_track() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.joined = true;
        controller.participants = vec![
            Participant {
                connection_id: "alice".into(),
                user_id: 7,
                ..Participant::default()
            },
            Participant {
                connection_id: "bob".into(),
                user_id: 8,
                ..Participant::default()
            },
        ];
        controller.owners.insert("0".into(), "alice".into());
        controller.owners.insert("1".into(), "bob".into());
        for (mid, track) in [("0", "track-alice"), ("1", "track-bob")] {
            controller.media_event(Event::Track {
                track_id: track.into(),
                stream_id: None,
                mid: Some(mid.into()),
            });
        }
        let mut volumes = std::collections::BTreeMap::new();
        volumes.insert(7, 150);
        volumes.insert(8, 80);
        let actions = controller.update_controls(
            Controls {
                user_volumes: volumes,
                ..Controls::default()
            },
            false,
        );
        let mut volumes: Vec<_> = actions
            .iter()
            .filter_map(|action| match action {
                Action::Media(Command::Volume { track_id, gain, .. }) => {
                    Some((track_id.as_str(), *gain))
                }
                _ => None,
            })
            .collect();
        volumes.sort_by(|a, b| a.0.cmp(b.0));
        assert_eq!(
            volumes,
            vec![("track-alice", 1.5), ("track-bob", 0.8)],
            "each user's percentage reaches that user's track: {actions:?}"
        );
    }

    /// A reconnect can give the old connection id to another participant. The
    /// volume is keyed by user id and validated against the current owner, so
    /// it must not move to the new holder of that connection.
    #[test]
    fn a_user_volume_never_moves_to_another_participant_after_reconnect() {
        let mut controller = VoiceController::new(1, 2, Controls::default(), false);
        controller.joined = true;
        controller.participants = vec![
            Participant {
                connection_id: "conn-a".into(),
                user_id: 8,
                ..Participant::default()
            },
            Participant {
                connection_id: "conn-b".into(),
                user_id: 7,
                ..Participant::default()
            },
        ];
        let mut volumes = std::collections::BTreeMap::new();
        volumes.insert(7, 200);
        controller.controls.user_volumes = volumes;
        // Alice was on conn-a; after a reconnect Bob holds conn-a and Alice
        // has conn-b. The roster is the authority for who holds a connection.
        controller.owners.insert("0".into(), "conn-a".into());
        controller.owners.insert("1".into(), "conn-b".into());
        let bob = controller.media_event(Event::Track {
            track_id: "track-bob".into(),
            stream_id: None,
            mid: Some("0".into()),
        });
        assert!(
            matches!(
                bob.first(),
                Some(Action::Media(Command::Volume { gain, .. }))
                    if (*gain - 1.0).abs() < f64::EPSILON
            ),
            "Bob must not inherit Alice's 200 % through the reused connection id: {bob:?}"
        );
        let alice = controller.media_event(Event::Track {
            track_id: "track-alice".into(),
            stream_id: None,
            mid: Some("1".into()),
        });
        assert!(
            matches!(
                alice.first(),
                Some(Action::Media(Command::Volume { gain, .. }))
                    if (*gain - 2.0).abs() < f64::EPSILON
            ),
            "Alice's own track keeps her 200 %: {alice:?}"
        );
        // A stale owner that is no longer in the roster is unity, not a
        // remembered override.
        controller.owners.insert("2".into(), "gone".into());
        let stale = controller.media_event(Event::Track {
            track_id: "track-gone".into(),
            stream_id: None,
            mid: Some("2".into()),
        });
        assert!(matches!(
            stale.first(),
            Some(Action::Media(Command::Volume { gain, .. }))
                if (*gain - 1.0).abs() < f64::EPSILON
        ));
    }

    /// One settings change is one `Command::State` carrying the whole snapshot;
    /// a second command per field would make the engine apply them separately.
    #[test]
    fn one_settings_change_is_exactly_one_state_command() {
        // The explicit input selection owns `controls.input` (a stale snapshot
        // cannot revert it), so the controller starts with the selected device.
        let mut controller = VoiceController::new(
            1,
            2,
            Controls {
                input: Some("endpoint-mic".into()),
                ..Controls::default()
            },
            false,
        );
        controller.joined = true;
        let actions = controller.update_controls(
            Controls {
                input: Some("endpoint-mic".into()),
                output: Some("endpoint-spk".into()),
                bitrate_kbps: Some(96),
                input_gain: 1.4,
                output_gain: Some(1.5),
                noise_mode: crate::dsp::NoiseMode::Threshold,
                noise_gate_percent: 32,
                echo_cancellation: false,
                auto_gain_control: false,
                ..Controls::default()
            },
            false,
        );
        let states: Vec<_> = actions
            .iter()
            .filter(|action| matches!(action, Action::Media(Command::State(_))))
            .collect();
        assert_eq!(states.len(), 1, "exactly one State snapshot: {actions:?}");
        let Action::Media(Command::State(state)) = states[0] else {
            unreachable!()
        };
        assert_eq!(state.input.as_deref(), Some("endpoint-mic"));
        assert_eq!(state.output.as_deref(), Some("endpoint-spk"));
        assert_eq!(state.bitrate_kbps, Some(96));
        assert!((state.input_gain - 1.4).abs() < f32::EPSILON);
        assert_eq!(state.output_gain, Some(1.5));
        assert_eq!(state.noise_mode, crate::dsp::NoiseMode::Threshold);
        assert_eq!(state.noise_gate_percent, 32);
        assert!(!state.echo_cancellation);
        assert!(!state.auto_gain_control);
        // No track exists yet, so nothing else goes to the media layer; the
        // `voice.state_update` signal is not a parameter write.
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, Action::Media(_)))
                .count(),
            1,
            "no extra media command besides the one State: {actions:?}"
        );
    }
}
