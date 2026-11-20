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

use std::{
    collections::VecDeque,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use libwebrtc::{
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    ice_candidate::IceCandidate,
    media_stream_track::MediaStreamTrack,
    peer_connection::{AnswerOptions, PeerConnection, PeerConnectionState},
    peer_connection_factory::{
        IceServer as RtcIceServer, IceTransportsType, PeerConnectionFactory, RtcConfiguration,
        native::PeerConnectionFactoryExt,
    },
    rtp_sender::{RtpSender, VideoEncoderBackend},
    session_description::{SdpType, SessionDescription},
    stats::{QualityLimitationReason, RtcStats},
    video_codec::VideoCodec,
    video_frame::{BoxVideoBuffer, I420Buffer, VideoFrame, VideoRotation},
    video_source::{VideoResolution, native::NativeVideoSource},
};
use tokio::sync::mpsc;

use crate::{
    Candidate, IceServer,
    publisher::{
        BitrateResult, PublishRequest, Quality, QualityOp,
        bitrate::{
            Limits, apply_sender_limits, confirmed_quality, failure_message, readback_limits,
        },
    },
    screen_capture::{
        CaptureFailure, CapturedImage, LatestImage, PreviewStore, ScreenCapture, preview_from_i420,
    },
    system_audio::{AudioProducer, CHANNELS, SAMPLE_RATE, SystemAudio},
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

/// Whether the pinned build has a VP8 encoder. Only a confirmed codec may be
/// offered by the picker.
pub fn vp8_encoder_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let factory = PeerConnectionFactory::default();
        factory
            .video_encoder_codecs(VideoEncoderBackend::Auto)
            .contains(&VideoCodec::VP8)
    })
}

/// Where the pump takes its pictures. The production producer pulls the
/// Windows desktop capturer; the loopback test injects a synthetic pattern.
pub trait FrameProducer: Send {
    fn next_frame(&mut self) -> Option<CapturedImage>;
    fn failure(&self) -> Option<CaptureFailure>;
    fn dropped(&self) -> u64;
}

struct DesktopProducer {
    capture: ScreenCapture,
    latest: Arc<LatestImage>,
}

impl FrameProducer for DesktopProducer {
    fn next_frame(&mut self) -> Option<CapturedImage> {
        self.latest.take()
    }

    fn failure(&self) -> Option<CaptureFailure> {
        self.capture.failure()
    }

    fn dropped(&self) -> u64 {
        self.latest.dropped()
    }
}

/// A known moving I420 pattern. It exercises the real encoder and RTP path
/// without a desktop session, which keeps the loopback test CI-safe. The first
/// slice of the plan asked for exactly this synthetic source.
pub struct SyntheticProducer {
    width: u32,
    height: u32,
    frame: u64,
    preview: bool,
}

impl SyntheticProducer {
    pub fn new(width: u32, height: u32, preview: bool) -> Self {
        Self {
            width: width.max(2) & !1,
            height: height.max(2) & !1,
            frame: 0,
            preview,
        }
    }
}

impl FrameProducer for SyntheticProducer {
    fn next_frame(&mut self) -> Option<CapturedImage> {
        let mut buffer = I420Buffer::new(self.width, self.height);
        let (stride_y, stride_u, stride_v) = buffer.strides();
        let chroma = (
            buffer.chroma_width() as usize,
            buffer.chroma_height() as usize,
        );
        let (data_y, data_u, data_v) = buffer.data_mut();
        // A vertical bar moves one pixel per frame and the background changes
        // luminance every second, so a frozen or single-colour stream is
        // visible in the encoded output.
        let bar = (self.frame % u64::from(self.width)) as u32;
        let value = if (self.frame / 30) % 2 == 0 { 220 } else { 80 };
        for y in 0..self.height as usize {
            for x in 0..self.width as usize {
                let bright = x as u32 >= bar && (x as u32) < bar + self.width / 8;
                data_y[y * stride_y as usize + x] = if bright { 235 } else { value };
            }
        }
        for y in 0..chroma.1 {
            for x in 0..chroma.0 {
                data_u[y * stride_u as usize + x] = 128;
                data_v[y * stride_v as usize + x] = 128;
            }
        }
        self.frame = self.frame.wrapping_add(1);
        let preview = if self.preview {
            preview_from_i420(&buffer, 640)
        } else {
            None
        };
        Some(CapturedImage {
            width: self.width,
            height: self.height,
            i420: buffer,
            preview,
        })
    }

    fn failure(&self) -> Option<CaptureFailure> {
        None
    }

    fn dropped(&self) -> u64 {
        0
    }
}

