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
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use libwebrtc::{
    ice_candidate::IceCandidate,
    media_stream_track::MediaStreamTrack,
    peer_connection::{AnswerOptions, BitrateSettings, PeerConnection, PeerConnectionState},
    peer_connection_factory::{
        IceServer as RtcIceServer, IceTransportsType, PeerConnectionFactory, RtcConfiguration,
        native::PeerConnectionFactoryExt,
    },
    rtp_sender::{RtpSender, VideoEncoderBackend},
    session_description::{SdpType, SessionDescription},
    video_codec::VideoCodec,
    video_frame::{BoxVideoBuffer, I420Buffer, VideoFrame, VideoRotation},
    video_source::{VideoResolution, native::NativeVideoSource},
};
use tokio::sync::mpsc;

use crate::{
    Candidate, IceServer,
    publisher::{PublishRequest, Quality},
    screen_capture::{
        CaptureFailure, CapturedImage, LatestImage, PreviewFrame, PreviewStore, ScreenCapture,
        preview_from_i420,
    },
};

/// How long the capture may deliver nothing before the attempt is failed.
/// Covers a minimized window, a closed window and an unplugged monitor that
/// the capturer reports late or not at all.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// A disconnection that does not recover is treated as a dead SFU session.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Bounded pending ICE while no remote description exists (server contract).
const ICE_CAPACITY: usize = 64;
const ENGINE_EVENT_CAPACITY: usize = 32;

/// Measured counters of the publishing path. There is no CPU or GPU probe in
/// this build, so none is reported.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PublishStats {
    /// Frames accepted by the video source in the last measured second.
    pub fps: f32,
    /// Captured frames replaced before the encoder read them.
    pub dropped: u64,
    pub width: u32,
    pub height: u32,
}

impl Default for PublishStats {
    fn default() -> Self {
        Self {
            fps: 0.0,
            dropped: 0,
            width: 0,
            height: 0,
        }
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

#[derive(Default)]
struct EventQueue {
    items: Mutex<VecDeque<EngineEvent>>,
}

impl EventQueue {
    fn push(&self, event: EngineEvent) {
        let Ok(mut items) = self.items.lock() else {
            return;
        };
        if matches!(event, EngineEvent::Stats(_)) {
            // Only the latest counters matter.
            items.retain(|item| !matches!(item, EngineEvent::Stats(_)));
        }
        if items.len() >= ENGINE_EVENT_CAPACITY {
            items.pop_front();
        }
        items.push_back(event);
    }

    fn pop(&self) -> Option<EngineEvent> {
        self.items.lock().ok()?.pop_front()
    }
}

enum Command {
    Offer(String),
    Ice(Candidate),
    Stop,
}

/// Non-blocking handle of one publishing engine. Dropping it releases the
/// capture and closes the peer.
pub struct PublisherEngine {
    commands: mpsc::Sender<Command>,
    events: Arc<EventQueue>,
    preview: Arc<PreviewStore>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PublisherEngine {
    pub fn start(
        request: PublishRequest,
        servers: Vec<IceServer>,
        policy: String,
        preview: Arc<PreviewStore>,
    ) -> Result<Self, String> {
        let latest = Arc::new(LatestImage::default());
        let target = request.quality.resolution.size();
        let capture = ScreenCapture::start(
            request.source.clone(),
            target,
            request.quality.fps,
            latest.clone(),
        )
        .map_err(str::to_owned)?;
        Self::spawn(
            request,
            servers,
            policy,
            Box::new(DesktopProducer { capture, latest }),
            preview,
        )
    }

    /// Test entry point: the same engine with a frame producer that does not
    /// need a desktop. `frame_size` may differ from the declared quality, like
    /// a 21:9 monitor inside a 720p publication.
    #[cfg(test)]
    pub(crate) fn start_synthetic(
        request: PublishRequest,
        servers: Vec<IceServer>,
        policy: String,
        preview: bool,
        frame_size: (u32, u32),
    ) -> Result<Self, String> {
        Self::spawn(
            request,
            servers,
            policy,
            Box::new(SyntheticProducer::new(frame_size.0, frame_size.1, preview)),
            Arc::new(PreviewStore::default()),
        )
    }

    fn spawn(
        request: PublishRequest,
        servers: Vec<IceServer>,
        policy: String,
        producer: Box<dyn FrameProducer>,
        preview: Arc<PreviewStore>,
    ) -> Result<Self, String> {
        let (commands, receiver) = mpsc::channel(16);
        let events = Arc::new(EventQueue::default());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let worker_events = events.clone();
        let worker_preview = preview.clone();
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
                    request,
                    servers,
                    policy,
                    producer,
                    ready_tx,
                ));
            })
            .map_err(|_| "Не удалось запустить поток трансляции.".to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                commands,
                events,
                preview,
                thread: Some(thread),
            }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(message)
            }
            Err(_) => {
                let _ = commands.try_send(Command::Stop);
                let _ = thread.join();
                Err("Трансляция не запустилась за 5 секунд.".into())
            }
        }
    }

    /// Apply the server offer and answer it. Never blocks the caller.
    pub fn offer(&self, sdp: String) -> Result<(), String> {
        self.commands
            .try_send(Command::Offer(sdp))
            .map_err(|_| "Медиадвижок трансляции не принимает команды.".to_string())
    }

    pub fn ice(&self, candidate: Candidate) -> Result<(), String> {
        self.commands
            .try_send(Command::Ice(candidate))
            .map_err(|_| "Медиадвижок трансляции не принимает команды.".to_string())
    }

    pub fn poll(&self) -> Option<EngineEvent> {
        self.events.pop()
    }

    /// The newest local preview picture for the on-air card.
    pub fn take_preview(&self) -> Option<PreviewFrame> {
        self.preview.take()
    }
}

