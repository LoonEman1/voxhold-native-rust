//! Inbound stream signaling and video-frame access. Drive this alongside the
//! authenticated voice socket; only accept offers from the selected publisher.
use crate::{
    Candidate, Command, Event, IceServer, Media, VideoFrame,
    controller::{Publication, Signal},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub enum ViewerUpdate {
    Signal(Signal),
    Frame,
    Status(&'static str),
    Error(String),
}
pub struct StreamViewer {
    pub publication: Publication,
    pub frame: VideoFrame,
    media: Media,
    sequence: Arc<AtomicU64>,
    request_start: u64,
    watch_request: String,
    retries: u8,
    muted: bool,
}
impl StreamViewer {
    pub fn start(
        publication: Publication,
        servers: Vec<IceServer>,
        policy: String,
        output: Option<String>,
        muted: bool,
        sequence: Arc<AtomicU64>,
    ) -> Result<(Self, Signal), String> {
        if publication.mode != "p2p" && publication.mode != "server" {
            return Err("Неизвестный режим трансляции".into());
        }
        let media = Media::new().map_err(|e| format!("{e:?}"))?;
        media
            .command(Command::ViewStart {
                ice_servers: servers,
                ice_transport_policy: policy,
                output,
            })
            .map_err(|e| format!("{e:?}"))?;
        if muted {
            media
                .command(Command::ViewAudio { muted: true })
                .map_err(|e| format!("{e:?}"))?;
        }
        let mut viewer = Self {
            publication,
            frame: VideoFrame::default(),
            media,
            sequence,
            request_start: 0,
            watch_request: String::new(),
            retries: 0,
            muted,
        };
        let request=viewer.signal("stream.watch",json!({"server_id":viewer.publication.server_id,"channel_id":viewer.publication.channel_id,"publisher_connection_id":viewer.publication.publisher_connection_id,"supported_dynamic_ranges":["sdr"]}));
        viewer.watch_request = request.request_id.clone().unwrap_or_default();
        viewer.request_start = viewer.sequence.load(Ordering::Relaxed);
        Ok((viewer, request))
    }
    fn signal(&mut self, kind: &'static str, data: Value) -> Signal {
        let n = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        Signal {
            kind,
            data,
            request_id: Some(format!("native-stream-{n}")),
        }
    }
    pub fn set_muted(&mut self, muted: bool) -> Result<(), String> {
        self.media
            .command(Command::ViewAudio { muted })
            .map_err(|e| format!("{e:?}"))?;
        self.muted = muted;
        Ok(())
    }
    pub fn muted(&self) -> bool {
        self.muted
    }
    pub fn leave(&mut self) -> Signal {
        let _ = self.media.command(Command::Stop);
        self.signal("stream.leave", json!({}))
    }
    pub fn on_signal(&mut self, kind: &str, request_id: &str, data: &Value) -> Result<(), String> {
        match kind {
            "stream.watching" if request_id == self.watch_request => {
                let publication: Publication = serde_json::from_value(data["stream"].clone())
                    .map_err(|_| "Некорректный ответ сервера")?;
                if publication.publisher_connection_id != self.publication.publisher_connection_id {
                    return Err("Сервер выбрал другую трансляцию".into());
                }
                self.publication = publication;
            }
            "stream.webrtc_offer" | "stream.p2p_offer" => {
                if !self.accept_sender(kind, data) {
                    return Ok(());
                }
                if let Some(sdp) = data["sdp"].as_str().filter(|s| !s.is_empty()) {
                    self.media
                        .command(Command::Offer { sdp: sdp.into() })
                        .map_err(|e| format!("{e:?}"))?;
                }
            }
            "stream.ice_candidate" | "stream.p2p_ice_candidate" => {
                if !self.accept_sender(kind, data) {
                    return Ok(());
                }
                let c = Candidate {
                    candidate: data["candidate"].as_str().unwrap_or_default().into(),
                    sdp_mid: data["sdp_mid"].as_str().map(str::to_owned),
                    sdp_mline_index: data["sdp_mline_index"]
                        .as_u64()
                        .and_then(|n| u16::try_from(n).ok()),
                    username_fragment: data["username_fragment"].as_str().map(str::to_owned),
                };
                self.media
                    .command(Command::Ice(c))
                    .map_err(|e| format!("{e:?}"))?;
            }
            "stream.stopped"
                if data["channel_id"].as_i64() == Some(self.publication.channel_id)
                    && (data["publisher_connection_id"]
                        .as_str()
                        .unwrap_or_default()
                        .is_empty()
                        || data["publisher_connection_id"].as_str()
                            == Some(self.publication.publisher_connection_id.as_str())) =>
            {
                return Err("Трансляция завершена".into());
            }

            "error"
                if request_id
                    .strip_prefix("native-stream-")
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n >= self.request_start) =>
            {
                return Err("Сервер отклонил просмотр трансляции".into());
            }
            _ => {}
        }
        Ok(())
    }
    fn accept_sender(&self, kind: &str, data: &Value) -> bool {
        if kind.starts_with("stream.p2p_") {
            self.publication.mode == "p2p"
                && data["from_connection_id"].as_str()
                    == Some(self.publication.publisher_connection_id.as_str())
        } else {
            self.publication.mode == "server"
        }
    }
    pub fn poll(&mut self, width: u32, height: u32) -> Option<ViewerUpdate> {
        let event = self.media.poll(Duration::ZERO)?;
        match event {
            Event::Frame => {
                if self.media.read_video(&mut self.frame, width, height) {
                    Some(ViewerUpdate::Frame)
                } else {
                    None
                }
            }
            Event::Answer { sdp } => {
                let p2p = self.publication.mode == "p2p";
                let mut data = json!({"sdp":sdp});
                if p2p {
                    data["target_connection_id"] = json!(self.publication.publisher_connection_id);
                }
                Some(ViewerUpdate::Signal(self.signal(
                    if p2p {
                        "stream.p2p_answer"
                    } else {
                        "stream.webrtc_answer"
                    },
                    data,
                )))
            }
            Event::Ice(c) => {
                let p2p = self.publication.mode == "p2p";
                let mut data = json!({"candidate":c.candidate,"sdp_mid":c.sdp_mid,"sdp_mline_index":c.sdp_mline_index,"username_fragment":c.username_fragment});
                if p2p {
                    data["target_connection_id"] = json!(self.publication.publisher_connection_id);
                }
                Some(ViewerUpdate::Signal(self.signal(
                    if p2p {
                        "stream.p2p_ice_candidate"
                    } else {
                        "stream.ice_candidate"
                    },
                    data,
                )))
            }
            Event::State { state: "connected" } => Some(ViewerUpdate::Status("Подключено")),
            Event::State {
                state: "failed" | "disconnected",
            } if self.retries < 2 => {
                self.retries += 1;
                let p2p = self.publication.mode == "p2p";
                let data = if p2p {
                    json!({"target_connection_id":self.publication.publisher_connection_id})
                } else {
                    json!({"server_id":self.publication.server_id,"channel_id":self.publication.channel_id,"action":"ice_restart"})
                };
                Some(ViewerUpdate::Signal(self.signal(
                    if p2p {
                        "stream.p2p_restart"
                    } else {
                        "stream.recovery_request"
                    },
                    data,
                )))
            }
            Event::Error { message } => Some(ViewerUpdate::Error(message.into())),
            _ => None,
        }
    }
}
