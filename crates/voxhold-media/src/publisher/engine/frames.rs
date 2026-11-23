//! The frame pump of one publication: raw I420 on the CPU path and pre-encoded
//! access units on the hardware path.
//!
//! Both loops own only the capture producer, the source and the counters; the
//! worker owns the peer. The hardware loop is bounded by the encoder queue and
//! forwards PLI/FIR and rate control back to the session.

use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use libwebrtc::{
    video_frame::{
        BoxVideoBuffer, EncodedFrameType, EncodedVideoCodec, EncodedVideoFrame, VideoFrame,
        VideoRotation,
    },
    video_source::{EncodedRateControl, VideoResolution, native::NativeVideoSource},
};

use super::{EngineEvent, EventQueue, FRAME_TIMEOUT, FrameCounters, FrameProducer};
use crate::publisher::Codec;
use crate::screen_capture::{CaptureFailure, PreviewStore};
use crate::video_encoder::{EncodeSubmit, GpuEncoder, Nv12Frame};

/// What the hardware frame pump reads from the pass-through source.
///
/// The real value is [`NativeVideoSource`]; the trait exists so the same pump
/// (including the PLI/FIR and rate-control forwarding) runs against a
/// deterministic source in tests instead of a copy of the loop.
pub(super) trait EncodedSource {
    /// Hand one encoded access unit to the sender. `false` means it was
    /// dropped and a keyframe is needed to repair the chain.
    fn capture_encoded_frame(&self, frame: &EncodedVideoFrame<'_>) -> bool;
    /// A PLI/FIR (or an internal reconfiguration) asked for a keyframe.
    fn take_keyframe_request(&self) -> bool;
    /// The negotiated sender asked for a new rate-control target.
    fn take_rate_control_request(&self) -> Option<EncodedRateControl>;
}

impl EncodedSource for NativeVideoSource {
    fn capture_encoded_frame(&self, frame: &EncodedVideoFrame<'_>) -> bool {
        NativeVideoSource::capture_encoded_frame(self, frame)
    }

    fn take_keyframe_request(&self) -> bool {
        NativeVideoSource::take_keyframe_request(self)
    }

