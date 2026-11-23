//! One WebRTC publisher per screen-share attempt.
//!
//! The engine owns the capture, the peer connection and the video track. It
//! never talks to the socket: the state machine in `publisher` decides what to
//! send, and `runtime` delivers the events.
//!
//! The SFU creates the offer, so the publisher adds a **sendonly** video
//! transceiver before any remote description. The pinned `libwebrtc 0.4.0`
//! exposes no `get_transceivers`, so the browser path "flip the recvonly
//! transceiver after setRemoteDescription" is not available; adding the
//! transceiver first makes the answer put it on the offered m-line. The
//! in-process loopback test proves the answer keeps the offered mid, is
//! `a=sendonly` and that decoded frames arrive at the SFU.
//!
//! The requested SDR codec is negotiated, not assumed: the video transceiver's
//! codec preferences are pinned to the checked SDR capability, the offer and
//! the answer are verified (mid, primary payload, FMTP profile, RTX `apt`), and
//! a re-offer after an ICE restart must keep the same pair. Transmission is
//! confirmed only by encoded frames plus the codec statistics they point at;
//! ICE and the answer prove connectivity alone.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use libwebrtc::{
    MediaType,
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    ice_candidate::IceCandidate,
    media_stream_track::MediaStreamTrack,
    peer_connection::{PeerConnection, PeerConnectionState},
    peer_connection_factory::{
        IceServer as RtcIceServer, IceTransportsType, PeerConnectionFactory, RtcConfiguration,
        native::PeerConnectionFactoryExt,
    },
    rtp_sender::VideoEncoderBackend,
    stats::QualityLimitationReason,
    video_source::{VideoResolution, native::NativeVideoSource},
};
use tokio::sync::mpsc;

use crate::{
    Candidate, IceServer,
    publisher::{
        BitrateResult, Codec, PublishRequest, Quality, QualityOp,
        bitrate::{
            Limits, apply_sender_limits, confirmed_quality, failure_message, readback_limits,
        },
    },
    screen_capture::{LatestImage, PreviewDemand, PreviewStore, ScreenCapture},
    system_audio::{AudioProducer, CHANNELS, SAMPLE_RATE, SystemAudio},
    video_encoder::{GpuEncoder, HardwareEncoderIdentity, open_gpu_encoder},
};

/// How long the capture may deliver nothing before the attempt is failed.
/// Covers a minimized window, a closed window and an unplugged monitor that
/// the capturer reports late or not at all.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// A disconnection that does not recover is treated as a dead SFU session.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Bounded pending ICE while no remote description exists (server contract).
const ICE_CAPACITY: usize = 64;
/// The server may hold up to 64 pending candidates; a burst plus an offer must
/// fit without turning into a fatal command-channel overflow.
const ENGINE_COMMAND_CAPACITY: usize = 64;
const ENGINE_EVENT_CAPACITY: usize = 32;
/// The outbound stats poll is cheap but must not stall command handling.
const STATS_INTERVAL: Duration = Duration::from_secs(1);
const STATS_TIMEOUT: Duration = Duration::from_millis(500);
/// ICE and the answer prove connectivity, never transmission. The first
/// encoded frame and its reported codec must arrive inside this window, or the
/// attempt ends with a codec diagnostic instead of a silent black stream.
const CODEC_CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);
/// A worker that does not finish after its channel closed is detached instead
/// of hanging the runtime forever.
const WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(5);
/// The session cancelled the preparation between two stages.
const PREP_CANCELLED: &str = "Подготовка трансляции отменена.";

/// The plan asks for a GPU/CPU share; this build has no GPU probe. The pinned
/// `OutboundRtpStats` does report `quality_limitation_reason`, which is the
/// honest equivalent of the web diagnostics: none/cpu/bandwidth/other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QualityLimitation {
    #[default]
    None,
    Cpu,
    Bandwidth,
    Other,
}

impl QualityLimitation {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "нет",
            Self::Cpu => "CPU",
            Self::Bandwidth => "сеть",
            Self::Other => "другое",
        }
    }

    fn from_reason(reason: QualityLimitationReason) -> Self {
        match reason {
            QualityLimitationReason::None => Self::None,
            QualityLimitationReason::Cpu => Self::Cpu,
            QualityLimitationReason::Bandwidth => Self::Bandwidth,
            QualityLimitationReason::Other => Self::Other,
        }
    }
}

