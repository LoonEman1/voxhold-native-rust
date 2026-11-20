//! Pure PCM timeline policy for the software microphone path.
//!
//! WinMM delivers 20 ms buffers when the driver completes them, not on a
//! 10 ms clock. Sending every completed block immediately makes the block
//! cadence follow the 5 ms poll/driver jitter, which the Opus encoder on the
//! other side hears as quiet crackle. This module separates the two clocks:
//! driver readiness only fills a bounded jitter buffer, and a 10 ms deadline
//! decides when exactly one 480-sample block may leave.
//!
//! The policy is deliberately free of OS calls and `NativeAudioSource` so the
//! reserve, ceiling, late-tick, underrun and epoch rules can be tested with
//! injected `Instant`s and synthetic 20 ms driver buffers.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use super::BLOCK;

/// Initial prefill: the 10 ms clock starts only after 30 ms of PCM is queued.
/// The same reserve is rebuilt after an underrun before transmission resumes.
pub const RESERVE_SAMPLES: usize = BLOCK * 3;
/// Hard ceiling: a stalled consumer may leave at most 80 ms queued. Whole
/// oldest blocks are dropped above it, so latency cannot grow without bound.
pub const CEILING_SAMPLES: usize = BLOCK * 8;
/// One output block per 10 ms at 48 kHz mono.
pub const TICK: Duration = Duration::from_millis(10);
/// Longest wait for a driver event. The deadline (not this timeout) is the
/// output clock; the bound only lets mute/PTT epochs be noticed promptly.
pub const MAX_WAIT: Duration = TICK;
/// A short 5 ms ramp is enough to leave a gap without a full-scale step.
/// `pub(crate)` so the capture tests can assert the fade-in/out envelope.
pub(crate) const FADE_SAMPLES: usize = BLOCK / 2;

/// Aggregated software-capture counters. No PCM or personal content is kept
/// here: only counts, so the values are safe to report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureStats {
    /// Real 480-sample blocks read from the driver and handed to the DSP.
    /// Synthesized fade/silence blocks never increment this counter.
    pub captured_blocks: u64,
    /// `NativeAudioSource::capture_frame` calls that returned `Ok`.
    pub source_ok: u64,
    /// `capture_frame` calls that returned an error (sample rate, state, ...).
    pub source_errors: u64,
    /// `capture_frame` futures that unexpectedly stayed `Pending` instead of
    /// completing synchronously on the `queue_size_ms == 0` path.
    pub source_pending: u64,
    /// Due ticks that could not obtain a full block. One event is counted per
    /// gap, not per wake.
    pub underruns: u64,
    /// Whole 10 ms blocks dropped because the queue exceeded 80 ms.
    pub overflow_blocks: u64,
    /// Whole blocks discarded without being sent: overflow, an underrun
    /// remainder, or an epoch/mute flush.
    pub dropped_blocks: u64,
    /// Whole 10 ms periods skipped because the consumer woke at least one tick
    /// late. This is the count of missed slots, not of late wakes.
    pub late_ticks: u64,
    /// Ticks that woke after the deadline but by less than one 10 ms period.
    /// The deadline is still re-anchored to `now`, so repeated small jitter
    /// never accumulates latency; this counter shows it instead of hiding it.
    pub late_sends: u64,
    /// Driver waits that returned `WAIT_TIMEOUT` (normal when the next tick is
    /// earlier than the next 20 ms driver buffer).
    pub wait_timeouts: u64,
    /// Driver waits that returned an unexpected result.
    pub wait_errors: u64,
    /// Driver queue/completion failures: failed flush, failed close, or a
    /// failed buffer requeue.
    pub driver_errors: u64,
}

/// Result of one poll of the 10 ms policy. The caller sends at most one block
/// per wake and never replays a backlog in a burst.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// The reserve is not built yet, or the next deadline is still ahead.
    None,
    /// `out` holds one real captured block.
    Send,
    /// `out` holds a synthesized fade-to-silence block; the policy has
    /// re-entered buffering and counts one underrun.
    Fade,
}

