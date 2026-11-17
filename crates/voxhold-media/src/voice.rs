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

/// Permission to transmit is tracked independently of the socket worker, so a
/// blocked `send` cannot keep the microphone open. Transmission requires all
/// of: no leave latch, a confirmed join, server permission and the local
/// mute/deafen/push-to-talk decision.
#[derive(Default)]
pub struct CaptureGate(Mutex<CaptureState>);

#[derive(Default)]
struct CaptureState {
    latched: bool,
    joined: bool,
    server_allowed: bool,
    local_allowed: bool,
    track: Option<RtcAudioTrack>,
}

impl CaptureGate {
    fn is_enabled(state: &CaptureState) -> bool {
        !state.latched && state.joined && state.server_allowed && state.local_allowed
    }

    fn apply(state: &CaptureState) {
        if let Some(track) = &state.track {
            track.set_enabled(Self::is_enabled(state));
        }
    }

    /// Leave/logout/PTT release for the whole attempt: a late `State` command
    /// can never reopen the capture because the latch outranks every flag.
    pub fn latch(&self) {
        let mut state = self.0.lock().unwrap();
        state.latched = true;
        Self::apply(&state);
    }

    /// A new media session starts closed until its own `voice.joined`.
    pub fn reset(&self) {
        let mut state = self.0.lock().unwrap();
        state.latched = false;
        state.joined = false;
        state.server_allowed = false;
        state.local_allowed = false;
        Self::apply(&state);
    }

    pub fn set_joined(&self, joined: bool) {
        let mut state = self.0.lock().unwrap();
        state.joined = joined;
        Self::apply(&state);
    }

    pub fn set_server_allowed(&self, allowed: bool) {
        let mut state = self.0.lock().unwrap();
        state.server_allowed = allowed;
        Self::apply(&state);
    }

    pub fn set_local(&self, allowed: bool) {
        let mut state = self.0.lock().unwrap();
        state.local_allowed = allowed;
        Self::apply(&state);
    }

    fn attach(&self, track: RtcAudioTrack) {
        let mut state = self.0.lock().unwrap();
        track.set_enabled(Self::is_enabled(&state));
        state.track = Some(track);
    }

    fn detach(&self) {
        let mut state = self.0.lock().unwrap();
        if let Some(track) = state.track.take() {
            track.set_enabled(false);
        }
    }

