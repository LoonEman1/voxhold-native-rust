use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use libwebrtc::{
    audio_track::RtcAudioTrack,
    ice_candidate::IceCandidate,
    media_stream_track::MediaStreamTrack,
    peer_connection::{AnswerOptions, PeerConnection, PeerConnectionState},
    peer_connection_factory::{
        IceServer as RtcIceServer, IceTransportsType, PeerConnectionFactory, RtcConfiguration,
        native::PeerConnectionFactoryExt,
    },
    rtp_sender::RtpSender,
    session_description::{SdpType, SessionDescription},
    stats::RtcStats,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};

use crate::{Candidate, Command, Controls, Event, Events, IceServer, video::VideoStore};

const ICE_CAPACITY: usize = 64;
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(8);
const SPEAKING_THRESHOLD: f64 = 0.018;

#[derive(Default)]
pub struct CaptureGate(Mutex<(bool, Option<RtcAudioTrack>)>);

impl CaptureGate {
    pub fn set_enabled(&self, enabled: bool) {
        let mut state = self.0.lock().unwrap();
        if state.0 != enabled {
            state.0 = enabled;
            if let Some(track) = &state.1 {
                track.set_enabled(enabled);
            }
        }
    }

    fn attach(&self, track: RtcAudioTrack) {
        let mut state = self.0.lock().unwrap();
        track.set_enabled(state.0);
        state.1 = Some(track);
    }

    fn detach(&self) {
        let mut state = self.0.lock().unwrap();
        if let Some(track) = state.1.take() {
            track.set_enabled(false);
        }
    }
}

pub async fn run(
    mut commands: mpsc::Receiver<Command>,
    events: Arc<Events>,
    capture: Arc<CaptureGate>,
    video: Arc<VideoStore>,
) {
    let mut session: Option<Session> = None;
    let mut stats = time::interval(Duration::from_millis(250));
    stats.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => {
                let Some(command) = command else { break };
                let result = match command {
                    Command::Start { ice_servers, ice_transport_policy, controls } => {
                        session.take();
                        Session::new(ice_servers, &ice_transport_policy, controls, &events, capture.clone(), None).map(|new_session| {
                            session = Some(new_session);
                        })
                    }
                    Command::ViewStart { ice_servers, ice_transport_policy, output } => {
                        session.take();
                        let controls = Controls { output, ..Controls::default() };
                        Session::new(ice_servers, &ice_transport_policy, controls, &events, capture.clone(), Some(video.clone())).map(|new_session| {
                            session = Some(new_session);
                        })
                    }
                    Command::Stop => {
                        session.take();
                        Ok(())
                    }
                    command => match session.as_mut() {
                        Some(session) => session.apply(command, &events).await,
                        None => Err("Голосовое соединение ещё не открыто."),
                    }
                };
                if let Err(message) = result {
                    events.push(Event::Error { message });
                }
            }
            _ = stats.tick(), if session.as_ref().is_some_and(|session| session.sender.is_some()) => {
                if let Some(session) = &mut session {
                    session.sample_levels(&events).await;
                }
            }
        }
    }
}

struct PlatformAudio(PeerConnectionFactory);

