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
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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
#[derive(Clone, Debug)]
pub struct Signal {
    pub kind: &'static str,
    pub data: Value,
    pub request_id: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Action {
    Send(Signal),
    Media(Command),
    Reconnect,
    Closed(&'static str),
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
    owners: HashMap<String, String>,
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
            owners: HashMap::new(),
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
    fn effective(&self) -> Controls {
        let mut c = self.controls.clone();
        c.muted |= self.server_muted;
        c
    }
    pub fn update_controls(&mut self, controls: Controls, self_muted: bool) -> Vec<Action> {
        self.controls = controls;
        self.self_muted = self_muted;
        let mut a = vec![Action::Media(Command::State(self.effective()))];
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
            .map(|(track, _)| {
                Action::Media(Command::Volume {
                    track_id: track.clone(),
                    gain: if muted { 0.0 } else { 1.0 },
                })
            })
            .collect()
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
                if let Some(owner) = self.owners.get(&mid) {
                    self.tracks.insert(track_id.clone(), owner.clone());
                    return vec![Action::Media(Command::Volume {
                        track_id,
                        gain: if self.local_mutes.contains(owner) {
                            0.0
                        } else {
                            1.0
                        },
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
            Event::State {
                state: "failed" | "closed",
            } => vec![Action::Reconnect],
            Event::Error { message } => vec![Action::Closed(message)],
            _ => vec![],
        }
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
                    self.owners = track_owners(sdp);
                    self.tracks.clear();
                    vec![Action::Media(Command::Offer { sdp: sdp.into() })]
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
}