/// Bounded PCM queue plus the 10 ms emission clock.
pub struct PcmJitter {
    samples: VecDeque<i16>,
    generation: u64,
    /// True until the reserve exists, and again after every underrun.
    buffering: bool,
    next_tick: Option<Instant>,
    /// Last sample handed to the source, used only to ramp an empty underrun
    /// to silence instead of stepping from full scale to zero.
    last_sample: i16,
    underruns: u64,
    overflow_blocks: u64,
    dropped_blocks: u64,
    late_ticks: u64,
    late_sends: u64,
    /// True until the next `Tick::Send` has been ramped in from silence. Set
    /// by construction, every `reset` and every underrun; cleared by the first
    /// send after that, so steady-state blocks are never attenuated.
    ramp_pending: bool,
}

impl Default for PcmJitter {
    fn default() -> Self {
        Self::new()
    }
}

impl PcmJitter {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(CEILING_SAMPLES + BLOCK * 2),
            generation: 0,
            buffering: true,
            next_tick: None,
            last_sample: 0,
            underruns: 0,
            overflow_blocks: 0,
            dropped_blocks: 0,
            late_ticks: 0,
            late_sends: 0,
            ramp_pending: true,
        }
    }

    /// Append one completed driver buffer. A chunk tagged with a generation
    /// the policy does not own is discarded: a buffer that started before a
    /// mute/PTT edge must never adopt the new permission or hijack the policy
    /// back to the old epoch. The capture loop calls `reset` at the edge.
    pub fn push(&mut self, chunk: &[i16], generation: u64) {
        if generation != self.generation {
            self.dropped_blocks += (chunk.len() / BLOCK) as u64;
            return;
        }
        self.samples.extend(chunk.iter().copied());
        while self.samples.len() > CEILING_SAMPLES {
            for _ in 0..BLOCK {
                self.samples.pop_front();
            }
            self.overflow_blocks += 1;
            self.dropped_blocks += 1;
        }
    }

    /// Drop every queued sample, the fade source and the fade-in state, and
    /// require a fresh 30 ms reserve. Used for a mute/PTT/server-permission
    /// epoch change and a capture restart: no audio from the old permission may
    /// leave afterwards, not even through a fade.
    pub fn reset(&mut self, generation: u64) {
        self.dropped_blocks += (self.samples.len() / BLOCK) as u64;
        self.samples.clear();
        self.generation = generation;
        self.buffering = true;
        self.next_tick = None;
        self.last_sample = 0;
        self.ramp_pending = true;
    }

    /// The last sample that actually reached the source. Only a successfully
    /// delivered block may seed this: a failed send never reached the far end,
    /// so fading from it would hide the real discontinuity. A later underrun
    /// ramps from this value so the gap does not start with a click.
    pub fn note_last_sample(&mut self, sample: i16) {
        self.last_sample = sample;
    }

    /// Ask the policy what this wake should do. `out` is filled only for
    /// `Tick::Send` and `Tick::Fade`.
    pub fn tick(&mut self, now: Instant, out: &mut [i16; BLOCK]) -> Tick {
        if self.buffering {
            if self.samples.len() < RESERVE_SAMPLES {
                return Tick::None;
            }
            // Start the clock at this wake: the first block leaves as soon as
            // the reserve exists, and the next deadline is exactly 10 ms later.
            self.buffering = false;
            self.next_tick = Some(now);
        }
        let Some(deadline) = self.next_tick else {
            return Tick::None;
        };
        if now < deadline {
            return Tick::None;
        }
        // Any lateness re-anchors the next deadline to the actual wake. Adding
        // whole periods to the old deadline would let the next block leave
        // almost immediately after a 1..9 ms late send (0/19/20 ms pattern).
        let late = now.duration_since(deadline);
        self.next_tick = Some(now + TICK);
        if late >= TICK {
            // Whole periods were missed: count slots, not wakes.
            self.late_ticks += (late.as_nanos() / TICK.as_nanos()) as u64;
        } else if late > Duration::ZERO {
            // Sub-period lateness: still a late send, counted separately so
            // repeated small jitter is visible without inflating `late_ticks`.
            self.late_sends += 1;
        }
        if self.pop_into(out) {
            self.fade_in(out);
            Tick::Send
        } else {
            self.underrun(out);
            Tick::Fade
        }
    }

    /// Longest safe wait before the next tick or driver/epoch check. A late
    /// deadline returns zero so the caller drains immediately.
    pub fn wait_duration(&self, now: Instant) -> Duration {
        match self.next_tick {
            Some(deadline) if !self.buffering => {
                deadline.saturating_duration_since(now).min(MAX_WAIT)
            }
            _ => MAX_WAIT,
        }
    }

    #[cfg(test)]
    pub fn is_buffering(&self) -> bool {
        self.buffering
    }

    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    pub fn overflow_blocks(&self) -> u64 {
        self.overflow_blocks
    }

    pub fn dropped_blocks(&self) -> u64 {
        self.dropped_blocks
    }

    pub fn late_ticks(&self) -> u64 {
        self.late_ticks
    }

    pub fn late_sends(&self) -> u64 {
        self.late_sends
    }

    /// Counter subset owned by the policy; the capture loop merges its driver
    /// and source counters into it before publishing.
    #[cfg(test)]
    pub fn stats(&self) -> CaptureStats {
        CaptureStats {
            underruns: self.underruns,
            overflow_blocks: self.overflow_blocks,
            dropped_blocks: self.dropped_blocks,
            late_ticks: self.late_ticks,
            late_sends: self.late_sends,
            ..CaptureStats::default()
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[cfg(test)]
    pub fn first(&self) -> Option<i16> {
        self.samples.front().copied()
    }

    fn pop_into(&mut self, out: &mut [i16; BLOCK]) -> bool {
        if self.samples.len() < BLOCK {
            return false;
        }
        for sample in out.iter_mut() {
            *sample = self.samples.pop_front().unwrap_or(0);
        }
        true
    }

    /// Ramp the first block after a reset/underrun in from silence. Only one
    /// block is affected: the next one is already at full amplitude. This is
    /// the symmetric counterpart of [`Self::underrun`], so resuming a stream
    /// after a gap does not start with a full-scale step.
    fn fade_in(&mut self, out: &mut [i16; BLOCK]) {
        if !self.ramp_pending {
            return;
        }
        self.ramp_pending = false;
        let length = FADE_SAMPLES as i32;
        for (index, sample) in out.iter_mut().take(FADE_SAMPLES).enumerate() {
            *sample = (i32::from(*sample) * index as i32 / length) as i16;
        }
    }

    /// Build one fade block from whatever short remainder exists, or ramp the
    /// last sample to silence when the queue is completely empty. The policy
    /// then clears the queue and re-buffers, so stale speech is never replayed
    /// after the gap.
    fn underrun(&mut self, out: &mut [i16; BLOCK]) {
        self.underruns += 1;
        let available = self.samples.len().min(BLOCK);
        if available == 0 {
            let last = i32::from(self.last_sample);
            for (index, sample) in out.iter_mut().enumerate() {
                *sample = if index < FADE_SAMPLES {
                    let gain = (FADE_SAMPLES - 1 - index) as i32;
                    (last * gain / FADE_SAMPLES as i32) as i16
                } else {
                    0
                };
            }
        } else {
            let ramp = available.min(FADE_SAMPLES) as i32;
            for (index, sample) in out.iter_mut().enumerate() {
                if index < available {
                    let value = i32::from(self.samples.pop_front().unwrap_or(0));
                    let gain = if index + ramp as usize >= available {
                        (available - index - 1).min(ramp as usize) as i32
                    } else {
                        ramp
                    };
                    *sample = (value * gain / ramp) as i16;
                } else {
                    *sample = 0;
                }
            }
        }
        self.dropped_blocks += (self.samples.len() / BLOCK) as u64;
        self.samples.clear();
        self.last_sample = 0;
        self.buffering = true;
        self.next_tick = None;
        self.ramp_pending = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(start: i16) -> Vec<i16> {
        (0..BLOCK * 2)
            .map(|index| start.wrapping_add(index as i16))
            .collect()
    }

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn twenty_ms_driver_buffers_emit_one_block_per_ten_ms_without_jitter() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        let mut pushed = 0_i16;
        let mut sent = 0_i16;
        let mut sends = 0;

        for step in 0..=20 {
            let now = at(base, step * 10);
            if step == 1 {
                assert_eq!(jitter.tick(now, &mut out), Tick::None);
                assert_eq!(jitter.len(), BLOCK * 2, "20 ms is below the reserve");
            }
            if step % 2 == 0 {
                jitter.push(&chunk(pushed), 0);
                pushed = pushed.wrapping_add((BLOCK * 2) as i16);
            }
            match jitter.tick(now, &mut out) {
                Tick::Send => {
                    // The sent stream is a contiguous prefix of the captured
                    // stream: order is kept and nothing is duplicated.
                    assert_eq!(out[0], sent);
                    assert_eq!(out[BLOCK - 1], sent.wrapping_add((BLOCK - 1) as i16));
                    sent = sent.wrapping_add(BLOCK as i16);
                    sends += 1;
                }
                Tick::None => assert!(step < 2, "steady state must never stall"),
                Tick::Fade => panic!("unexpected underrun at step {step}"),
            }
        }

        assert_eq!(jitter.len(), BLOCK * 3, "the 30 ms reserve stays intact");
        assert_eq!(jitter.stats().underruns, 0);
        assert_eq!(jitter.stats().overflow_blocks, 0);
        assert_eq!(jitter.stats().late_ticks, 0);
        assert_eq!(sent as usize, sends * BLOCK);
        assert_eq!(sent, pushed.wrapping_sub((BLOCK * 3) as i16));
    }

    #[test]
    fn a_late_consumer_skips_overdue_ticks_instead_of_bursting() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        jitter.push(&chunk(0), 0);
        jitter.push(&chunk((BLOCK * 2) as i16), 0);
        assert_eq!(jitter.tick(at(base, 20), &mut out), Tick::Send);
        assert_eq!(out[0], 0);

        // Driver still runs normally while the consumer is 50 ms late.
        jitter.push(&chunk((BLOCK * 4) as i16), 0);
        jitter.push(&chunk((BLOCK * 6) as i16), 0);

        let before = jitter.stats().late_ticks;
        assert_eq!(jitter.tick(at(base, 70), &mut out), Tick::Send);
        assert_eq!(
            jitter.stats().late_ticks - before,
            4,
            "ticks at 30, 40, 50 and 60 ms are overdue"
        );
        assert_eq!(out[0], (BLOCK) as i16, "speech order is preserved");
        // Exactly one block for the wake: the backlog is not flushed.
        assert_eq!(jitter.tick(at(base, 71), &mut out), Tick::None);
        assert_eq!(jitter.tick(at(base, 80), &mut out), Tick::Send);
        assert_eq!(out[0], (BLOCK * 2) as i16);
        assert_eq!(jitter.stats().overflow_blocks, 0);
    }

    #[test]
    fn an_underrun_fades_once_and_rebuffers_without_stale_speech() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        jitter.push(&chunk(0), 0);
        jitter.push(&chunk((BLOCK * 2) as i16), 0);
        for step in 0..4 {
            assert_eq!(jitter.tick(at(base, 20 + step * 10), &mut out), Tick::Send);
            jitter.note_last_sample(out[BLOCK - 1]);
        }
        assert_eq!(jitter.len(), 0);

        // No PCM for well over 80 ms: exactly one underrun event, not one per
        // wake, and the fade reaches silence.
        assert_eq!(jitter.tick(at(base, 70), &mut out), Tick::Fade);
        assert!(jitter.is_buffering());
        assert_eq!(jitter.stats().underruns, 1);
        assert!(out[0] > 0, "the fade starts from the last sample");
        assert_eq!(out[BLOCK - 1], 0, "the fade ends in silence");
        for step in 0..18 {
            assert_eq!(jitter.tick(at(base, 80 + step * 10), &mut out), Tick::None);
        }
        assert_eq!(jitter.stats().underruns, 1);

        // Two new 20 ms buffers rebuild the 30 ms reserve; only the new PCM
        // resumes and the pre-gap samples are not replayed. Resumption is
        // ramped in from silence: a full-amplitude first block would click.
        jitter.push(&chunk(5_000), 0);
        assert_eq!(jitter.tick(at(base, 270), &mut out), Tick::None);
        jitter.push(&chunk(5_000 + (BLOCK * 2) as i16), 0);
        assert_eq!(jitter.tick(at(base, 280), &mut out), Tick::Send);
        assert_eq!(out[0], 0, "the recovered block must start at silence");
        assert!(
            out[..FADE_SAMPLES]
                .windows(2)
                .all(|pair| pair[0] <= pair[1]),
            "the recovered block must rise monotonically"
        );
        let raw = |index: usize| 5_000_i32 + index as i32;
        assert!(
            i32::from(out[FADE_SAMPLES - 1]) < raw(FADE_SAMPLES - 1),
            "the last ramped sample is still attenuated"
        );
        for index in FADE_SAMPLES..BLOCK {
            assert_eq!(out[index], raw(index) as i16);
        }
        // The following block is back to full amplitude.
        assert_eq!(jitter.tick(at(base, 290), &mut out), Tick::Send);
        assert_eq!(out[0], (5_000 + BLOCK) as i16);
        assert_eq!(jitter.stats().underruns, 1);
    }

    #[test]
    fn overflow_drops_whole_oldest_blocks_and_stays_bounded() {
        let mut jitter = PcmJitter::new();
        for index in 0..10_i16 {
            jitter.push(&chunk(index.wrapping_mul((BLOCK * 2) as i16)), 0);
            assert!(jitter.len() <= CEILING_SAMPLES);
        }
        assert_eq!(jitter.len(), CEILING_SAMPLES);
        assert_eq!(jitter.stats().overflow_blocks, 12);
        assert_eq!(jitter.stats().dropped_blocks, 12);
        // Ten 20 ms buffers minus the six oldest dropped 20 ms blocks.
        assert_eq!(jitter.first(), Some(6_i16.wrapping_mul((BLOCK * 2) as i16)));
    }

    #[test]
    fn a_fast_mute_unmute_epoch_never_replays_pre_mute_audio() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        jitter.push(&vec![9_000_i16; BLOCK * 2], 0);
        jitter.push(&vec![9_000_i16; BLOCK * 2], 0);
        assert_eq!(jitter.tick(at(base, 0), &mut out), Tick::Send);
        // The very first block after construction is ramped in; its tail still
        // carries the captured 9 000 amplitude.
        assert_eq!(out[0], 0, "the first block ramps in from silence");
        assert!(out[FADE_SAMPLES..].iter().all(|sample| *sample == 9_000));

        // Mute and unmute both advanced the epoch; the fast edge restores the
        // boolean but not the queue.
        jitter.reset(2);
        assert_eq!(jitter.len(), 0);
        assert!(jitter.is_buffering());
        // A completed driver buffer tagged with the old generation is dropped
        // instead of adopting the new permission.
        jitter.push(&vec![7_000_i16; BLOCK * 2], 1);
        assert_eq!(jitter.len(), 0);
        jitter.push(&vec![222_i16; BLOCK * 2], 2);
        jitter.push(&vec![222_i16; BLOCK * 2], 2);
        assert_eq!(jitter.tick(at(base, 40), &mut out), Tick::Send);
        assert_eq!(
            out[0], 0,
            "the first block after unmute must ramp in from silence"
        );
        assert!(
            out[FADE_SAMPLES..].iter().all(|sample| *sample == 222),
            "a pre-mute sample reached the output"
        );
    }

    /// Regression for review item P2: at 0/19/20 ms the 9 ms late send used to
    /// keep the old deadline (20 ms), so the next block left 1 ms later.
    #[test]
    fn a_sub_period_late_send_reanchors_the_deadline_and_does_not_burst() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        jitter.push(&chunk(0), 0);
        jitter.push(&chunk((BLOCK * 2) as i16), 0);

        assert_eq!(jitter.tick(base, &mut out), Tick::Send);
        assert_eq!(jitter.stats().late_sends, 0);
        assert_eq!(jitter.tick(at(base, 19), &mut out), Tick::Send);
        assert_eq!(
            jitter.stats().late_sends,
            1,
            "9 ms lateness is a short late send"
        );
        assert_eq!(jitter.stats().late_ticks, 0, "no whole period was missed");
        assert_eq!(
            jitter.tick(at(base, 20), &mut out),
            Tick::None,
            "the next block is not taken 1 ms after a late send"
        );
        // The deadline is anchored at the actual send (29 ms), not at 20 ms.
        assert_eq!(jitter.tick(at(base, 28), &mut out), Tick::None);
        assert_eq!(jitter.tick(at(base, 29), &mut out), Tick::Send);
        assert_eq!(jitter.stats().late_sends, 1, "an on-time send is not late");
    }

    /// A constant 2 ms lateness per wake must not accumulate: every gap stays
    /// one period plus the fixed offset (never a 1 ms catch-up), `late_sends`
    /// counts each short late send, `late_ticks` stays zero.
    #[test]
    fn repeated_small_lateness_never_accumulates_latency() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        jitter.push(&chunk(0), 0);
        jitter.push(&chunk((BLOCK * 2) as i16), 0);
        let mut pushed = 2_i16;
        let mut last_send = None;
        let mut sends = 0_u64;
        let mut minimum_gap = Duration::MAX;
        let mut maximum_gap = Duration::ZERO;

        for cycle in 0..1_000_u64 {
            if jitter.len() < RESERVE_SAMPLES {
                jitter.push(&chunk(pushed.wrapping_mul((BLOCK * 2) as i16)), 0);
                pushed += 1;
            }
            // Every wake is exactly 2 ms after its deadline. Re-anchoring at
            // `now` makes the next wake 12 ms later, so the offset stays 2 ms
            // instead of compounding into the deadline.
            let now = at(base, cycle * 12 + 2);
            if jitter.tick(now, &mut out) == Tick::Send {
                if let Some(last) = last_send {
                    minimum_gap = minimum_gap.min(now - last);
                    maximum_gap = maximum_gap.max(now - last);
                }
                last_send = Some(now);
                sends += 1;
            }
        }

        assert_eq!(sends, 1_000, "one block per wake, never a burst");
        assert!(
            minimum_gap >= TICK,
            "sends are never closer than one period: {minimum_gap:?}"
        );
        assert!(
            maximum_gap <= TICK + Duration::from_millis(2),
            "the constant 2 ms offset does not grow: {maximum_gap:?}"
        );
        assert!(
            jitter.stats().late_sends >= 999,
            "999 short late sends are visible: {}",
            jitter.stats().late_sends
        );
        assert_eq!(
            jitter.stats().late_ticks,
            0,
            "short lateness must not inflate late_ticks"
        );
        assert_eq!(jitter.stats().underruns, 0);
    }

    /// A 55 ms stall misses whole periods, sends exactly one block and anchors
    /// the next deadline at now + 10 ms instead of replaying the backlog.
    #[test]
    fn a_long_stall_counts_missed_periods_and_sends_one_block() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        jitter.push(&chunk(0), 0);
        jitter.push(&chunk((BLOCK * 2) as i16), 0);
        assert_eq!(jitter.tick(base, &mut out), Tick::Send);

        // The driver keeps filling the bounded queue during the stall.
        jitter.push(&chunk((BLOCK * 4) as i16), 0);
        jitter.push(&chunk((BLOCK * 6) as i16), 0);
        assert_eq!(jitter.tick(at(base, 65), &mut out), Tick::Send);
        assert_eq!(jitter.stats().late_ticks, 5, "55 ms is 5 missed periods");
        assert_eq!(jitter.stats().late_sends, 0);
        assert_eq!(out[0], BLOCK as i16, "speech order is preserved");
        // Exactly one block for the wake, and the deadline moved to 75 ms.
        assert_eq!(jitter.tick(at(base, 66), &mut out), Tick::None);
        assert_eq!(jitter.tick(at(base, 74), &mut out), Tick::None);
        assert_eq!(jitter.tick(at(base, 75), &mut out), Tick::Send);
        assert_eq!(jitter.stats().overflow_blocks, 0);
    }

    /// Ten simulated minutes of a 20 ms driver with jitter: the bounded queue
    /// never leaves [RESERVE, CEILING] and neither latency nor overflow grows.
    #[test]
    fn ten_minutes_of_jittered_driver_keeps_the_queue_bounded() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        let mut pushed = 0_i16;
        let mut chunk_index = 0_u64;
        let mut next_driver = 0_u64;
        let driver_jitter = [0_u64, 2, 4, 1, 3, 0, 2, 1];
        let mut maximum_len = 0_usize;
        let mut last_refill_len = 0_usize;

        for millis in 0..600_000_u64 {
            let mut refilled = false;
            if millis == next_driver {
                jitter.push(&chunk(pushed), 0);
                pushed = pushed.wrapping_add((BLOCK * 2) as i16);
                chunk_index += 1;
                let offset = driver_jitter[chunk_index as usize % driver_jitter.len()];
                next_driver = chunk_index * 20 + offset;
                refilled = true;
            }
            match jitter.tick(at(base, millis), &mut out) {
                Tick::Send | Tick::None => {}
                Tick::Fade => panic!("underrun at {millis} ms"),
            }
            maximum_len = maximum_len.max(jitter.len());
            assert!(
                jitter.len() <= CEILING_SAMPLES,
                "queue exceeded the ceiling at {millis} ms"
            );
            if refilled && millis >= 100 {
                // At a driver refill boundary the queue has rebuilt at least
                // the reserve and never passes the ceiling.
                assert!(
                    (RESERVE_SAMPLES..=CEILING_SAMPLES).contains(&jitter.len()),
                    "refill left {} samples at {millis} ms",
                    jitter.len()
                );
                last_refill_len = jitter.len();
            }
        }
        assert_eq!(jitter.stats().underruns, 0);
        assert_eq!(jitter.stats().overflow_blocks, 0);
        assert_eq!(jitter.stats().late_ticks, 0);
        assert!(maximum_len <= CEILING_SAMPLES);
        assert!(
            last_refill_len >= RESERVE_SAMPLES,
            "the queue did not stay above the reserve: {last_refill_len}"
        );
    }

    #[test]
    fn wait_timeout_never_exceeds_the_tick_and_zero_when_overdue() {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut out = [0_i16; BLOCK];
        assert_eq!(jitter.wait_duration(base), MAX_WAIT);
        jitter.push(&chunk(0), 0);
        jitter.push(&chunk((BLOCK * 2) as i16), 0);
        assert_eq!(jitter.tick(base, &mut out), Tick::Send);
        assert_eq!(jitter.wait_duration(base), Duration::from_millis(10));
        assert_eq!(jitter.wait_duration(at(base, 25)), Duration::ZERO);
    }
}
