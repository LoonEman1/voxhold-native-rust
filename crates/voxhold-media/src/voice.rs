use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
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
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};

use crate::{
    Candidate, Command, Controls, Event, Events, IceServer,
    capture::{self, ECHO_CANCELLATION_AVAILABLE, MicCapture},
    dsp::{AudioParams, NoiseMode},
    video::VideoStore,
};

mod output;
use output::{OutputState, SwitchResult};
mod input;
use input::normalize_input;
mod platform;
use platform::PlatformAudio;
mod playout;
use playout::{Playout, PlayoutMode, RetryAt, SinkRecovery};
mod steps;
use steps::Step;
#[cfg(test)]
mod loopback;
mod stats;
#[cfg(test)]
mod test_support;
mod winmm_out;

use crate::mixer::Source;

const ICE_CAPACITY: usize = 64;
const COMPLETED_UFRAG_CAPACITY: usize = 16;
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(8);
const SPEAKING_THRESHOLD: f64 = 0.018;
/// A recovery attempt is only cleared after this much uninterrupted capture.
/// One delivered block is not proof that a driver recovered.
const INPUT_STABLE_CAPTURE: Duration = Duration::from_secs(5);
/// The system WinMM input may open without ever delivering PCM. Bound those
/// reopens so the ladder always reaches the platform ADM.
const MAX_SYSTEM_INPUT_ATTEMPTS: u8 = 2;
/// How long the platform ADM may take to report its first audio-source stats
/// after the device was opened.
const INPUT_CAPTURE_GRACE: Duration = Duration::from_secs(5);

mod capture_gate;
pub use capture_gate::CaptureGate;