    /// Test hook: the gate state is the one applied to the capture track.
    #[cfg(test)]
    pub fn enabled(&self) -> bool {
        let state = self.0.lock().unwrap();
        Self::is_enabled(&state)
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
            _ = stats.tick(), if session.is_some() => {
                if let Some(session) = &mut session {
                    session.check_devices(&events);
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

    /// Resolve a persisted device id to a current Core Audio index. The legacy
    /// WinMM UI stores `index:name`, so a unique name prefix is accepted; a GUID
    /// from the engine is matched exactly.
    fn resolve(&self, id: &str, input: bool) -> Option<u16> {
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
        if id.is_empty() {
            return (count > 0).then_some(0);
        }
        if let Some((_, old_name)) = id
            .split_once(':')
            .filter(|(index, _)| index.parse::<u32>().is_ok())
        {
            let mut matches =
                (0..count.max(0) as u16).filter(|&index| name(index).starts_with(old_name));
            let first = matches.next();
            first.filter(|_| matches.next().is_none())
        } else {
            (0..count.max(0) as u16).find(|&index| guid(index) == id)
        }
    }

    fn running(&self, input: bool) -> bool {
        if input {
            self.0.recording_is_initialized()
        } else {
            self.0.playout_is_initialized()
        }
    }

    fn stop(&self, input: bool) -> bool {
        if input {
            self.0.stop_recording()
        } else {
            self.0.stop_playout()
        }
    }

    fn start(&self, input: bool) -> bool {
        if input {
            self.0.init_recording() && self.0.start_recording()
        } else {
            self.0.init_playout() && self.0.start_playout()
        }
    }

    /// Point the ADM at one index and restore the requested running state.
    /// The previous device is only released after the new station is set; a
    /// failed set or start leaves the caller able to roll back explicitly.
    fn apply(&self, index: u16, input: bool, running: bool) -> bool {
        let factory = &self.0;
        let was_running = self.running(input);
        if was_running && !self.stop(input) {
            return false;
        }
        let selected = if input {
            factory.set_recording_device(index)
        } else {
            factory.set_playout_device(index)
        };
        if !selected {
            return false;
        }
        !running || self.start(input)
    }

    /// Initial join: choose a device and start the path only if the platform
    /// path is expected to run. Used by `Session::new`.
    fn activate(&self, index: u16, input: bool) -> bool {
        let running = self.running(input);
        self.apply(index, input, running)
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
    /// Only the voice session owns the room output coefficient; the stream
    /// player is a separate engine with its own mute.
    control_playout_gain: bool,
    controls: Controls,
    volumes: HashMap<String, f64>,
    /// last effective coefficient applied per receiver.
    gains: HashMap<String, f64>,
    /// Last room coefficient accepted by the process playout session and when
    /// it was applied. A new WASAPI session is re-applied on the stats tick.
    applied_playout_gain: Option<f64>,
    playout_applied_at: Option<std::time::Instant>,
    speaking: HashMap<String, bool>,
    local_speaking: bool,
    remote_sdp: Option<RemoteSdp>,
    last_ufrags: Vec<String>,
    pending_ice: VecDeque<Candidate>,
    input_index: Option<u16>,
    output_index: Option<u16>,
    /// The id of the device that is actually open, not merely selected. A
    /// hot-unplugged device index can be reused by another device.
    input_opened: Option<String>,
    output_opened: Option<String>,
    device_check: std::time::Instant,
}

#[derive(Clone, Copy, Debug)]
struct SwitchResult {
    switched: bool,
    /// Whether the previous (or default) path was restored after a failure.
    audible: bool,
}

impl Session {
    fn new(
        servers: Vec<IceServer>,
        transport_policy: &str,
        mut controls: Controls,
        events: &Arc<Events>,
        capture: Arc<CaptureGate>,
        viewer: Option<Arc<VideoStore>>,
    ) -> Result<Self, &'static str> {
        let control_playout_gain = viewer.is_none();
        let audio = PlatformAudio::new(control_playout_gain)?;
        // A saved device that is gone at join time must not fail the call:
        // keep the default path and report it once.
        let mut input_index = None;
        let mut output_index = None;
        let mut input_opened = None;
        let mut output_opened = None;
        if let Some(id) = controls.input.clone().filter(|id| !id.is_empty()) {
            if let Some(index) = audio
                .resolve(&id, true)
                .filter(|index| audio.activate(*index, true))
            {
                input_index = Some(index);
                input_opened = Some(id);
            } else {
                // Report the actual result: the saved id is not open.
                controls.input = None;
                events.push(Event::DeviceError {
                    message: "Выбранный микрофон недоступен, используется системный.",
                });
            }
        }
        if let Some(id) = controls.output.clone().filter(|id| !id.is_empty()) {
            if let Some(index) = audio
                .resolve(&id, false)
                .filter(|index| audio.activate(*index, false))
            {
                output_index = Some(index);
                output_opened = Some(id);
            } else {
                controls.output = None;
                events.push(Event::DeviceError {
                    message: "Выбранное устройство воспроизведения недоступно, используется системное.",
                });
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
            control_playout_gain,
            controls,
            volumes: HashMap::new(),
            gains: HashMap::new(),
            applied_playout_gain: None,
            playout_applied_at: None,
            speaking: HashMap::new(),
            local_speaking: false,
            remote_sdp: None,
            last_ufrags: Vec::new(),
            pending_ice: VecDeque::new(),
            input_index,
            output_index,
            input_opened,
            output_opened,
            device_check: std::time::Instant::now(),
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
            Command::State(controls) => self.update_controls(controls, events),
            Command::ViewAudio { muted } => {
                self.controls.deafened = muted;
                self.apply_audio_controls();
                Ok(())
            }
            Command::ViewOutput { output } => {
                if self.controls.output == output {
                    return Ok(());
                }
                let result = match output {
                    Some(id) if !id.is_empty() => {
                        let result = self.switch_device(&id, false);
                        if result.switched {
                            self.controls.output = Some(id);
                        }
                        result
                    }
                    _ => {
                        let result = self.switch_device("", false);
                        if result.switched {
                            self.controls.output = None;
                        }
                        result
                    }
                };
                if result.switched {
                    Ok(())
                } else if result.audible {
                    Err("Не удалось переключить устройство воспроизведения, прежнее работает.")
                } else {
                    Err("Не удалось переключить звук и восстановить прежнее устройство.")
                }
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

    /// Apply controls without ever losing the working audio path. A device is
    /// resolved and the new ADM path is started before the old one is closed;
    /// if that fails the previous index is restored and the caller gets a
    /// non-fatal notice.
    fn update_controls(&mut self, controls: Controls, events: &Events) -> Result<(), &'static str> {
        let deafen_changed = self.controls.deafened != controls.deafened;
        let gain_changed = self.controls.output_gain != controls.output_gain;
        self.controls.muted = controls.muted;
        self.controls.deafened = controls.deafened;
        self.controls.output_gain = controls.output_gain;
        if let Some(bitrate) = controls
            .bitrate_kbps
            .filter(|value| Some(*value) != self.controls.bitrate_kbps)
        {
            self.controls.bitrate_kbps = Some(bitrate);
            self.update_bitrate();
        }
        if deafen_changed || gain_changed {
            self.apply_audio_controls();
        }
        if let Some(input) = controls
            .input
            .filter(|id| self.controls.input.as_ref() != Some(id))
        {
            let result = self.switch_device(&input, true);
            if result.switched {
                self.controls.input = Some(input);
            } else {
                events.push(Event::DeviceError {
                    message: if result.audible {
                        "Не удалось переключить микрофон, прежний работает."
                    } else {
                        "Не удалось переключить микрофон и восстановить прежний."
                    },
                });
            }
        }
        if let Some(output) = controls
            .output
            .filter(|id| self.controls.output.as_ref() != Some(id))
        {
            let result = self.switch_device(&output, false);
            if result.switched {
                self.controls.output = Some(output);
            } else {
                events.push(Event::DeviceError {
                    message: if result.audible {
                        "Не удалось переключить устройство воспроизведения, прежнее работает."
                    } else {
                        "Не удалось переключить звук и восстановить прежнее устройство."
                    },
                });
            }
        }
        Ok(())
    }

    fn switch_device(&mut self, id: &str, input: bool) -> SwitchResult {
        let Some(index) = self.audio.resolve(id, input) else {
            return SwitchResult {
                switched: false,
                audible: self.audio.running(input),
            };
        };
        let previous = if input {
            self.input_index
        } else {
            self.output_index
        };
        let running = self.audio.running(input);
        let opened = if input {
            self.input_opened.as_deref()
        } else {
            self.output_opened.as_deref()
        };
        // The index alone is not identity: after an unplug the same numeric
        // index can belong to another device, and the system default is often
        // index 0. Re-apply unless the same opened id is requested.
        let same_device = if id.is_empty() {
            opened.is_none()
        } else {
            opened == Some(id)
        };
        if previous == Some(index) && same_device {
            return SwitchResult {
                switched: true,
                audible: running,
            };
        }
        if self.audio.apply(index, input, running) {
            if input {
                self.input_index = Some(index);
                self.input_opened = (!id.is_empty()).then(|| id.to_owned());
            } else {
                self.output_index = Some(index);
                self.output_opened = (!id.is_empty()).then(|| id.to_owned());
            }
            return SwitchResult {
                switched: true,
                audible: running,
            };
        }
        // Restore the previous device, or the system default when nothing was
        // selected. A successful set is not enough: the previous running state
        // must start again, otherwise the call stays silent.
        let fallback = previous.unwrap_or(0);
        let audible = self.audio.apply(fallback, input, running);
        if audible {
            if input {
                self.input_index = Some(fallback);
            } else {
                self.output_index = Some(fallback);
            }
        }
        SwitchResult {
            switched: false,
            audible,
        }
    }

    /// A hot-unplugged device cannot be reported by WebRTC; poll the Core
    /// Audio list once a second and fall back to the default path.
    fn check_devices(&mut self, events: &Events) {
        if self.device_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.device_check = std::time::Instant::now();
        for input in [true, false] {
            let selected = if input {
                self.input_opened.as_deref()
            } else {
                self.output_opened.as_deref()
            };
            let Some(id) = selected.filter(|id| !id.is_empty()) else {
                continue;
            };
            if self.audio.resolve(id, input).is_some() {
                continue;
            }
            // The old index may now point at another device. Resolve the
            // system default from scratch instead of reusing it.
            let result = self.switch_device("", input);
            if result.switched {
                if input {
                    self.controls.input = None;
                } else {
                    self.controls.output = None;
                }
                events.push(Event::DeviceError {
                    message: if input {
                        "Микрофон отключён, используется системный."
                    } else {
                        "Устройство воспроизведения отключено, используется системное."
                    },
                });
            } else {
                events.push(Event::DeviceError {
                    message: if result.audible {
                        if input {
                            "Микрофон отключён, прежний путь восстановлен."
                        } else {
                            "Устройство воспроизведения отключено, прежний путь восстановлен."
                        }
                    } else if input {
                        "Микрофон отключён, звук не восстановлен."
                    } else {
                        "Устройство воспроизведения отключено, звук не восстановлен."
                    },
                });
            }
        }
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
        let master = if self.controls.deafened { 0.0 } else { 1.0 };
        if self.control_playout_gain {
            let room = self.controls.output_gain.unwrap_or(1.0).clamp(0.0, 2.0);
            if crate::playout_volume::set_process_gain(room * master) {
                self.applied_playout_gain = Some(room * master);
                self.playout_applied_at = Some(std::time::Instant::now());
            }
        }
        self.gains.clear();
        for receiver in self.peer.receivers() {
            if let Some(MediaStreamTrack::Audio(track)) = receiver.track() {
                let id = track.id();
                // Keep the numeric coefficient: 1 %, 50 %, 100 % and 200 %
                // are distinct settings, not a boolean "audible" flag. Zero
                // disables the track so a mute is immediate.
                let gain = (self.volumes.get(&id).copied().unwrap_or(1.0) * master).clamp(0.0, 2.0);
                self.gains.insert(id, gain);
                track.set_enabled(gain > 0.0);
            }
        }
    }

    /// Effective coefficient used by the PCM path for one receiver. Reapplied
    /// whenever a track reappears or the SDP is renegotiated.
    #[allow(dead_code)]
    pub fn gain_for_track(&self, track_id: &str) -> f64 {
        self.gains.get(track_id).copied().unwrap_or(1.0)
    }

    #[cfg(test)]
    fn test_gain_for_track(&self, track_id: &str) -> f64 {
        self.gain_for_track(track_id)
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
        // A new WASAPI session may appear after the first control command.
        // Re-apply the room coefficient until the endpoint accepts it, then
        // refresh periodically in case the session was recreated.
        if self.control_playout_gain {
            let room = if self.controls.deafened {
                0.0
            } else {
                self.controls.output_gain.unwrap_or(1.0).clamp(0.0, 2.0)
            };
            let stale = self
                .playout_applied_at
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(2));
            if self.applied_playout_gain != Some(room) || stale {
                if crate::playout_volume::set_process_gain(room) {
                    self.applied_playout_gain = Some(room);
                    self.playout_applied_at = Some(std::time::Instant::now());
                }
            }
        }
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
        if self.control_playout_gain {
            // Do not leave the process audio session scaled after the call.
            crate::playout_volume::set_process_gain(1.0);
        }
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