impl PlatformAudio {
    fn new(recording: bool) -> Result<Self, &'static str> {
        let factory = PeerConnectionFactory::default();
        if !factory.acquire_platform_adm() {
            return Err("Не удалось открыть аудиосистему Windows.");
        }
        factory.set_adm_recording_enabled(recording);
        factory.set_adm_playout_enabled(true);
        Ok(Self(factory))
    }

    fn select(&self, id: &str, input: bool) -> bool {
        let factory = &self.0;
        let count = if input {
            factory.recording_devices()
        } else {
            factory.playout_devices()
        };
        let guid = |index| {
            if input {
                factory.recording_device_guid(index)
            } else {
                factory.playout_device_guid(index)
            }
        };
        let name = |index| {
            if input {
                factory.recording_device_name(index)
            } else {
                factory.playout_device_name(index)
            }
        };
        let index = if id.is_empty() {
            (count > 0).then_some(0)
        } else if let Some((_, old_name)) = id
            .split_once(':')
            .filter(|(index, _)| index.parse::<u32>().is_ok())
        {
            // The legacy WinMM UI stores an index and a potentially truncated display name.
            // Resolve a unique name in the Windows Core Audio list before switching.
            let mut matches =
                (0..count.max(0) as u16).filter(|&index| name(index).starts_with(old_name));
            let first = matches.next();
            first.filter(|_| matches.next().is_none())
        } else {
            (0..count.max(0) as u16).find(|&index| guid(index) == id)
        };
        let Some(index) = index else { return false };
        if input {
            let restart = factory.recording_is_initialized();
            if restart && !factory.stop_recording() {
                return false;
            }
            let changed = factory.set_recording_device(index);
            let resumed = !restart || (factory.init_recording() && factory.start_recording());
            changed && resumed
        } else {
            let restart = factory.playout_is_initialized();
            if restart && !factory.stop_playout() {
                return false;
            }
            let changed = factory.set_playout_device(index);
            let resumed = !restart || (factory.init_playout() && factory.start_playout());
            changed && resumed
        }
    }
}

impl Drop for PlatformAudio {
    fn drop(&mut self) {
        self.0.release_platform_adm();
    }
}

struct Session {
    peer: PeerConnection,
    sender: Option<RtpSender>,
    viewer: Option<(Arc<VideoStore>, JoinHandle<()>)>,
    capture: Arc<CaptureGate>,
    audio: PlatformAudio,
    controls: Controls,
    volumes: HashMap<String, f64>,
    speaking: HashMap<String, bool>,
    local_speaking: bool,
    remote_sdp: Option<RemoteSdp>,
    last_ufrags: Vec<String>,
    pending_ice: VecDeque<Candidate>,
}

