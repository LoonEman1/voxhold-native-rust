//! Audio pump of one publishing attempt. The native source's clock paces the
//! stream while WASAPI fills a separate, bounded capture reserve.

use std::{
    borrow::Cow,
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
    system_audio::{AudioProducer, BLOCK_SAMPLES, CHANNELS, SAMPLE_RATE},
};

/// `capture_frame` may block when the encoder queue is full; a stuck source
/// must not freeze the whole engine worker.
const AUDIO_PUSH_TIMEOUT: Duration = Duration::from_secs(1);

/// NativeAudioSource consumes one 10 ms block on its own high-precision
/// timer. A push above this threshold waits for that clock to consume one.
pub(super) const SOURCE_QUEUE_MS: u32 = 30;
const PREFILL_SAMPLES: usize = SOURCE_QUEUE_MS as usize / 10 * BLOCK_SAMPLES;
const RAMP_FRAMES: usize = 120; // 2.5 ms at 48 kHz.

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

/// Only lost or missing PCM needs a ramp. The ordinary path keeps the capture
/// bit-for-bit, including the stereo separation and phase across blocks.
#[derive(Default)]
pub(super) struct ProgramAudio {
    last: [i16; CHANNELS as usize],
    dropped: u64,
    playing: bool,
}

impl ProgramAudio {
    pub(super) fn next(
        &mut self,
        captured: Option<Vec<i16>>,
        dropped: u64,
        counters: &AudioCounters,
    ) -> AudioFrame<'static> {
        const SILENCE: [i16; BLOCK_SAMPLES] = [0; BLOCK_SAMPLES];
        let gap = self.dropped != dropped;
        self.dropped = dropped;
        if let Some(mut data) = captured.filter(|data| data.len() == BLOCK_SAMPLES) {
            if gap || !self.playing {
                self.ramp(&mut data);
            }
            self.last
                .copy_from_slice(&data[BLOCK_SAMPLES - CHANNELS as usize..]);
            self.playing = true;
            frame(Cow::Owned(data))
        } else {
            counters.silent.fetch_add(1, Ordering::Relaxed);
            self.playing = false;
            if self.last == [0; CHANNELS as usize] {
                frame(Cow::Borrowed(&SILENCE))
            } else {
                let mut fade = SILENCE.to_vec();
                self.ramp(&mut fade);
                self.last.fill(0);
                frame(Cow::Owned(fade))
            }
        }
    }

    fn ramp(&self, output: &mut [i16]) {
        for (index, stereo) in output
            .chunks_exact_mut(CHANNELS as usize)
            .take(RAMP_FRAMES)
            .enumerate()
        {
            let fade_in = (index + 1) as i32;
            let fade_out = RAMP_FRAMES as i32 - fade_in;
            for (sample, last) in stereo.iter_mut().zip(self.last) {
                *sample = ((i32::from(last) * fade_out + i32::from(*sample) * fade_in)
                    / RAMP_FRAMES as i32) as i16;
            }
        }
    }
}

fn frame(data: Cow<'static, [i16]>) -> AudioFrame<'static> {
    AudioFrame {
        samples_per_channel: (data.len() / CHANNELS as usize) as u32,
        data,
        sample_rate: SAMPLE_RATE,
        num_channels: CHANNELS,
    }
}

pub(super) async fn audio_loop(
    mut producer: Box<dyn AudioProducer>,
    source: NativeAudioSource,
    events: Arc<EventQueue>,
    counters: Arc<AudioCounters>,
) {
    // Prime the native queue to its completion threshold once. Subsequent
    // pushes wait for its real consumption. A second, drifting timer in this
    // task would eventually deliver fewer blocks than the source plays.
    const PREFILL: [i16; PREFILL_SAMPLES] = [0; PREFILL_SAMPLES];
    let mut block = frame(Cow::Borrowed(&PREFILL));
    let mut program = ProgramAudio::default();
    loop {
        match time::timeout(AUDIO_PUSH_TIMEOUT, source.capture_frame(&block)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                events.push(EngineEvent::Error(format!(
                    "Передача системного звука: {error}"
                )));
                return;
            }
            Err(_) => {
                // The native queue may still hold this push's callback. Do
                // not submit another block while that push is outstanding.
                counters.stalls.fetch_add(1, Ordering::Relaxed);
                events.push(EngineEvent::Error(
                    "Очередь системного звука не отвечает.".into(),
                ));
                return;
            }
        }
        if let Some(failure) = producer.failure() {
            events.push(EngineEvent::Error(format!("Системный звук: {failure}")));
            return;
        }
        let captured = producer.next_block();
        let dropped = producer.dropped();
        counters.dropped.store(dropped, Ordering::Relaxed);
        block = program.next(captured, dropped, &counters);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(left: i16, right: i16) -> Vec<i16> {
        [left, right].repeat(BLOCK_SAMPLES / CHANNELS as usize)
    }

    #[test]
    fn a_gap_fades_once_and_resumed_audio_keeps_the_stereo_channels() {
        let counters = AudioCounters::default();
        let mut program = ProgramAudio::default();

        let initial = program.next(Some(stereo(12_000, -6_000)), 0, &counters);
        assert_eq!(&initial.data[..2], &[100, -50]);
        assert_eq!(
            &initial.data[2 * RAMP_FRAMES - 2..2 * RAMP_FRAMES],
            &[12_000, -6_000]
        );

        let steady = stereo(-12_000, 6_000);
        assert_eq!(
            program
                .next(Some(steady.clone()), 0, &counters)
                .data
                .as_ref(),
            steady
        );
        let fading = program.next(None, 0, &counters);
        assert_eq!(&fading.data[..2], &[-11_900, 5_950]);
        assert!(
            fading.data[2 * RAMP_FRAMES - 2..]
                .iter()
                .all(|&sample| sample == 0)
        );
        assert!(matches!(
            program.next(None, 0, &counters).data,
            Cow::Borrowed(_)
        ));

        let resumed = program.next(Some(stereo(12_000, -6_000)), 0, &counters);
        assert_eq!(&resumed.data[..2], &[100, -50]);
        assert_eq!(counters.silent.load(Ordering::Relaxed), 2);

        let after_drop = program.next(Some(stereo(-12_000, 6_000)), 1, &counters);
        assert_eq!(&after_drop.data[..2], &[11_800, -5_900]);
        assert_eq!(
            &after_drop.data[2 * RAMP_FRAMES - 2..2 * RAMP_FRAMES],
            &[-12_000, 6_000]
        );
    }
}