pub async fn run(
    mut commands: mpsc::Receiver<Command>,
    events: Arc<Events>,
    capture: Arc<CaptureGate>,
    video: Arc<VideoStore>,
    stop: Arc<tokio::sync::Notify>,
) {
    let mut session: Option<Session> = None;
    let mut stats = time::interval(Duration::from_millis(250));
    stats.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    loop {
        let step = steps::next_step(&mut commands, &stop, &mut stats, session.is_some()).await;
        match step {
            Step::Stopped | Step::Closed => break,
            Step::Stats => {
                if let Some(session) = &mut session {
                    session.tick_playout(&events);
                    session.check_devices(&events);
                    session.sample_levels(&events).await;
                }
            }
            Step::Command(command) => {
                let result = match command {
                    Command::Start {
                        ice_servers,
                        ice_transport_policy,
                        controls,
                    } => {
                        session.take();
                        Session::new(
                            ice_servers,
                            &ice_transport_policy,
                            controls,
                            &events,
                            capture.clone(),
                            None,
                        )
                        .map(|new_session| {
                            session = Some(new_session);
                        })
                    }
                    Command::ViewStart {
                        ice_servers,
                        ice_transport_policy,
                        output,
                    } => {
                        session.take();
                        let controls = Controls {
                            output,
                            ..Controls::default()
                        };
                        Session::new(
                            ice_servers,
                            &ice_transport_policy,
                            controls,
                            &events,
                            capture.clone(),
                            Some(video.clone()),
                        )
                        .map(|new_session| {
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
                    },
                };
                if let Err(message) = result {
                    events.push(Event::Error { message });
                }
            }
        }
    }
}

struct Session {
    peer: PeerConnection,
    sender: Option<RtpSender>,
    viewer: Option<(Arc<VideoStore>, JoinHandle<()>)>,
    capture: Arc<CaptureGate>,
    /// Kept for capture restarts on the same session.
    events: Arc<Events>,
    audio: PlatformAudio,
    /// Software WinMM capture; `None` when the platform ADM fallback owns the
    /// microphone and the software DSP cannot run.
    mic: Option<MicCapture>,
    controls: Controls,
    /// Per-receiver-track playout gain and local mute. The gain is the
    /// participant percentage (0–200 %, resolved by the controller from the
    /// stable user id); `muted` is the separate local mute/deafen flag, so
    /// unmuting restores the percentage instead of a remembered zero.
    sources: HashMap<String, Source>,
    /// Software room playout. `None` means the platform ADM plays the room and
    /// the numeric gain is not applied (reported to the UI).
    playout: Option<Playout>,
    /// The room session owns the playout rather than the ADM. Set even while a
    /// suspend has released the sink, so resume reopens it instead of silently
    /// falling back to an ADM path that was never started.
    software_playout: bool,
    /// Current playout path. `AdmFallback` means the mixer budget was spent.
    playout_mode: PlayoutMode,
    /// Recovery state machine of the software sink (bounded shared budget,
    /// stability proof, phase for the report).
    playout_attempts: SinkRecovery,
    /// Slow retry of the ADM path after a fallback that could not start or
    /// stopped delivering PCM.
    adm_retry: RetryAt,
    /// Last observed ADM playout progress for the platform fallback watchdog.
    adm_playout_duration: Option<f64>,
    adm_playout_progress_at: std::time::Instant,
    adm_playout_started_at: std::time::Instant,
    /// Test-only guard: the headless session must never touch platform audio.
    #[cfg(test)]
    test_no_adm: bool,
    /// Suspend released the sink; only an explicit resume reopens it.
    playout_suspended: bool,
    /// Successful software-sink reopens in this session.
    playout_reconnects: u32,
    /// Sink factory; tests substitute a fake without a Windows device.
    playout_open: Arc<winmm_out::OpenSink>,
    /// Source tasks of the software playout; they end with the track and are
    /// aborted when the session drops.
    playout_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    /// Tokio handle for spawning the audio-sink tasks from the WebRTC
    /// callback thread.
    runtime: tokio::runtime::Handle,
    last_playout_report: std::time::Instant,
    speaking: HashMap<String, bool>,
    local_speaking: bool,
    remote_sdp: Option<RemoteSdp>,
    /// ICE generations already replaced by a newer remote description. Kept
    /// bounded so a late candidate from an old restart is discarded.
    completed_ufrags: Vec<String>,
    pending_ice: VecDeque<Candidate>,
    input_index: Option<u16>,
    output_index: Option<u16>,
    /// The id of the device that is actually open, not merely selected. A
    /// hot-unplugged device index can be reused by another device.
    input_opened: Option<String>,
    /// Software capture liveness. `failed` comes from a WinMM requeue/reset
    /// error; a stalled progress counter catches a driver that stopped
    /// delivering buffers without reporting an error (USB unplug).
    input_progress: u64,
    input_progress_at: std::time::Instant,
    /// When the current capture path was opened. Used for the startup grace
    /// before the platform path is expected to report PCM.
    input_started_at: std::time::Instant,
    /// Last `total_samples_duration` from the platform ADM's audio source.
    /// The pinned WebRTC build does not populate `total_samples_captured`, so
    /// this cumulative duration is the real proof that PCM is arriving.
    /// `None` means the stats have not reported yet.
    input_duration: Option<f64>,
    /// The peer has connected since the platform path was opened. The ADM
    /// watchdog starts its startup grace from this point, not from the device
    /// switch.
    input_connected: bool,
    /// Both software and platform fallbacks failed. Retry only on an explicit
    /// device selection, not every two seconds.
    input_recovery_failed: bool,
    /// The selected id was already reopened once in this failure episode. If
    /// it stays silent, the next recovery must use the system input.
    input_reopen_attempted: bool,
    /// System WinMM reopens in this failure episode. A driver can open the
    /// system device and never deliver PCM, so this is bounded separately
    /// from the selected-device retry; after the limit the ladder must reach
    /// the platform ADM.
    input_system_attempts: u8,
    /// Start of the current uninterrupted progress run. A single block must
    /// not clear the recovery counters; only sustained capture does.
    input_stable_since: Option<std::time::Instant>,
    output: OutputState,
    device_check: std::time::Instant,
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
        let audio = PlatformAudio::new(false)?;
        let room_session = viewer.is_none();
        // The software mixer is the only path with a numeric participant gain.
        // It is opened before the ADM playout is disabled: a sink that cannot
        // open must leave the working ADM path in place and be reported, not
        // silently degrade the call to silence.
        let mut initial_playout = None;
        let mut software_playout = false;
        let playout_open: Arc<winmm_out::OpenSink> = Arc::new(Box::new(winmm_out::default_open));
        if room_session {
            match Playout::start_with(controls.output.as_deref(), playout_open.clone()) {
                Ok(started) => {
                    audio.set_playout(false);
                    initial_playout = Some(started);
                    software_playout = true;
                }
                Err(_) => events.push(Event::DeviceError {
                    message: "Программный микшер не открылся: громкость участников не применяется, звук идёт напрямую.",
                }),
            }
        }
        let playout_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(Vec::new()));
        // A saved device that is gone at join time must not fail the call:
        // keep the default path and report it once. The software capture path
        // opens the recording device itself; the platform ADM recording side
        // stays closed unless that path falls back.
        let mut input_index = None;
        let output_index =
            output::activate_saved_output(&audio, &mut controls, initial_playout.is_none(), events);
        let mut input_opened = None;
        let output = OutputState::new(controls.output.clone());
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
        let mut mic: Option<MicCapture> = None;
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
            let clip = initial_playout.as_ref().map(Playout::clip);
            let runtime = tokio::runtime::Handle::current();
            let tasks = playout_tasks.clone();
            peer.on_track(Some(Box::new(move |event| {
                if let MediaStreamTrack::Audio(track) = &event.track {
                    let track_id = track.id();
                    track_events.push(Event::Track {
                        track_id: track_id.clone(),
                        stream_id: event.streams.first().map(|stream| stream.id()),
                        mid: event.transceiver.mid(),
                    });
                    // The mixer receives decoded PCM per receiver. The sink is
                    // created with the room playout and follows a resume.
                    if let Some(clip) = &clip {
                        let task = clip.attach(track_id, track.clone(), runtime.clone());
                        if let Ok(mut tasks) = tasks.lock() {
                            tasks.push(task);
                        }
                    }
                }
            })));
            // The software path owns capture so the gate, gain and mute are
            // provable over the exact PCM that reaches Opus. The platform ADM
            // is kept as a capability fallback for devices WinMM cannot open.
            capture.set_params(controls.audio_params());
            let params = capture.params();
            let source = libwebrtc::audio_source::native::NativeAudioSource::new(
                capture::source_options(),
                capture::SAMPLE_RATE,
                capture::CHANNELS,
                0,
            );
            let mut software = MicCapture::new(
                source.clone(),
                params,
                capture.permission_epoch(),
                Some(capture.clone()),
                events.clone(),
            );
            let mut software_mic = None;
            let selected_input = normalize_input(controls.input.clone());
            let (track, applied) = match software.start(selected_input.as_deref()) {
                Ok(()) => {
                    controls.input = selected_input.clone();
                    input_opened = selected_input;
                    input_index = None;
                    software_mic = Some(software);
                    let applied = controls.audio_params();
                    let track = factory.create_audio_track("voxhold-microphone", source.clone());
                    (
                        track,
                        (
                            "software",
                            applied.mode.as_str(),
                            ECHO_CANCELLATION_AVAILABLE && applied.echo_cancellation,
                            applied.auto_gain_control,
                        ),
                    )
                }
                Err(_) => {
                    // A temporary failure of the saved device must not drop the
                    // software DSP: try the system WinMM input before ADM.
                    let selected = controls.input.clone().filter(|id| !id.is_empty());
                    if software.start(None).is_ok() {
                        controls.input = None;
                        input_opened = None;
                        input_index = None;
                        software_mic = Some(software);
                        if selected.is_some() {
                            events.push(Event::DeviceError {
                                message: "Выбранный микрофон недоступен, используется системный.",
                            });
                        }
                        let applied = controls.audio_params();
                        let track =
                            factory.create_audio_track("voxhold-microphone", source.clone());
                        (
                            track,
                            (
                                "software",
                                applied.mode.as_str(),
                                ECHO_CANCELLATION_AVAILABLE && applied.echo_cancellation,
                                applied.auto_gain_control,
                            ),
                        )
                    } else {
                        // Its ADM applies its own NS/AGC and may enable echo
                        // cancellation on the render reference; the software
                        // toggles do not apply.
                        audio.set_recording(true);
                        let mut started = false;
                        if let Some(id) = controls.input.as_deref().filter(|id| !id.is_empty()) {
                            if let Some(index) = audio
                                .resolve(id, true)
                                .filter(|index| audio.apply(*index, true, true))
                            {
                                input_index = Some(index);
                                input_opened = Some(id.to_owned());
                                started = true;
                            } else {
                                controls.input = None;
                                input_index = None;
                                input_opened = None;
                                events.push(Event::DeviceError {
                                    message:
                                        "Выбранный микрофон недоступен, используется системный.",
                                });
                            }
                        }
                        // Start the system ADM too when no saved device was
                        // open. The start result is checked: initialization
                        // alone does not prove that recording began.
                        if !started {
                            if let Some(index) = audio
                                .resolve("", true)
                                .filter(|index| audio.apply(*index, true, true))
                            {
                                input_index = Some(index);
                                started = true;
                            }
                        }
                        if !started {
                            events.push(Event::DeviceError {
                                message: "Системный микрофон ADM недоступен.",
                            });
                        }
                        let track = factory.create_device_audio_track("voxhold-microphone");
                        // The ADM applies its own NS/AGC. Its echo canceller is
                        // not measured here (no render-reference probe), so the
                        // applied snapshot must not report AEC as enabled.
                        (track, ("platform", NoiseMode::Auto.as_str(), false, true))
                    }
                }
            };
            // The gate attaches after `add_track`; keep the new track silent
            // until it has applied the current permission state.
            track.set_enabled(false);
            let sender = peer
                .add_track(MediaStreamTrack::Audio(track.clone()), &["voxhold-voice"])
                .map_err(|_| "Не удалось подключить микрофон к WebRTC.")?;
            if let Some(mic) = &software_mic {
                capture.attach_source(track, source, mic.id());
            } else {
                capture.attach(track);
            }
            mic = software_mic;
            events.push(Event::Audio {
                capture: applied.0,
                noise_mode: applied.1,
                echo_cancellation: applied.2,
                auto_gain_control: applied.3,
                software_mixer: software_playout,
            });
            (Some(sender), None)
        };
        // Typed result of the device selection the session actually opened.
        // The UI uses it instead of clearing a saved id on any late device
        // notice.
        events.push(Event::InputApplied {
            generation: 0,
            input: controls.input.clone(),
            error: None,
        });
        let input_progress = mic.as_ref().map(MicCapture::progress).unwrap_or(0);
        let session = Self {
            peer,
            sender,
            viewer,
            capture,
            events: Arc::clone(events),
            audio,
            mic,
            controls,
            sources: HashMap::new(),
            playout: None,
            software_playout: false,
            playout_mode: PlayoutMode::AdmFallback,
            playout_attempts: SinkRecovery::default(),
            adm_retry: RetryAt::default(),
            adm_playout_duration: None,
            adm_playout_progress_at: std::time::Instant::now(),
            adm_playout_started_at: std::time::Instant::now(),
            #[cfg(test)]
            test_no_adm: false,
            playout_suspended: false,
            playout_reconnects: 0,
            playout_open,
            playout_tasks,
            runtime: tokio::runtime::Handle::current(),
            last_playout_report: std::time::Instant::now() - Duration::from_secs(1),
            speaking: HashMap::new(),
            local_speaking: false,
            remote_sdp: None,
            completed_ufrags: Vec::new(),
            pending_ice: VecDeque::new(),
            input_index,
            output_index,
            input_opened,
            input_progress,
            input_progress_at: std::time::Instant::now(),
            input_started_at: std::time::Instant::now(),
            input_duration: None,
            input_connected: false,
            input_recovery_failed: false,
            input_reopen_attempted: false,
            input_system_attempts: 0,
            input_stable_since: None,
            output,
            device_check: std::time::Instant::now(),
        };
        // The very first sink enters the same stability machine as a reopened
        // one; without this the phase stays `Opening` forever and the budget is
        // never cleared for a normal call.
        Ok(session.adopt_initial_playout(initial_playout))
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
            Command::SelectInput { input, generation } => {
                self.select_input(input, generation, events)
            }
            Command::ViewAudio { muted } => {
                self.controls.deafened = muted;
                self.apply_audio_controls();
                Ok(())
            }
            Command::ViewOutput { output } => {
                if !self.output.needs_switch(&output) {
                    return Ok(());
                }
                // The preference is recorded even when the switch fails. The
                // applied endpoint and working flag stay separate, so a later
                // repeat of the same device still starts the path again.
                self.output.request(output.clone());
                self.controls.output = output.clone();
                let result = match &output {
                    Some(id) if !id.is_empty() => self.switch_device(id, false, events),
                    _ => self.switch_device("", false, events),
                };
                if result.switched || result.audible {
                    self.output.confirmed(result.applied.clone());
                } else {
                    self.output.failed();
                    self.output_index = None;
                }
                let error = if result.switched {
                    None
                } else if result.audible {
                    Some("Не удалось переключить устройство воспроизведения, прежнее работает.")
                } else {
                    Some("Не удалось переключить звук и восстановить прежнее устройство.")
                };
                events.push(Event::Output {
                    applied: result.applied,
                    audible: result.audible,
                    working: result.switched || result.audible,
                    error,
                });
                Ok(())
            }
            Command::Volume {
                track_id,
                gain,
                muted,
            } => {
                if self.sources.len() < 64 || self.sources.contains_key(&track_id) {
                    self.sources.insert(
                        track_id,
                        Source {
                            gain: gain.clamp(0.0, 2.0),
                            muted,
                        },
                    );
                    self.apply_audio_controls();
                }
                Ok(())
            }
            Command::Resume => {
                self.resume_playout(events);
                Ok(())
            }
            Command::Suspend => {
                self.suspend_playout(events);
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
        let dsp_changed = self.controls.noise_mode != controls.noise_mode
            || self.controls.noise_gate_percent != controls.noise_gate_percent
            || (self.controls.input_gain - controls.input_gain).abs() > f32::EPSILON
            || self.controls.echo_cancellation != controls.echo_cancellation
            || self.controls.auto_gain_control != controls.auto_gain_control;
        let requested_output = controls.output.clone();
        let output_needs_switch = self.output.needs_switch(&requested_output);
        self.controls.muted = controls.muted;
        self.controls.deafened = controls.deafened;
        self.controls.noise_mode = controls.noise_mode;
        self.controls.noise_gate_percent = controls.noise_gate_percent;
        self.controls.input_gain = controls.input_gain;
        self.controls.echo_cancellation = controls.echo_cancellation;
        self.controls.auto_gain_control = controls.auto_gain_control;
        self.controls.output_gain = controls.output_gain;
        // The capture thread reads this snapshot; the permission flags keep
        // `muted` authoritative even if an older queued state arrives late.
        self.capture.set_params(controls.audio_params());
        if dsp_changed {
            self.report_audio(events);
        }
        self.output.request(requested_output.clone());
        self.controls.output = requested_output.clone();
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
        // Input changes during a call use `SelectInput` only. A stale generic
        // `State` queued before the user picked another device must not switch
        // the microphone back.
        if output_needs_switch {
            let result = match requested_output.as_deref() {
                Some(id) if !id.is_empty() => self.switch_device(id, false, events),
                _ => self.switch_device("", false, events),
            };
            if result.switched || result.audible {
                self.output.confirmed(result.applied.clone());
            } else {
                self.output.failed();
                self.output_index = None;
            }
            if !result.switched {
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

    fn switch_device(&mut self, id: &str, input: bool, events: &Events) -> SwitchResult {
        // The room playout is not the ADM: switching its sink is a WinMM open,
        // and the previous device stays open until the new one succeeded.
        if !input && self.playout.is_some() {
            return self.switch_playout(id, events);
        }
        let opened_id = if input {
            self.input_opened.clone()
        } else {
            self.output.applied.clone()
        };
        let Some(index) = self.audio.resolve(id, input) else {
            return SwitchResult {
                switched: false,
                audible: self.audio.running(input),
                applied: opened_id,
            };
        };
        let previous = if input {
            self.input_index
        } else {
            self.output_index
        };
        // A completed rollback may have stopped the old path. The next switch
        // must start playout again even though Core Audio reports it stopped.
        let running = self.audio.running(input) || (!input && !self.output.working);
        // The index alone is not identity: after an unplug the same numeric
        // index can belong to another device, and the system default is
        // resolved from Windows, not fixed at index 0. Re-apply unless the same
        // opened id is requested and that path is still known to work.
        let same_device = if id.is_empty() {
            opened_id.is_none()
        } else {
            opened_id.as_deref() == Some(id)
        };
        let confirmed = input || self.output.working;
        if previous == Some(index) && same_device && confirmed {
            return SwitchResult {
                switched: true,
                audible: running,
                applied: opened_id,
            };
        }
        if self.audio.apply(index, input, running) {
            let applied = (!id.is_empty()).then(|| id.to_owned());
            if input {
                self.input_index = Some(index);
                self.input_opened = applied.clone();
            } else {
                self.output_index = Some(index);
            }
            return SwitchResult {
                switched: true,
                audible: running,
                applied,
            };
        }
        // Restore the previous device by its GUID/name, not the saved numeric
        // index: after re-enumeration that index may belong to another device.
        // Empty means the fresh Windows default endpoint.
        let fallback_id = opened_id.as_deref().unwrap_or("");
        let fallback = self
            .audio
            .resolve(fallback_id, input)
            .filter(|index| self.audio.apply(*index, input, running));
        let audible = fallback.is_some();
        if let Some(index) = fallback {
            if input {
                self.input_index = Some(index);
            } else {
                self.output_index = Some(index);
            }
        }
        SwitchResult {
            switched: false,
            audible,
            applied: audible.then_some(opened_id.clone()).flatten(),
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

    async fn answer(&mut self, sdp: &str, events: &Events) -> Result<(), &'static str> {
        let offer = SessionDescription::parse(sdp, SdpType::Offer)
            .map_err(|_| "Сервер прислал некорректное предложение WebRTC.")?;
        self.peer
            .set_remote_description(offer)
            .await
            .map_err(|_| "Не удалось применить предложение WebRTC.")?;
        let remote = RemoteSdp::new(sdp);
        if let Some(old) = self.remote_sdp.replace(remote) {
            for ufrag in old.ufrags {
                let current = self
                    .remote_sdp
                    .as_ref()
                    .is_some_and(|remote| remote.ufrags.iter().any(|value| value == &ufrag));
                if !current && !self.completed_ufrags.iter().any(|value| value == &ufrag) {
                    self.completed_ufrags.push(ufrag);
                }
            }
            if self.completed_ufrags.len() > COMPLETED_UFRAG_CAPACITY {
                let excess = self.completed_ufrags.len() - COMPLETED_UFRAG_CAPACITY;
                self.completed_ufrags.drain(..excess);
            }
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
        candidate.ufrag().is_some_and(|ufrag| {
            self.completed_ufrags
                .iter()
                .any(|old| old.as_str() == ufrag)
        })
    }

    async fn add_candidate(&self, candidate: Candidate) {
        let mid = candidate.sdp_mid.as_deref().unwrap_or_default();
        let index = candidate.sdp_mline_index.map(i32::from).unwrap_or(-1);
        if let Ok(parsed) = IceCandidate::parse(mid, index, &candidate.candidate) {
            let _ = self.peer.add_ice_candidate(parsed).await;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.playout_tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
        self.playout.take();
        if let Some(mut mic) = self.mic.take() {
            mic.stop();
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

mod sdp;
use sdp::{RemoteSdp, candidate_ufrag};

#[cfg(test)]
mod tests;