impl Session {
    fn new(
        servers: Vec<IceServer>,
        transport_policy: &str,
        controls: Controls,
        events: &Arc<Events>,
        capture: Arc<CaptureGate>,
        viewer: Option<Arc<VideoStore>>,
    ) -> Result<Self, &'static str> {
        let audio = PlatformAudio::new(viewer.is_none())?;
        if let Some(id) = controls.input.as_deref().filter(|id| !id.is_empty()) {
            if !audio.select(id, true) {
                return Err("Выбранный микрофон недоступен.");
            }
        }
        if let Some(id) = controls.output.as_deref().filter(|id| !id.is_empty()) {
            if !audio.select(id, false) {
                return Err("Выбранное устройство воспроизведения недоступно.");
            }
        }
        let factory = &audio.0;
        let mut config = RtcConfiguration::default();
        config.ice_transport_type = match transport_policy {
            "" | "all" => IceTransportsType::All,
            "relay" => IceTransportsType::Relay,
            _ => return Err("Сервер прислал неизвестную политику ICE."),
        };
        config.ice_servers = servers
            .into_iter()
            .map(|server| RtcIceServer {
                urls: server.urls,
                username: server.username,
                password: server.credential,
            })
            .collect();
        let peer = factory
            .create_peer_connection(config)
            .map_err(|_| "Не удалось создать соединение WebRTC.")?;
        let state_events = events.clone();
        peer.on_connection_state_change(Some(Box::new(move |state| {
            let state = match state {
                PeerConnectionState::New | PeerConnectionState::Connecting => "connecting",
                PeerConnectionState::Connected => "connected",
                PeerConnectionState::Disconnected => "disconnected",
                PeerConnectionState::Failed => "failed",
                PeerConnectionState::Closed => "closed",
            };
            state_events.push(Event::State { state });
        })));
        let ice_events = events.clone();
        peer.on_ice_candidate(Some(Box::new(move |ice| {
            let candidate = ice.to_string();
            let username_fragment = candidate_ufrag(&candidate).map(str::to_owned);
            ice_events.push(Event::Ice(Candidate {
                candidate,
                sdp_mid: Some(ice.sdp_mid()).filter(|mid| !mid.is_empty()),
                sdp_mline_index: u16::try_from(ice.sdp_mline_index()).ok(),
                username_fragment,
            }));
        })));
        let (sender, viewer) = if let Some(video) = viewer {
            let (track_tx, track_rx) = watch::channel(None);
            peer.on_track(Some(Box::new(move |event| {
                if let MediaStreamTrack::Video(track) = event.track {
                    let _ = track_tx.send(Some(track));
                }
            })));
            let task = tokio::spawn(crate::video::consume(
                track_rx,
                video.clone(),
                events.clone(),
            ));
            (None, Some((video, task)))
        } else {
            let track_events = events.clone();
            peer.on_track(Some(Box::new(move |event| {
                if matches!(event.track, MediaStreamTrack::Audio(_)) {
                    track_events.push(Event::Track {
                        track_id: event.track.id(),
                        stream_id: event.streams.first().map(|stream| stream.id()),
                        mid: event.transceiver.mid(),
                    });
                }
            })));
            let mic = factory.create_device_audio_track("voxhold-microphone");
            let sender = peer
                .add_track(MediaStreamTrack::Audio(mic.clone()), &["voxhold-voice"])
                .map_err(|_| "Не удалось подключить микрофон к WebRTC.")?;
            capture.attach(mic);
            (Some(sender), None)
        };
        let mut session = Self {
            peer,
            sender,
            viewer,
            capture,
            audio,
            controls,
            volumes: HashMap::new(),
            speaking: HashMap::new(),
            local_speaking: false,
            remote_sdp: None,
            last_ufrags: Vec::new(),
            pending_ice: VecDeque::new(),
        };
        session.apply_audio_controls();
        Ok(session)
    }

    async fn apply(&mut self, command: Command, events: &Events) -> Result<(), &'static str> {
        match command {
            Command::Offer { sdp } => time::timeout(SIGNAL_TIMEOUT, self.answer(&sdp, events))
                .await
                .map_err(|_| "Согласование голосового соединения заняло слишком много времени.")?,
            Command::Ice(candidate) => {
                if candidate.candidate.is_empty() {
                    return Ok(());
                }
                if self
                    .remote_sdp
                    .as_ref()
                    .is_some_and(|remote| remote.accepts(&candidate))
                {
                    self.add_candidate(candidate).await;
                } else if !self.is_old_candidate(&candidate) {
                    if self.pending_ice.len() == ICE_CAPACITY {
                        self.pending_ice.pop_front();
                    }
                    self.pending_ice.push_back(candidate);
                }
                Ok(())
            }
            Command::State(controls) => self.update_controls(controls),
            Command::ViewAudio { muted } => {
                self.controls.deafened = muted;
                self.apply_audio_controls();
                Ok(())
            }
            Command::Volume { track_id, gain } => {
                if self.volumes.len() < 64 || self.volumes.contains_key(&track_id) {
                    self.volumes.insert(track_id, gain.clamp(0.0, 2.0));
                    self.apply_audio_controls();
                }
                Ok(())
            }
            Command::Start { .. } | Command::ViewStart { .. } | Command::Stop => unreachable!(),
        }
    }

    fn update_controls(&mut self, controls: Controls) -> Result<(), &'static str> {
        let deafen_changed = self.controls.deafened != controls.deafened;
        self.controls.muted = controls.muted;
        self.controls.deafened = controls.deafened;
        if let Some(bitrate) = controls
            .bitrate_kbps
            .filter(|value| Some(*value) != self.controls.bitrate_kbps)
        {
            self.controls.bitrate_kbps = Some(bitrate);
            self.update_bitrate();
        }
        if deafen_changed {
            self.apply_audio_controls();
        }
        if let Some(input) = controls
            .input
            .filter(|id| self.controls.input.as_ref() != Some(id))
        {
            if !self.audio.select(&input, true) {
                return Err("Не удалось переключить микрофон.");
            }
            self.controls.input = Some(input);
        }
        if let Some(output) = controls
            .output
            .filter(|id| self.controls.output.as_ref() != Some(id))
        {
            if !self.audio.select(&output, false) {
                return Err("Не удалось переключить устройство воспроизведения.");
            }
            self.controls.output = Some(output);
        }
        Ok(())
    }

    fn update_bitrate(&self) {
        let (Some(sender), Some(bitrate)) = (&self.sender, self.controls.bitrate_kbps) else {
            return;
        };
        let mut parameters = sender.parameters();
        if parameters.encodings.is_empty() {
            return;
        }
        for encoding in &mut parameters.encodings {
            encoding.max_bitrate = Some(u64::from(bitrate.clamp(6, 128)) * 1000);
        }
        let _ = sender.set_parameters(parameters);
    }

    fn apply_audio_controls(&mut self) {
        for receiver in self.peer.receivers() {
            if let Some(MediaStreamTrack::Audio(track)) = receiver.track() {
                let audible = self.volumes.get(&track.id()).copied().unwrap_or(1.0) > 0.0;
                track.set_enabled(!self.controls.deafened && audible);
            }
        }
    }

    async fn answer(&mut self, sdp: &str, events: &Events) -> Result<(), &'static str> {
        let offer = SessionDescription::parse(sdp, SdpType::Offer)
            .map_err(|_| "Сервер прислал некорректное предложение WebRTC.")?;
        self.peer
            .set_remote_description(offer)
            .await
            .map_err(|_| "Не удалось применить предложение WebRTC.")?;
        let remote = RemoteSdp::new(sdp);
        if let Some(old) = self.remote_sdp.replace(remote) {
            self.last_ufrags = old.ufrags;
        }
        let answer = self
            .peer
            .create_answer(AnswerOptions::default())
            .await
            .map_err(|_| "Не удалось создать ответ WebRTC.")?;
        let sdp = answer.to_string();
        self.peer
            .set_local_description(answer)
            .await
            .map_err(|_| "Не удалось применить локальный ответ WebRTC.")?;
        events.push(Event::Answer { sdp });
        self.update_bitrate();
        self.apply_audio_controls();
        for _ in 0..self.pending_ice.len() {
            let Some(candidate) = self.pending_ice.pop_front() else {
                break;
            };
            if self
                .remote_sdp
                .as_ref()
                .is_some_and(|remote| remote.accepts(&candidate))
            {
                self.add_candidate(candidate).await;
            } else if !self.is_old_candidate(&candidate) {
                self.pending_ice.push_back(candidate);
            }
        }
        Ok(())
    }

    fn is_old_candidate(&self, candidate: &Candidate) -> bool {
        candidate
            .ufrag()
            .is_some_and(|ufrag| self.last_ufrags.iter().any(|old| old == ufrag))
    }

    async fn add_candidate(&self, candidate: Candidate) {
        let mid = candidate.sdp_mid.as_deref().unwrap_or_default();
        let index = candidate.sdp_mline_index.map(i32::from).unwrap_or(-1);
        if let Ok(parsed) = IceCandidate::parse(mid, index, &candidate.candidate) {
            let _ = self.peer.add_ice_candidate(parsed).await;
        }
    }

    async fn sample_levels(&mut self, events: &Events) {
        if self.peer.connection_state() != PeerConnectionState::Connected {
            return;
        }
        let Ok(Ok(report)) = time::timeout(Duration::from_millis(125), self.peer.get_stats()).await
        else {
            return;
        };
        let mut local_level = 0.0_f64;
        for stat in report {
            match stat {
                RtcStats::MediaSource(source) => {
                    local_level = local_level.max(source.audio.audio_level);
                }
                RtcStats::InboundRtp(inbound) if inbound.stream.kind == "audio" => {
                    let track_id = inbound.inbound.track_identifier;
                    if track_id.is_empty() {
                        continue;
                    }
                    let level = inbound.inbound.audio_level;
                    let speaking = level >= SPEAKING_THRESHOLD;
                    if let Some(previous) = self.speaking.get_mut(&track_id) {
                        if *previous == speaking {
                            continue;
                        }
                        *previous = speaking;
                    } else {
                        if !speaking || self.speaking.len() >= 64 {
                            continue;
                        }
                        self.speaking.insert(track_id.clone(), speaking);
                    }
                    events.push(Event::Level {
                        track_id: Some(track_id),
                        level,
                    });
                }
                _ => {}
            }
        }
        let speaking =
            local_level >= SPEAKING_THRESHOLD && !self.controls.muted && !self.controls.deafened;
        if self.local_speaking != speaking {
            self.local_speaking = speaking;
            events.push(Event::Level {
                track_id: None,
                level: if speaking { local_level } else { 0.0 },
            });
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some((video, task)) = self.viewer.take() {
            task.abort();
            video.clear();
        }
        self.capture.detach();
        self.peer.on_connection_state_change(None);
        self.peer.on_ice_candidate(None);
        self.peer.on_track(None);
        self.peer.close();
    }
}