impl Drop for PublisherEngine {
    fn drop(&mut self) {
        let _ = self.commands.try_send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.preview.clear();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    mut commands: mpsc::Receiver<Command>,
    events: Arc<EventQueue>,
    preview: Arc<PreviewStore>,
    request: PublishRequest,
    servers: Vec<IceServer>,
    policy: String,
    producer: Box<dyn FrameProducer>,
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
    // The codec is pinned by the backend's `SetCodecPreferences`; the answer
    // selects the first common payload. The picker offers only verified codecs.
    let _ = request.codec;

    let pump = tokio::spawn(frame_loop(
        producer,
        source,
        events.clone(),
        preview,
        request.quality,
    ));
    let _ = ready.send(Ok(()));

    let mut remote_set = false;
    let mut pending: VecDeque<Candidate> = VecDeque::new();
    let mut disconnected_since: Option<Instant> = None;
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
                continue;
            }
        };
        match command {
            Some(Command::Offer(sdp)) => {
                if !apply_offer(&peer, &sender, &sdp, request.quality.bitrate_kbps, &events).await {
                    break;
                }
                remote_set = true;
                while let Some(candidate) = pending.pop_front() {
                    add_ice(&peer, candidate).await;
                }
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
    peer.on_connection_state_change(None);
    peer.on_ice_candidate(None);
    peer.close();
}

async fn apply_offer(
    peer: &PeerConnection,
    sender: &RtpSender,
    sdp: &str,
    bitrate_kbps: u32,
    events: &EventQueue,
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
    apply_bitrate(peer, sender, bitrate_kbps);
    events.push(EngineEvent::Answer(text));
    true
}

fn apply_bitrate(peer: &PeerConnection, sender: &RtpSender, bitrate_kbps: u32) {
    let max = Some(u64::from(bitrate_kbps.clamp(500, 16_000)) * 1000);
    let mut parameters = sender.parameters();
    for encoding in &mut parameters.encodings {
        encoding.max_bitrate = max;
    }
    let _ = sender.set_parameters(parameters);
    let _ = peer.set_bitrate(BitrateSettings {
        max_bitrate_bps: max,
        ..BitrateSettings::default()
    });
}

async fn add_ice(peer: &PeerConnection, candidate: Candidate) {
    let mid = candidate.sdp_mid.as_deref().unwrap_or_default();
    let index = candidate.sdp_mline_index.map(i32::from).unwrap_or(-1);
    if let Ok(parsed) = IceCandidate::parse(mid, index, &candidate.candidate) {
        let _ = peer.add_ice_candidate(parsed).await;
    }
}

async fn frame_loop(
    mut producer: Box<dyn FrameProducer>,
    source: NativeVideoSource,
    events: Arc<EventQueue>,
    preview: Arc<PreviewStore>,
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
            if failed == Some(CaptureFailure::Temporary) {
                failed = None;
            }
        }
        if let Some(failure) = failed {
            events.push(EngineEvent::Error(failure.message().into()));
            return;
        }
        if last_frame.elapsed() >= FRAME_TIMEOUT {
            events.push(EngineEvent::Error(
                "Источник захвата перестал присылать кадры.".into(),
            ));
            return;
        }
        let elapsed = window.elapsed();
        if elapsed >= Duration::from_secs(1) {
            events.push(EngineEvent::Stats(PublishStats {
                fps: sent_in_window as f32 / elapsed.as_secs_f32(),
                dropped: producer.dropped(),
                width: size.0,
                height: size.1,
            }));
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
