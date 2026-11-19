//! Audio pump of one publishing attempt: continuous 10 ms frames, silence when
//! the loopback has nothing, and bounded waits around the encoder queue.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use libwebrtc::{audio_frame::AudioFrame, audio_source::native::NativeAudioSource};
use tokio::time;

use super::EventQueue;
use crate::{
    publisher::EngineEvent,
    system_audio::{AudioProducer, BLOCK_INTERVAL, BLOCK_SAMPLES, CHANNELS, SAMPLE_RATE},
};

/// `capture_frame` may block when the encoder queue is full; a stuck source
/// must not freeze the whole engine worker.
const AUDIO_PUSH_TIMEOUT: Duration = Duration::from_secs(1);

/// Audio-side evidence of one attempt. The pump owns these counters; the
/// stats poll only reads them.
#[derive(Default)]
pub(super) struct AudioCounters {
    /// Blocks sent as silence because WASAPI had nothing playing.
    pub(super) silent: AtomicU64,
    /// Blocks skipped because `NativeAudioSource` did not accept one in time.
    pub(super) stalls: AtomicU64,
    /// 10 ms blocks the loopback queue had to drop.
    pub(super) dropped: AtomicU64,
}

impl AudioCounters {
    pub(super) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Measured audio bitrate between two stats polls. `bytes_sent` is cumulative
/// and monotonic within one peer connection.
#[derive(Default)]
pub(super) struct AudioMeter {
    last_bytes: Option<u64>,
    last_at: Option<Instant>,
    kbps: f32,
}

impl AudioMeter {
    pub(super) fn observe(&mut self, bytes: u64) {
        let now = Instant::now();
        if let (Some(previous), Some(at)) = (self.last_bytes, self.last_at) {
            let seconds = now.duration_since(at).as_secs_f32();
            if seconds > 0.2 && bytes >= previous {
                self.kbps = (bytes - previous) as f32 * 8.0 / seconds / 1000.0;
            }
        }
        self.last_bytes = Some(bytes);
        self.last_at = Some(now);
    }

    pub(super) fn kbps(&self) -> f32 {
        self.kbps
    }
}

/// Push exactly one 10 ms stereo frame every 10 ms. When the loopback has no
/// packet (nothing is playing), a silent frame keeps the RTP timeline and the
/// audio m-line continuous. A real device failure is reported to the state
/// machine instead of being disguised as silence.
/// One 10 ms frame from whatever the producer returned. `None`, a wrong
/// length or a failed source become exactly one silent block: the RTP timeline
/// and the audio m-line continue, and the counter records why. A real device
/// failure is reported to the state machine before this runs instead of being
/// disguised as silence.
pub(super) fn frame_for(
    captured: Option<Vec<i16>>,
    counters: &AudioCounters,
) -> AudioFrame<'static> {
    const SILENCE: [i16; BLOCK_SAMPLES] = [0; BLOCK_SAMPLES];
    match captured {
        Some(data) if data.len() == BLOCK_SAMPLES => AudioFrame {
            data: std::borrow::Cow::Owned(data),
            sample_rate: SAMPLE_RATE,
            num_channels: CHANNELS,
            samples_per_channel: (BLOCK_SAMPLES / CHANNELS as usize) as u32,
        },
        _ => {
            counters.silent.fetch_add(1, Ordering::Relaxed);
            AudioFrame {
                data: std::borrow::Cow::Borrowed(&SILENCE),
                sample_rate: SAMPLE_RATE,
                num_channels: CHANNELS,
                samples_per_channel: (BLOCK_SAMPLES / CHANNELS as usize) as u32,
            }
        }
    }
}

pub(super) async fn audio_loop(
    mut producer: Box<dyn AudioProducer>,
    source: NativeAudioSource,
    events: Arc<EventQueue>,
    counters: Arc<AudioCounters>,
) {
    let mut next = time::Instant::now() + BLOCK_INTERVAL;
    loop {
        time::sleep_until(next).await;
        let now = time::Instant::now();
        next = if next <= now {
            now + BLOCK_INTERVAL
        } else {
            next + BLOCK_INTERVAL
        };
        if let Some(failure) = producer.failure() {
            events.push(EngineEvent::Error(format!("Системный звук: {failure}")));
            return;
        }
        counters
            .dropped
            .store(producer.dropped(), Ordering::Relaxed);
        let captured = producer.next_block();
        let frame = frame_for(captured, &counters);
        match time::timeout(AUDIO_PUSH_TIMEOUT, source.capture_frame(&frame)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return,
            Err(_) => {
                // The encoder queue was full for a whole second: drop this
                // block, keep the timeline, count the stall.
                counters.stalls.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