struct RemoteSdp {
    ufrags: Vec<String>,
}

impl RemoteSdp {
    fn new(sdp: &str) -> Self {
        let mut ufrags = Vec::new();
        for ufrag in sdp
            .lines()
            .filter_map(|line| line.strip_prefix("a=ice-ufrag:"))
        {
            if !ufrags.iter().any(|value| value == ufrag) {
                ufrags.push(ufrag.to_owned());
            }
        }
        Self { ufrags }
    }

    fn accepts(&self, candidate: &Candidate) -> bool {
        candidate.ufrag().is_none_or(|ufrag| {
            self.ufrags.is_empty() || self.ufrags.iter().any(|value| value == ufrag)
        })
    }
}

impl Candidate {
    fn ufrag(&self) -> Option<&str> {
        self.username_fragment
            .as_deref()
            .or_else(|| candidate_ufrag(&self.candidate))
    }
}

fn candidate_ufrag(candidate: &str) -> Option<&str> {
    let mut tokens = candidate.split_ascii_whitespace().skip(8);
    while let (Some(key), Some(value)) = (tokens.next(), tokens.next()) {
        if key == "ufrag" {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use libwebrtc::{MediaType, rtp_sender::VideoEncoderBackend, video_codec::VideoCodec};

    #[test]
    fn installed_webrtc_build_has_voxhold_web_codecs() {
        let factory = PeerConnectionFactory::default();
        let video = factory.video_encoder_codecs(VideoEncoderBackend::Auto);
        let receive_video = factory.get_rtp_receiver_capabilities(MediaType::Video);
        for (codec, name) in [
            (VideoCodec::VP8, "VP8"),
            (VideoCodec::VP9, "VP9"),
            (VideoCodec::H264, "H264"),
            (VideoCodec::AV1, "AV1"),
        ] {
            assert!(video.contains(&codec), "no {name} encoder: {video:?}");
            let mime = format!("video/{name}");
            assert!(
                receive_video
                    .codecs
                    .iter()
                    .any(|codec| codec.mime_type.eq_ignore_ascii_case(&mime)),
                "no {name} decoder"
            );
        }
        for capabilities in [
            factory.get_rtp_sender_capabilities(MediaType::Audio),
            factory.get_rtp_receiver_capabilities(MediaType::Audio),
        ] {
            assert!(capabilities.codecs.iter().any(|codec| {
                codec.mime_type.eq_ignore_ascii_case("audio/opus")
                    && codec.clock_rate == Some(48_000)
            }));
        }
    }

    #[test]
    fn trickle_ice_is_matched_to_the_offer_generation() {
        let mut candidate = Candidate {
            candidate: "candidate:1 1 udp 1 127.0.0.1 9999 typ host ufrag old".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            username_fragment: None,
        };
        let offer = RemoteSdp::new("v=0\r\na=ice-ufrag:new\r\n");
        assert!(!offer.accepts(&candidate));
        candidate.username_fragment = Some("new".into());
        assert!(offer.accepts(&candidate));
    }
}