/// Measured counters of the publishing path. FPS, resolution and limitation
/// come from `OutboundRtpStats`; the drop counter belongs to the capture slot.
/// There is no CPU or GPU percentage probe in this build, so none is invented.
/// A/V offset and source-change events are also absent: W04a publishes one
/// source, and the A/V synchronization/perf stage will add measured values
/// rather than an estimate here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PublishStats {
    /// Encoded frames per second reported by the outbound RTP stats.
    pub fps: f32,
    /// Captured frames replaced before the encoder read them.
    pub dropped: u64,
    pub width: u32,
    pub height: u32,
    /// Active screen sources of this publication. The stage publishes one.
    pub sources: u32,
    pub limitation: QualityLimitation,
    /// The engine really sends the system-audio track.
    pub audio_active: bool,
    /// Encoded audio bitrate measured over the interval between two stats
    /// polls. `0` means the first sample has not produced a delta yet.
    pub audio_kbps: f32,
    /// Cumulative audio RTP packets reported by the outbound stats.
    pub audio_packets_sent: u64,
    /// 10 ms blocks the capture queue dropped because the encoder was behind.
    pub audio_dropped: u64,
    /// The codec the live sender really encoded, confirmed from the outbound
    /// statistics and the `RtcStats::Codec` they point at. `None` until the
    /// first encoded frame; ICE and the answer never set it.
    pub confirmed_codec: Option<Codec>,
}

impl Default for PublishStats {
    fn default() -> Self {
        Self {
            fps: 0.0,
            dropped: 0,
            width: 0,
            height: 0,
            sources: 1,
            limitation: QualityLimitation::None,
            audio_active: false,
            audio_kbps: 0.0,
            audio_packets_sent: 0,
            audio_dropped: 0,
            confirmed_codec: None,
        }
    }
}

impl PublishStats {
    /// One honest line for the on-air card.
    pub fn describe(self) -> String {
        let audio = if !self.audio_active {
            "без звука".to_owned()
        } else if self.audio_kbps > 0.0 {
            format!("звук ~{:.0} кбит/с", self.audio_kbps)
        } else {
            "звук включён".to_owned()
        };
        format!(
            "{:.0} FPS · сброс {} · {}×{} · ограничение: {} · источников {} · {audio}",
            self.fps,
            self.dropped,
            self.width,
            self.height,
            self.limitation.label(),
            self.sources
        )
    }
}

/// An event that belongs to the current publishing attempt.
pub use crate::publisher::EngineEvent;

mod frames;
use frames::{frame_loop, gpu_frame_loop};

mod queue;
use queue::EventQueue;

/// Capture-side counters that the outbound stats poll merges into one
/// `PublishStats`. The frame loop owns them; the command loop only reads.
#[derive(Default)]
struct FrameCounters {
    dropped: AtomicU64,
    fps_bits: AtomicU32,
    width: AtomicU32,
    height: AtomicU32,
}

/// The limits the worker really confirmed on the negotiated senders. They are
/// written only after `RtpSender::parameters()` read the values back, never
/// from the requested quality, so a refused change cannot look applied here.
/// The loopback test reads this value.
#[derive(Default)]
struct AppliedLimits {
    video_bps: AtomicU64,
    audio_bps: AtomicU64,
}

impl AppliedLimits {
    fn store(&self, limits: &Limits) {
        if let Some(video) = limits.video_bps {
            self.video_bps.store(video, Ordering::Relaxed);
        }
        self.audio_bps
            .store(limits.audio_bps.unwrap_or(0), Ordering::Relaxed);
    }

    #[cfg(test)]
    fn pair(&self) -> (u64, u64) {
        (
            self.video_bps.load(Ordering::Relaxed),
            self.audio_bps.load(Ordering::Relaxed),
        )
    }
}

mod audio;
use audio::{AudioCounters, AudioMeter, audio_loop};

/// Codec negotiation of one publication: the requested SDR payload, the
/// offered mid and the RTX pair.
mod negotiation;
/// The frame producer contract and its implementations.
mod producer;
/// The outbound statistics poll and the raw video stream it reports.
mod stats;
use producer::DesktopProducer;
#[cfg(test)]
use producer::StalledProducer;
pub use producer::{FrameProducer, SyntheticProducer};

enum Command {
    Offer(String),
    Ice(Candidate),
    /// A live bitrate change: no renegotiation, no new `stream.start`. Both
    /// tags travel back with the result so a stale operation cannot be
    /// mistaken for the current one.
    Bitrate {
        attempt: u64,
        op: QualityOp,
        quality: Quality,
    },
    /// Test hook: shorten the codec-confirmation window so the "connected but
    /// no encoded frame" regression does not cost ten seconds per run. Only
    /// test builds can create this command.
    #[cfg(test)]
    ConfirmTimeout(Duration),
    Stop,
}

/// Answer of one live bitrate command.
///
/// `Sent` only proves that the command reached the bounded worker queue, not
/// that the senders accepted it; the verified result arrives later as
/// [`crate::publisher::EngineEvent::Bitrate`]. `Full` is a burst of the queue,
/// not a dead worker: the runtime keeps the newest quality and retries it on
/// the next tick. Only `Closed` means the worker and its capture are gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitrateSend {
    Sent,
    Full,
    Closed,
}