/// An event that belongs to the current publishing attempt.
pub use crate::publisher::EngineEvent;

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
        cancelled: &AtomicBool,
    ) -> Result<Self, String> {
        if cancelled.load(Ordering::SeqCst) {
            return Err(PREP_CANCELLED.into());
        }
        let latest = Arc::new(LatestImage::default());
        let target = request.quality.resolution.size();
        let capture = ScreenCapture::start(
            request.source.clone(),
            target,
            request.quality.fps,
            latest.clone(),
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
        let engine = Self::spawn(
            request,
            servers,
            policy,
            Box::new(DesktopProducer { capture, latest }),
            audio,
            audio_scope,
            preview,
        )?;
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
    ) -> Result<Self, String> {
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
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let factory = PeerConnectionFactory::default();
    let (width, height) = request.quality.resolution.size();
    let source = NativeVideoSource::new(VideoResolution { width, height }, true);
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
    peer.on_connection_state_change(Some(Box::new(move |state| match state {
        PeerConnectionState::Connected => {
            state_events.push(EngineEvent::State(crate::publisher::Connection::Connected));
        }
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
    // The audio track is added after the video track, so the local
    // transceivers mirror the backend's m-line order: video, then audio. EC,
    // NS and AGC are disabled: system audio is a program feed, not a mic.
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
                200,
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
    // The codec is pinned by the backend's `SetCodecPreferences`; the answer
    // selects the first common payload. The picker offers only verified codecs.
    let _ = request.codec;

    let pump = tokio::spawn(frame_loop(
        producer,
        source,
        events.clone(),
        preview,
        counters.clone(),
        request.quality,
    ));
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
                if last_stats.elapsed() >= STATS_INTERVAL {
                    last_stats = Instant::now();
                    if let Some(stats) = outbound_stats(
                        &peer,
                        &counters,
                        &audio_counters,
                        &mut audio_meter,
                        audio_sender.is_some(),
                    )
                    .await
                    {
                        events.push(EngineEvent::Stats(stats));
                    }
                }
                continue;
            }
        };
        match command {
            Some(Command::Offer(sdp)) => {
                if !apply_offer(
                    &peer,
                    &sender,
                    audio_sender.as_ref(),
                    &sdp,
                    quality,
                    &events,
                    Some(&limits),
                    &mut applied,
                )
                .await
                {
                    break;
                }
                remote_set = true;
                while let Some(candidate) = pending.pop_front() {
                    add_ice(&peer, candidate).await;
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

#[allow(clippy::too_many_arguments)]
async fn apply_offer(
    peer: &PeerConnection,
    sender: &RtpSender,
    audio_sender: Option<&RtpSender>,
    sdp: &str,
    quality: Quality,
    events: &EventQueue,
    limits: Option<&AppliedLimits>,
    applied: &mut Limits,
) -> bool {
    let offer = match SessionDescription::parse(sdp, SdpType::Offer) {
        Ok(offer) => offer,
        Err(_) => {
            events.push(EngineEvent::Error(
                "Сервер прислал некорректное предложение трансляции.".into(),
            ));
            return false;
        }
    };
    if peer.set_remote_description(offer).await.is_err() {
        events.push(EngineEvent::Error(
            "Не удалось применить предложение трансляции.".into(),
        ));
        return false;
    }
    let answer = match peer.create_answer(AnswerOptions::default()).await {
        Ok(answer) => answer,
        Err(_) => {
            events.push(EngineEvent::Error(
                "Не удалось создать ответ трансляции.".into(),
            ));
            return false;
        }
    };
    let text = answer.to_string();
    if peer.set_local_description(answer).await.is_err() {
        events.push(EngineEvent::Error(
            "Не удалось применить ответ трансляции.".into(),
        ));
        return false;
    }
    apply_confirmed_limits(peer, sender, audio_sender, quality, limits, applied);
    events.push(EngineEvent::Answer(text));
    true
}

/// The initial answer applies the same limits, but its result is not a live
/// operation: a refused readback leaves the confirmed limits unknown instead
/// of failing the publication.
fn apply_confirmed_limits(
    peer: &PeerConnection,
    sender: &RtpSender,
    audio_sender: Option<&RtpSender>,
    quality: Quality,
    limits: Option<&AppliedLimits>,
    applied: &mut Limits,
) {
    if let Ok(confirmed) = apply_sender_limits(peer, sender, audio_sender, quality, *applied) {
        *applied = confirmed;
        if let Some(limits) = limits {
            limits.store(&confirmed);
        }
    }
}

async fn add_ice(peer: &PeerConnection, candidate: Candidate) {
    let mid = candidate.sdp_mid.as_deref().unwrap_or_default();
    let index = candidate.sdp_mline_index.map(i32::from).unwrap_or(-1);
    if let Ok(parsed) = IceCandidate::parse(mid, index, &candidate.candidate) {
        let _ = peer.add_ice_candidate(parsed).await;
    }
}

/// One honest diagnostics sample: encoded FPS / resolution / limitation from
/// `OutboundRtpStats`, the capture drop counter and the single active source.
/// A failed or empty stats call falls back to the capture-side numbers.
async fn outbound_stats(
    peer: &PeerConnection,
    counters: &FrameCounters,
    audio_counters: &AudioCounters,
    audio_meter: &mut AudioMeter,
    audio_active: bool,
) -> Option<PublishStats> {
    let capture_width = counters.width.load(Ordering::Relaxed);
    let capture_height = counters.height.load(Ordering::Relaxed);
    let capture_fps = f32::from_bits(counters.fps_bits.load(Ordering::Relaxed));
    let dropped = counters.dropped.load(Ordering::Relaxed);
    let (mut video, mut audio) = (None, None);
    if let Ok(Ok(stats)) = tokio::time::timeout(STATS_TIMEOUT, peer.get_stats()).await {
        for stat in stats {
            let RtcStats::OutboundRtp(outbound) = stat else {
                continue;
            };
            match outbound.stream.kind.as_str() {
                "video" => video = Some(outbound),
                "audio" => audio = Some(outbound),
                _ => {}
            }
        }
    }
    let mut audio_packets_sent = 0;
    let mut audio_kbps = 0.0;
    if let Some(outbound) = &audio {
        audio_packets_sent = outbound.sent.packets_sent;
        audio_meter.observe(outbound.sent.bytes_sent);
        audio_kbps = audio_meter.kbps();
    }
    let Some(outbound) = video else {
        return Some(PublishStats {
            fps: capture_fps,
            dropped,
            width: capture_width,
            height: capture_height,
            sources: 1,
            limitation: QualityLimitation::None,
            audio_active,
            audio_kbps,
            audio_packets_sent,
            audio_dropped: audio_counters.dropped(),
        });
    };
    let stream = outbound.outbound;
    #[cfg(test)]
    crate::live_probe::record_outbound(
        stream.frames_encoded,
        outbound.sent.packets_sent,
        stream.frame_width,
        stream.frame_height,
        audio_packets_sent,
        audio_kbps,
    );
    Some(PublishStats {
        fps: stream.frames_per_second as f32,
        dropped,
        width: if stream.frame_width > 0 {
            stream.frame_width
        } else {
            capture_width
        },
        height: if stream.frame_height > 0 {
            stream.frame_height
        } else {
            capture_height
        },
        sources: 1,
        limitation: QualityLimitation::from_reason(stream.quality_limitation_reason),
        audio_active,
        audio_kbps,
        audio_packets_sent,
        audio_dropped: audio_counters.dropped(),
    })
}

async fn frame_loop(
    mut producer: Box<dyn FrameProducer>,
    source: NativeVideoSource,
    events: Arc<EventQueue>,
    preview: Arc<PreviewStore>,
    counters: Arc<FrameCounters>,
    quality: Quality,
) {
    let interval = Duration::from_micros(1_000_000 / u64::from(quality.fps.max(1)));
    let mut window = Instant::now();
    let mut sent_in_window = 0_u32;
    let mut first = false;
    let mut last_frame = Instant::now();
    let mut size = quality.resolution.size();
    loop {
        tokio::time::sleep(interval).await;
        let mut failed = producer.failure();
        if let Some(image) = producer.next_frame() {
            last_frame = Instant::now();
            size = (image.width, image.height);
            let frame = VideoFrame::new(
                VideoRotation::VideoRotation0,
                Box::new(image.i420) as BoxVideoBuffer,
            );
            if source.capture_frame(&frame) {
                sent_in_window += 1;
                if !first {
                    first = true;
                    events.push(EngineEvent::State(crate::publisher::Connection::FirstFrame));
                }
            }
            if let Some(picture) = image.preview {
                preview.publish(picture);
            }
            // A good frame proves that a temporary compositor failure did not
            // end the capture. Only a permanent failure is terminal below.
            failed = None;
        }
        if let Some(failure) = failed {
            match failure {
                // A temporary failure waits for the frame watchdog: the next
                // good frame recovers the publication, a dead source is
                // caught by `FRAME_TIMEOUT` without an immediate false stop.
                CaptureFailure::Temporary => {}
                CaptureFailure::Permanent | CaptureFailure::Ended => {
                    events.push(EngineEvent::Error(failure.message().into()));
                    return;
                }
            }
        }
        if last_frame.elapsed() >= FRAME_TIMEOUT {
            events.push(EngineEvent::Error(
                "Источник захвата перестал присылать кадры.".into(),
            ));
            return;
        }
        let elapsed = window.elapsed();
        if elapsed >= Duration::from_secs(1) {
            counters
                .dropped
                .store(producer.dropped(), Ordering::Relaxed);
            counters.fps_bits.store(
                (sent_in_window as f32 / elapsed.as_secs_f32()).to_bits(),
                Ordering::Relaxed,
            );
            counters.width.store(size.0, Ordering::Relaxed);
            counters.height.store(size.1, Ordering::Relaxed);
            sent_in_window = 0;
            window = Instant::now();
        }
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