    fn take_rate_control_request(&self) -> Option<EncodedRateControl> {
        NativeVideoSource::take_rate_control_request(self)
    }
}

pub(super) async fn frame_loop(
    mut producer: Box<dyn FrameProducer>,
    source: NativeVideoSource,
    events: Arc<EventQueue>,
    preview: Arc<PreviewStore>,
    counters: Arc<FrameCounters>,
    fps: u32,
    mut size: (u32, u32),
) {
    let interval = Duration::from_secs(1) / fps.clamp(1, crate::publisher::MAX_SCREEN_FPS);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut window = Instant::now();
    let mut sent_in_window = 0_u32;
    let mut first = false;
    let mut last_frame = Instant::now();
    loop {
        tick.tick().await;
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

/// The hardware path of the frame pump.
///
/// Captured I420 frames are converted to NV12 and submitted to the bounded
/// encoder queue; the encoded access units come back from the session and are
/// handed to the pass-through encoder as they are. The queue drops happen
/// before the encoder, so an input drop is not a lost reference frame; a lost
/// encoded packet is repaired by the session with a keyframe. PLI/FIR and the
/// driver's bitrate target travel from the WebRTC source back to the encoder.
#[allow(clippy::too_many_arguments)]
pub(super) async fn gpu_frame_loop<S>(
    mut producer: Box<dyn FrameProducer>,
    source: S,
    mut encoder: Box<dyn GpuEncoder>,
    events: Arc<EventQueue>,
    preview: Arc<PreviewStore>,
    counters: Arc<FrameCounters>,
    fps: u32,
    mut size: (u32, u32),
    cpu_available: bool,
    codec: Codec,
) where
    S: EncodedSource + Send + 'static,
{
    let interval = Duration::from_secs(1) / fps.clamp(1, crate::publisher::MAX_SCREEN_FPS);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut window = Instant::now();
    let mut sent_in_window = 0_u32;
    let mut first = false;
    let mut last_timestamp = 0_i64;
    let mut last_capture = Instant::now();
    let mut last_delivered = Instant::now();
    let mut delivered = 0_u64;
    loop {
        tick.tick().await;
        let mut failed = producer.failure();
        if let Some(image) = producer.next_frame() {
            last_capture = Instant::now();
            size = (image.width, image.height);
            let timestamp = wall_micros().max(last_timestamp + 1);
            last_timestamp = timestamp;
            let submit = encoder.submit(Nv12Frame::from_captured(&image, timestamp));
            if submit != EncodeSubmit::Closed {
                failed = None;
            }
            if let Some(picture) = image.preview {
                preview.publish(picture);
            }
        }
        // The pass-through encoder raises PLI/FIR as a keyframe request and
        // the negotiated sender's target as rate control. Both go back to the
        // driver; neither is a local decision of this loop.
        if source.take_keyframe_request() {
            encoder.request_keyframe();
        }
        if let Some(control) = source.take_rate_control_request() {
            let target = u32::try_from(control.target_bitrate_bps).unwrap_or(u32::MAX);
            encoder.set_bitrate(target);
        }
        for _ in 0..4 {
            let Some(packet) = encoder.next_packet() else {
                break;
            };
            let frame = EncodedVideoFrame {
                codec: encoded_codec(codec),
                payload: &packet.payload,
                timestamp_us: packet.timestamp_us,
                frame_type: if packet.keyframe {
                    EncodedFrameType::Key
                } else {
                    EncodedFrameType::Delta
                },
                resolution: VideoResolution {
                    width: size.0,
                    height: size.1,
                },
                frame_metadata: None,
            };
            if source.capture_encoded_frame(&frame) {
                delivered += 1;
                sent_in_window += 1;
                last_delivered = Instant::now();
                if !first {
                    first = true;
                    events.push(EngineEvent::State(crate::publisher::Connection::FirstFrame));
                }
            } else {
                // The adapter dropped the access unit after it was encoded.
                encoder.request_keyframe();
            }
        }
        if let Some(failure) = encoder.failure() {
            events.push(EngineEvent::EncoderFailed {
                message: failure.message,
                cpu_available,
            });
            return;
        }
        if let Some(failure) = failed {
            match failure {
                CaptureFailure::Temporary => {}
                CaptureFailure::Permanent | CaptureFailure::Ended => {
                    events.push(EngineEvent::Error(failure.message().into()));
                    return;
                }
            }
        }
        if last_capture.elapsed() >= FRAME_TIMEOUT {
            events.push(EngineEvent::Error(
                "Источник захвата перестал присылать кадры.".into(),
            ));
            return;
        }
        if delivered > 0 && last_delivered.elapsed() >= FRAME_TIMEOUT {
            events.push(EngineEvent::EncoderFailed {
                message: "Аппаратный кодировщик перестал отдавать закодированные кадры.".into(),
                cpu_available,
            });
            return;
        }
        let elapsed = window.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let stats = encoder.counters();
            counters.dropped.store(
                producer.dropped() + stats.dropped_input + stats.dropped_output,
                Ordering::Relaxed,
            );
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

/// Wall-clock capture time in microseconds. Pre-encoded frames must carry the
/// same base as the raw path (`SystemTime`), otherwise the sender's timestamp
/// aligner sees frames decades old and drops them.
fn wall_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros() as i64)
        .unwrap_or(0)
}

fn encoded_codec(codec: Codec) -> EncodedVideoCodec {
    match codec {
        Codec::H264 => EncodedVideoCodec::H264,
        Codec::Vp8 => EncodedVideoCodec::VP8,
        Codec::Vp9 => EncodedVideoCodec::VP9,
        Codec::Av1 => EncodedVideoCodec::AV1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::engine::SyntheticProducer;
    use crate::video_encoder::{
        AdapterInfo, EncodedAccessUnit, EncoderCounters, GpuEncoderFailure,
    };
    use std::sync::{Mutex, atomic::AtomicBool};
    use std::time::Duration;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Call {
        Keyframe,
        Bitrate(u32),
    }

    /// The pass-through source as the pump sees it: the test decides which
    /// control requests are pending instead of needing a live WebRTC sender.
    struct TestSource {
        keyframe: AtomicBool,
        control: Mutex<Option<EncodedRateControl>>,
    }

    impl TestSource {
        fn new(keyframe: bool, control: Option<EncodedRateControl>) -> Self {
            Self {
                keyframe: AtomicBool::new(keyframe),
                control: Mutex::new(control),
            }
        }
    }

    impl EncodedSource for TestSource {
        fn capture_encoded_frame(&self, _frame: &EncodedVideoFrame<'_>) -> bool {
            true
        }

        fn take_keyframe_request(&self) -> bool {
            self.keyframe.swap(false, Ordering::SeqCst)
        }

        fn take_rate_control_request(&self) -> Option<EncodedRateControl> {
            self.control.lock().unwrap().take()
        }
    }

    /// An encoder that only records what the pump asks of it. It produces no
    /// access units, so the delivered-frame watchdog cannot end the run.
    struct RecordingEncoder {
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl GpuEncoder for RecordingEncoder {
        fn submit(&mut self, _frame: Nv12Frame) -> EncodeSubmit {
            EncodeSubmit::Queued
        }

        fn next_packet(&mut self) -> Option<EncodedAccessUnit> {
            None
        }

        fn request_keyframe(&mut self) {
            self.calls.lock().unwrap().push(Call::Keyframe);
        }

        fn set_bitrate(&mut self, target_bps: u32) {
            self.calls.lock().unwrap().push(Call::Bitrate(target_bps));
        }

        fn counters(&self) -> EncoderCounters {
            EncoderCounters::default()
        }

        fn adapter(&self) -> Option<AdapterInfo> {
            None
        }

        fn failure(&self) -> Option<GpuEncoderFailure> {
            None
        }

        fn shutdown(&mut self) {}
    }

    /// Run the real `gpu_frame_loop` for a few ticks and observe what it asked
    /// of the encoder. The loop has no terminal condition by design, so the
    /// test stops it after the bounded window.
    fn run_pump(source: TestSource, calls: Arc<Mutex<Vec<Call>>>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let task = tokio::spawn(gpu_frame_loop(
                Box::new(SyntheticProducer::new(64, 36, false)),
                source,
                Box::new(RecordingEncoder {
                    calls: calls.clone(),
                }),
                Arc::new(EventQueue::default()),
                Arc::new(PreviewStore::default()),
                Arc::new(FrameCounters::default()),
                30,
                (64, 36),
                false,
                Codec::H264,
            ));
            tokio::time::sleep(Duration::from_millis(300)).await;
            task.abort();
            let _ = task.await;
        });
    }

    /// A PLI/FIR forwarded by the pass-through source must reach the driver as
    /// a keyframe request; a delta frame after a lost reference is undecodable.
    #[test]
    fn a_source_keyframe_request_reaches_the_encoder() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        run_pump(TestSource::new(true, None), calls.clone());
        let calls = calls.lock().unwrap().clone();
        assert!(
            calls.contains(&Call::Keyframe),
            "the pump dropped the source keyframe request: {calls:?}"
        );
    }

    /// The negotiated sender's target must be handed to the hardware session
    /// as a rate-control request, not ignored by the local loop.
    #[test]
    fn a_source_rate_control_request_reaches_the_encoder() {
        let control = EncodedRateControl {
            target_bitrate_bps: 1_200_000,
            framerate_fps: 30.0,
        };
        let calls = Arc::new(Mutex::new(Vec::new()));
        run_pump(TestSource::new(false, Some(control)), calls.clone());
        let calls = calls.lock().unwrap().clone();
        assert!(
            calls.contains(&Call::Bitrate(1_200_000)),
            "the pump dropped the rate-control target: {calls:?}"
        );
    }
}