/// Non-blocking handle of one publishing engine. Dropping it releases the
/// capture and closes the peer.
pub struct PublisherEngine {
    /// `None` after `Drop` started: closing the channel is what wakes the
    /// worker, so `join` never depends on `recv()` with a live sender.
    commands: Option<mpsc::Sender<Command>>,
    events: Arc<EventQueue>,
    preview: Arc<PreviewStore>,
    ice_dropped: AtomicU64,
    /// Read by the loopback test through `applied_bitrate_limits`; production
    /// only writes it from the worker.
    #[cfg_attr(not(test), allow(dead_code))]
    limits: Arc<AppliedLimits>,
    has_audio: bool,
    /// Actual scope of the opened system-audio capture.
    audio_scope: Option<String>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PublisherEngine {
    /// Open the capture, the loopback and the peer of one attempt.
    ///
    /// `cancelled` is checked before and after every blocking stage. A stage
    /// that is already running may finish, but its product is dropped here on
    /// the preparing thread and never reaches `stream.start`.
    pub fn start(
        request: PublishRequest,
        servers: Vec<IceServer>,
        policy: String,
        preview: Arc<PreviewStore>,
        demand: Arc<PreviewDemand>,
        cancelled: &AtomicBool,
    ) -> Result<Self, String> {
        if cancelled.load(Ordering::SeqCst) {
            return Err(PREP_CANCELLED.into());
        }
        let latest = Arc::new(LatestImage::default());
        let target = request.capture_target();
        let capture = ScreenCapture::start(
            request.source.clone(),
            target,
            request.quality.fps,
            latest.clone(),
            demand.clone(),
        )
        .map_err(str::to_owned)?;
        if cancelled.load(Ordering::SeqCst) {
            return Err(PREP_CANCELLED.into());
        }
        // The loopback is opened before the engine reports itself ready, so
        // `stream.start` can carry the real `has_audio`. A missing endpoint is
        // an explicit capability error, never a silent video-only stream. The
        // user's scope is resolved here, on the preparation thread: a window
        // becomes its process id and the whole-screen choice becomes an
        // exclude-self process loopback. No failure falls back to the device.
        let mut audio_scope = None;
        let audio: Option<Box<dyn AudioProducer>> = if request.audio {
            let scope = request
                .audio_scope
                .resolve(&request.source)
                .map_err(|error| format!("Системный звук недоступен: {error}"))?;
            match SystemAudio::start(scope) {
                Ok(capture) => {
                    audio_scope = Some(scope.describe());
                    Some(Box::new(capture))
                }
                Err(error) => {
                    return Err(format!("Системный звук недоступен: {error}"));
                }
            }
        } else {
            None
        };
        if cancelled.load(Ordering::SeqCst) {
            return Err(PREP_CANCELLED.into());
        }
        // The hardware session is activated here, on the preparation thread:
        // an activation or type error must fail this attempt before
        // `stream.start`, never after the server created the publication.
        let gpu = if request.encoder.is_gpu() {
            let (width, height) = request.capture_size();
            let query = crate::video_encoder::EncodeQuery::new(
                width,
                height,
                request.quality.fps.clamp(1, 120),
            );
            let Some(resolved) = request.encoder.resolved(request.codec, query) else {
                return Err(
                    "Аппаратный кодировщик этой попытки не подтверждён; выберите видеокарту снова."
                        .into(),
                );
            };
            let Some(adapter) = resolved.adapter.as_ref() else {
                return Err("У выбранной видеокарты нет аппаратного кодировщика.".into());
            };
            Some(open_gpu_encoder(
                adapter,
                request.codec,
                query,
                request.quality.bitrate_kbps.saturating_mul(1000),
            )?)
        } else {
            None
        };
        if cancelled.load(Ordering::SeqCst) {
            return Err(PREP_CANCELLED.into());
        }
        let engine = match gpu {
            Some(gpu) => Self::spawn(
                request.clone(),
                servers,
                policy,
                Box::new(DesktopProducer { capture, latest }),
                audio,
                audio_scope,
                preview,
                true,
                Some(gpu),
            )?,
            None => Self::spawn(
                request.clone(),
                servers,
                policy,
                Box::new(DesktopProducer { capture, latest }),
                audio,
                audio_scope,
                preview,
                true,
                None,
            )?,
        };
        if cancelled.load(Ordering::SeqCst) {
            drop(engine);
            return Err(PREP_CANCELLED.into());
        }
        Ok(engine)
    }

    /// Test entry point: the same engine with a frame producer that does not
    /// need a desktop. `frame_size` may differ from the declared quality, like
    /// a 21:9 monitor inside a 720p publication. A synthetic audio producer
    /// replaces the WASAPI loopback for the in-process SFU tests.
    #[cfg(test)]
    pub(crate) fn start_synthetic(
        request: PublishRequest,
        servers: Vec<IceServer>,
        policy: String,
        preview: bool,
        frame_size: (u32, u32),
        audio: Option<Box<dyn AudioProducer>>,
    ) -> Result<Self, String> {
        Self::spawn(
            request,
            servers,
            policy,
            Box::new(SyntheticProducer::new(frame_size.0, frame_size.1, preview)),
            audio,
            None,
            Arc::new(PreviewStore::default()),
            true,
            None,
        )
    }

    /// Test entry point: the same synthetic publication with an injected
    /// hardware encoder. `plan` names the GPU path; the encoder is the double
    /// the test drives, so the engine code is exactly the production one.
    #[cfg(test)]
    pub(crate) fn start_synthetic_gpu(
        request: PublishRequest,
        servers: Vec<IceServer>,
        policy: String,
        frame_size: (u32, u32),
        encoder: Box<dyn GpuEncoder>,
    ) -> Result<Self, String> {
        Self::spawn(
            request,
            servers,
            policy,
            Box::new(SyntheticProducer::new(frame_size.0, frame_size.1, false)),
            None,
            None,
            Arc::new(PreviewStore::default()),
            false,
            Some(encoder),
        )
    }

    /// Test entry point: the capture never delivers a frame and the source
    /// injects no black-frame keepalive, so the encoder cannot produce a frame
    /// even while ICE connects. Used by the "connected but no encoded frames"
    /// regression.
    #[cfg(test)]
    pub(crate) fn start_synthetic_stalled(
        request: PublishRequest,
        servers: Vec<IceServer>,
        policy: String,
    ) -> Result<Self, String> {
        Self::spawn(
            request,
            servers,
            policy,
            Box::new(StalledProducer),
            None,
            None,
            Arc::new(PreviewStore::default()),
            false,
            None,
        )
    }

    /// Whether the peer really carries the system-audio track. The state
    /// machine forwards this value as `has_audio` of `stream.start`.
    pub fn has_audio(&self) -> bool {
        self.has_audio
    }

    /// Actual scope of the opened capture, for the local on-air line. `None`
    /// when the attempt has no system audio.
    pub fn audio_scope(&self) -> Option<String> {
        self.audio_scope.clone()
    }

    fn spawn(
        request: PublishRequest,
        servers: Vec<IceServer>,
        policy: String,
        producer: Box<dyn FrameProducer>,
        audio: Option<Box<dyn AudioProducer>>,
        audio_scope: Option<String>,
        preview: Arc<PreviewStore>,
        video_keepalive: bool,
        gpu: Option<Box<dyn GpuEncoder>>,
    ) -> Result<Self, String> {
        if request.encoder.is_gpu() && gpu.is_none() {
            return Err("Аппаратный кодировщик этой попытки не открыт.".into());
        }
        let has_audio = audio.is_some();
        let (commands, receiver) = mpsc::channel(ENGINE_COMMAND_CAPACITY);
        let events = Arc::new(EventQueue::default());
        let counters = Arc::new(FrameCounters::default());
        let audio_counters = Arc::new(AudioCounters::default());
        let limits = Arc::new(AppliedLimits::default());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let worker_events = events.clone();
        let worker_preview = preview.clone();
        let worker_counters = counters.clone();
        let worker_audio_counters = audio_counters.clone();
        let worker_limits = limits.clone();
        let thread = std::thread::Builder::new()
            .name("voxhold-screen-publisher".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    let _ = ready_tx.send(Err("Не удалось запустить поток трансляции.".into()));
                    return;
                };
                runtime.block_on(run(
                    receiver,
                    worker_events,
                    worker_preview,
                    worker_counters,
                    worker_audio_counters,
                    worker_limits,
                    request,
                    servers,
                    policy,
                    producer,
                    audio,
                    video_keepalive,
                    gpu,
                    ready_tx,
                ));
            })
            .map_err(|_| "Не удалось запустить поток трансляции.".to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                commands: Some(commands),
                events,
                preview,
                ice_dropped: AtomicU64::new(0),
                limits,
                has_audio,
                audio_scope: if has_audio { audio_scope } else { None },
                thread: Some(thread),
            }),
            Ok(Err(message)) => {
                join_bounded(thread, WORKER_JOIN_TIMEOUT);
                Err(message)
            }
            Err(_) => {
                let _ = commands.try_send(Command::Stop);
                drop(commands);
                join_bounded(thread, WORKER_JOIN_TIMEOUT);
                Err("Трансляция не запустилась за 5 секунд.".into())
            }
        }
    }

    /// Apply the server offer and answer it. Never blocks the caller.
    pub fn offer(&self, sdp: String) -> Result<(), String> {
        self.commands
            .as_ref()
            .ok_or_else(|| "Медиадвижок трансляции уже закрыт.".to_string())?
            .try_send(Command::Offer(sdp))
            .map_err(|_| "Медиадвижок трансляции не принимает команды.".to_string())
    }

    /// Change the video and published-audio bitrate of the live sender.
    ///
    /// No offer, answer or `stream.start` is involved: the worker applies the
    /// same `RtpSender::set_parameters` + `PeerConnection::set_bitrate` path
    /// that the initial answer used. `attempt` and `op` are only tags: the
    /// verified result comes back as an engine event with the same pair, and
    /// the caller ignores it once a newer attempt or operation started. A
    /// `Full` command queue is a burst, not a failed worker: the caller retries
    /// the same operation instead of ending the attempt.
    pub fn set_bitrate(&self, attempt: u64, op: QualityOp, quality: Quality) -> BitrateSend {
        send_bitrate(self.commands.as_ref(), attempt, op, quality)
    }

    /// The limits the worker really handed to the negotiated senders.
    #[cfg(test)]
    pub(crate) fn applied_bitrate_limits(&self) -> (u64, u64) {
        self.limits.pair()
    }

    /// Test hook: shorten the codec-confirmation window of the worker.
    #[cfg(test)]
    pub(crate) fn set_codec_confirm_timeout(&self, timeout: Duration) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|commands| commands.try_send(Command::ConfirmTimeout(timeout)).is_ok())
    }

    /// Apply one server ICE candidate. A full queue drops the candidate; the
    /// server may hold up to 64 pending ones and a multi-homed/TURN burst must
    /// not turn into a terminal publication error. The loss is counted.
    pub fn ice(&self, candidate: Candidate) -> bool {
        let Some(commands) = self.commands.as_ref() else {
            return false;
        };
        match commands.try_send(Command::Ice(candidate)) {
            Ok(()) => true,
            Err(_) => {
                self.ice_dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Candidates dropped by a full command queue since the engine started.
    pub fn ice_dropped(&self) -> u64 {
        self.ice_dropped.load(Ordering::Relaxed)
    }

    pub fn poll(&self) -> Option<EngineEvent> {
        self.events.pop()
    }
}

impl Drop for PublisherEngine {
    fn drop(&mut self) {
        // Close the channel before joining. A full queue may have swallowed
        // the Stop command, but a closed channel always ends `recv()`.
        drop(self.commands.take());
        if let Some(thread) = self.thread.take() {
            join_bounded(thread, WORKER_JOIN_TIMEOUT);
        }
        self.preview.clear();
    }
}

/// Join a worker without an unbounded wait. The detached thread keeps only its
/// own capture and peer; the publication handle is already gone, so a stuck
/// driver cannot hold the session or the UI forever.
fn join_bounded(thread: std::thread::JoinHandle<()>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = thread.join();
}

/// Hand one live bitrate change to the bounded worker queue. Only a closed
/// channel proves that the worker is gone; a full queue is a burst that must be
/// retried with the newest quality.
fn send_bitrate(
    commands: Option<&mpsc::Sender<Command>>,
    attempt: u64,
    op: QualityOp,
    quality: Quality,
) -> BitrateSend {
    let Some(commands) = commands else {
        return BitrateSend::Closed;
    };
    match commands.try_send(Command::Bitrate {
        attempt,
        op,
        quality,
    }) {
        Ok(()) => BitrateSend::Sent,
        Err(mpsc::error::TrySendError::Full(_)) => BitrateSend::Full,
        Err(mpsc::error::TrySendError::Closed(_)) => BitrateSend::Closed,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    mut commands: mpsc::Receiver<Command>,
    events: Arc<EventQueue>,
    preview: Arc<PreviewStore>,
    counters: Arc<FrameCounters>,
    audio_counters: Arc<AudioCounters>,
    limits: Arc<AppliedLimits>,
    request: PublishRequest,
    servers: Vec<IceServer>,
    policy: String,
    producer: Box<dyn FrameProducer>,
    audio: Option<Box<dyn AudioProducer>>,
    video_keepalive: bool,
    mut gpu: Option<Box<dyn GpuEncoder>>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let factory = PeerConnectionFactory::default();
    let profile = if request.encoder.is_gpu() {
        match request.encoder.profile {
            Some(profile) => profile,
            None => {
                let _ = ready.send(Err("Выбранный аппаратный профиль не подтверждён.".into()));
                return;
            }
        }
    } else {
        match negotiation::sender_profile(request.codec) {
            Ok(profile) => profile,
            Err(message) => {
                let _ = ready.send(Err(message));
                return;
            }
        }
    };
    let (width, height) = request.capture_size();
    // The production source injects a black-frame keepalive before the first
    // real capture, so a stalled desktop still sends something and the capture
    // watchdog decides. The stalled test source must not use it: without a
    // keepalive the encoder stays silent and the codec watchdog is the only
    // thing that can end the attempt. A pre-encoded passthrough never uses the
    // raw keepalive: a raw frame would start a second encoder inside WebRTC.
    let source = if gpu.is_some() || !video_keepalive {
        NativeVideoSource::new_encoded(VideoResolution { width, height })
    } else {
        NativeVideoSource::new(VideoResolution { width, height }, true)
    };
    let mut config = RtcConfiguration::default();
    config.ice_transport_type = match policy.as_str() {
        "" | "all" => IceTransportsType::All,
        "relay" => IceTransportsType::Relay,
        _ => {
            let _ = ready.send(Err("Сервер прислал неизвестную политику ICE.".into()));
            return;
        }
    };
    config.ice_servers = servers
        .into_iter()
        .map(|server| RtcIceServer {
            urls: server.urls,
            username: server.username,
            password: server.credential,
        })
        .collect();
    let peer = match factory.create_peer_connection(config) {
        Ok(peer) => peer,
        Err(_) => {
            let _ = ready.send(Err("Не удалось создать соединение трансляции.".into()));
            return;
        }
    };
    let ice_events = events.clone();
    peer.on_ice_candidate(Some(Box::new(move |ice| {
        let candidate = ice.to_string();
        if candidate.is_empty() {
            return;
        }
        ice_events.push(EngineEvent::Ice(Candidate {
            username_fragment: ufrag(&candidate).map(str::to_owned),
            candidate,
            sdp_mid: Some(ice.sdp_mid()).filter(|mid| !mid.is_empty()),
            sdp_mline_index: u16::try_from(ice.sdp_mline_index()).ok(),
        }));
    })));
    let state_events = events.clone();
    // ICE reaching `Connected` is not a confirmation of the transmission: the
    // worker emits `Connection::Connected` from its own tick only after the
    // first encoded frame reported the negotiated codec. A callback-triggered
    // event here would let an ICE-only link look live.
    peer.on_connection_state_change(Some(Box::new(move |state| match state {
        PeerConnectionState::Failed | PeerConnectionState::Closed => {
            state_events.push(EngineEvent::Closed);
        }
        _ => {}
    })));
    // The send track exists before any remote description so the answer puts
    // it on the offered m-line. The pinned build ignores a pre-added
    // `add_transceiver(SendOnly)` when it applies an offer (the answer is
    // `a=inactive`); `add_track` creates the local sendrecv transceiver that
    // associates with the offered recvonly m-line, and the answer is
    // `a=sendonly` with the offered mid. The loopback test pins this.
    let track = factory.create_video_track("voxhold-screen", source.clone());
    let sender = match peer.add_track(MediaStreamTrack::Video(track), &["voxhold-screen"]) {
        Ok(sender) => sender,
        Err(_) => {
            let _ = ready.send(Err("Не удалось добавить видеодорожку трансляции.".into()));
            return;
        }
    };
    let gpu_backend = gpu.is_some();
    // The GPU attempt sends access units that were already encoded by the
    // hardware session: the sender must use the pass-through encoder, never a
    // software encoder of the pinned build. The audio track is added after the
    // video track, so the local transceivers mirror the backend's m-line order:
    // video, then audio. EC, NS and AGC are disabled: system audio is a program
    // feed, not a mic.
    let (audio_sender, audio_pump) = match audio {
        Some(producer) => {
            let audio_source = NativeAudioSource::new(
                AudioSourceOptions {
                    echo_cancellation: false,
                    noise_suppression: false,
                    auto_gain_control: false,
                },
                SAMPLE_RATE,
                CHANNELS,
                audio::SOURCE_QUEUE_MS,
            );
            let track = factory.create_audio_track("voxhold-system-audio", audio_source.clone());
            let sender = match peer.add_track(MediaStreamTrack::Audio(track), &["voxhold-screen"]) {
                Ok(sender) => sender,
                Err(_) => {
                    let _ = ready.send(Err("Не удалось добавить аудиодорожку трансляции.".into()));
                    return;
                }
            };
            let pump = tokio::spawn(audio_loop(
                producer,
                audio_source,
                events.clone(),
                audio_counters.clone(),
            ));
            (Some(sender), Some(pump))
        }
        None => (None, None),
    };
    // Pin the requested SDR payload on the video transceiver that already
    // carries the send track. The local preference keeps the answer on the
    // requested codec when the SFU offers more than one compatible payload
    // (for example H.264 Constrained Baseline next to legacy), while the plain
    // `video/rtx` entry lets the answer derive the real `apt` pair from the
    // offer instead of assuming a payload type.
    let capabilities = factory.get_rtp_sender_capabilities(MediaType::Video);
    if let Err(message) =
        negotiation::pin_video_codec(&peer, &capabilities.codecs, request.codec, profile)
    {
        let _ = ready.send(Err(message));
        return;
    }
    if gpu_backend {
        // Install the pass-through selector on the send track's sender and on
        // every transceiver sender that carries it: the pinned build may hand
        // the engine a wrapper whose encoder is owned by the transceiver.
        sender.set_video_encoder_backend(VideoEncoderBackend::PreEncoded);
        for transceiver in peer.transceivers() {
            if matches!(
                transceiver.sender().track(),
                Some(MediaStreamTrack::Video(_))
            ) {
                transceiver
                    .sender()
                    .set_video_encoder_backend(VideoEncoderBackend::PreEncoded);
            }
        }
    }
    // The open session names its adapter; the publisher compares this
    // identity with the attempt's plan before anything may look «GPU». The
    // evidence travels with it: an attribute read from the MFT is a fact, an
    // echoed plan is only the request and is labelled as such.
    if let Some((adapter, evidence)) = gpu.as_ref().and_then(|encoder| {
        encoder
            .adapter()
            .map(|adapter| (adapter, encoder.adapter_evidence()))
    }) {
        events.push(EngineEvent::EncoderOpened(HardwareEncoderIdentity {
            luid: adapter.luid,
            adapter: adapter.name,
            driver_version: adapter.driver_version,
            codec: request.codec,
            generation: request.encoder.generation,
            adapter_evidence: evidence,
        }));
    }
    // The pinned CPU build can encode this codec; that is the honest answer the
    // failure paths use to offer «Продолжить на процессоре». The codec never
    // changes and nothing is substituted automatically.
    let cpu_available = negotiation::sender_profile(request.codec).is_ok();
    let pump = match gpu.take() {
        Some(encoder) => tokio::spawn(gpu_frame_loop(
            producer,
            source,
            encoder,
            events.clone(),
            preview,
            counters.clone(),
            request.quality.fps,
            (width, height),
            cpu_available,
            request.codec,
        )),
        None => tokio::spawn(frame_loop(
            producer,
            source,
            events.clone(),
            preview,
            counters.clone(),
            request.quality.fps,
            (width, height),
        )),
    };
    // The last quality really applied to the live senders. The backend's
    // `startRecovery` may offer again to a live publisher, so a later answer
    // must use this value instead of the spawn-time request: otherwise a live
    // bitrate change is silently reverted by the re-offer. A refused change
    // never moves it.
    let mut quality = request.quality;
    // The limits the senders really reported. Only a verified application or
    // a successful answer updates them; they are the rollback target of the
    // next live change.
    let mut applied = Limits::default();
    let _ = ready.send(Ok(()));

    let mut remote_set = false;
    let mut pending: VecDeque<Candidate> = VecDeque::new();
    let mut disconnected_since: Option<Instant> = None;
    let mut last_stats = Instant::now();
    let mut audio_meter = AudioMeter::default();
    // What the delivered SDP negotiated, and what the encoder really produced.
    let mut selected: Option<negotiation::SelectedVideo> = None;
    let mut confirmed: Option<negotiation::ConfirmedCodec> = None;
    let mut confirm_deadline: Option<Instant> = None;
    #[cfg_attr(not(test), allow(unused_mut))]
    let mut confirm_timeout = CODEC_CONFIRM_TIMEOUT;
    let mut connected_announced = false;
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                if peer.connection_state() == PeerConnectionState::Disconnected {
                    let since = disconnected_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= DISCONNECT_TIMEOUT {
                        events.push(EngineEvent::Error(
                            "Соединение трансляции потеряно.".into(),
                        ));
                        break;
                    }
                } else {
                    disconnected_since = None;
                }
                // Transmission is confirmed only by encoded frames and their
                // reported codec. The check runs after the statistics poll of
                // this tick, so the first sample that confirms the codec also
                // announces the connection; ICE alone never does.
                if confirmed.is_none()
                    && confirm_deadline.is_some_and(|deadline| Instant::now() >= deadline)
                {
                    let message = format!(
                        "Кодек {} не подтверждён: за {} с не пришло ни одного закодированного кадра.",
                        request.codec.label(),
                        confirm_timeout.as_secs().max(1)
                    );
                    // A GPU attempt that never confirmed the codec is a failed
                    // hardware session, not an ordinary codec error: report it
                    // as such so the publisher can offer the explicit
                    // «Продолжить на процессоре» when the same codec exists on
                    // the CPU build. Nothing switches by itself.
                    if request.encoder.is_gpu() && cpu_available {
                        events.push(EngineEvent::EncoderFailed {
                            message,
                            cpu_available: true,
                        });
                    } else {
                        events.push(EngineEvent::Error(message));
                    }
                    break;
                }
                if last_stats.elapsed() >= STATS_INTERVAL {
                    last_stats = Instant::now();
                    if let Some(mut sample) = stats::sample(
                        &peer,
                        &counters,
                        &audio_counters,
                        &mut audio_meter,
                        audio_sender.is_some(),
                        selected.as_ref().map(|selected| selected.mid.as_str()),
                    )
                    .await
                    {
                        if confirmed.is_none()
                            && let (Some(selected), Some(wire)) =
                                (selected.as_ref(), sample.video.as_ref())
                        {
                            match negotiation::confirm_codec(selected, wire) {
                                Ok(codec) => confirmed = codec,
                                Err(message) => {
                                    events.push(EngineEvent::Error(message));
                                    break;
                                }
                            }
                        }
                        sample.stats.confirmed_codec = confirmed.as_ref().map(|codec| codec.codec);
                        events.push(EngineEvent::Stats(sample.stats));
                    }
                }
                if confirmed.is_some()
                    && !connected_announced
                    && peer.connection_state() == PeerConnectionState::Connected
                {
                    connected_announced = true;
                    events.push(EngineEvent::State(
                        crate::publisher::Connection::Connected,
                    ));
                }
                continue;
            }
        };
        match command {
            Some(Command::Offer(sdp)) => {
                // The context borrows the previous negotiation, so it ends
                // before the new one replaces it.
                let outcome = {
                    let context = negotiation::OfferContext {
                        codec: request.codec,
                        profile,
                        audio_active: audio_sender.is_some(),
                        previous: selected.as_ref(),
                        quality,
                    };
                    negotiation::apply_offer(
                        &peer,
                        &sender,
                        audio_sender.as_ref(),
                        &sdp,
                        &context,
                        Some(&limits),
                        &mut applied,
                    )
                    .await
                };
                match outcome {
                    Ok((answer, next)) => {
                        if selected.is_none() {
                            confirm_deadline = Some(Instant::now() + confirm_timeout);
                        }
                        selected = Some(next);
                        remote_set = true;
                        events.push(EngineEvent::Answer(answer));
                        while let Some(candidate) = pending.pop_front() {
                            add_ice(&peer, candidate).await;
                        }
                    }
                    Err(message) => {
                        events.push(EngineEvent::Error(message));
                        break;
                    }
                }
            }
            #[cfg(test)]
            Some(Command::ConfirmTimeout(timeout)) => {
                confirm_timeout = timeout;
                if confirmed.is_none()
                    && let Some(deadline) = confirm_deadline
                {
                    confirm_deadline = Some(deadline.min(Instant::now() + confirm_timeout));
                }
            }
            Some(Command::Bitrate {
                attempt,
                op,
                quality: next,
            }) => {
                let result =
                    match apply_sender_limits(&peer, &sender, audio_sender.as_ref(), next, applied)
                    {
                        Ok(confirmed) => {
                            applied = confirmed;
                            limits.store(&confirmed);
                            quality = confirmed_quality(quality, next, true);
                            BitrateResult::Applied
                        }
                        Err(failure) => {
                            // The readback after a failed restore is the honest
                            // state of the senders. `AppliedLimits` reflects it so
                            // a test or a later diagnostic never sees the refused
                            // request as applied.
                            let actual = readback_limits(&sender, audio_sender.as_ref());
                            if let Some(video) = actual.video_bps {
                                applied.video_bps = Some(video);
                            }
                            if let Some(audio) = actual.audio_bps {
                                applied.audio_bps = Some(audio);
                            }
                            limits.store(&applied);
                            quality = confirmed_quality(quality, next, false);
                            let restored = failure.restored;
                            let message = failure_message(failure, actual, audio_sender.is_some());
                            BitrateResult::Failed { message, restored }
                        }
                    };
                events.push(EngineEvent::Bitrate {
                    attempt,
                    op,
                    result,
                });
            }
            Some(Command::Ice(candidate)) => {
                if remote_set {
                    add_ice(&peer, candidate).await;
                } else if pending.len() < ICE_CAPACITY {
                    pending.push_back(candidate);
                }
            }
            Some(Command::Stop) | None => break,
        }
    }
    pump.abort();
    let _ = pump.await;
    if let Some(pump) = audio_pump {
        pump.abort();
        let _ = pump.await;
    }
    peer.on_connection_state_change(None);
    peer.on_ice_candidate(None);
    peer.close();
}

async fn add_ice(peer: &PeerConnection, candidate: Candidate) {
    let mid = candidate.sdp_mid.as_deref().unwrap_or_default();
    let index = candidate.sdp_mline_index.map(i32::from).unwrap_or(-1);
    if let Ok(parsed) = IceCandidate::parse(mid, index, &candidate.candidate) {
        let _ = peer.add_ice_candidate(parsed).await;
    }
}

fn ufrag(candidate: &str) -> Option<&str> {
    let mut tokens = candidate.split_ascii_whitespace().skip(8);
    while let (Some(key), Some(value)) = (tokens.next(), tokens.next()) {
        if key == "ufrag" {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod loopback;

#[cfg(test)]
mod bitrate_tests;

#[cfg(test)]
mod audio_tests;

#[cfg(test)]
mod capture_tests;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod codec_tests;

#[cfg(test)]
mod codec_reject_tests;

#[cfg(test)]
mod gpu_tests;
