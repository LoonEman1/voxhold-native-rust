//! Software playout mixer for the room voice bus.
//!
//! The pinned WebRTC wrapper exposes no per-track gain, so a numeric 0–200 %
//! participant volume can only be applied to decoded PCM. This module is that
//! PCM path: per-source gains, a local-mute flag that is *not* the stored gain,
//! a peak limiter for the summed mix, a post-mix meter, and the starvation /
//! drop counters that the playout transport publishes.
//!
//! Nothing here touches a device: the transport lives in `voice::playout`, and
//! the same rules are unit-tested with two known tones.

use std::collections::{BTreeMap, VecDeque};

/// One 10 ms block at 48 kHz mono, the quantum of the playout path.
pub const BLOCK_SAMPLES: usize = 480;
/// Bounded per-source queue. Five blocks (50 ms) is more than one late RTP
/// packet; growing further would only add latency.
pub const SOURCE_BLOCKS: usize = 5;
/// A source that delivered audio within this many blocks but not for the
/// current one is treated as starved (underrun), not as silence.
const STARVE_BLOCKS: u64 = 5;
/// The summed mix is kept just below full scale so the i16 conversion never
/// wraps on a hot mix.
pub const LIMIT_CEILING: f64 = 0.98;
/// Release of the limiter gain towards unity, applied once per quiet block.
/// 2 % per 10 ms block: a gain of 0.49 recovers to 99 % in about 195 blocks
/// (about 2 s), which the unit test measures instead of leaving it
/// undocumented.
const LIMIT_RELEASE: f64 = 0.02;
/// Rolling window for the block-time percentiles (about 5 s at 10 ms blocks).
const LATENCY_WINDOW: usize = 512;
/// The room meter uses the same display scale as the WinMM preview (Go
/// `min(1, sqrt(energy / samples) * 4.5)`).
const METER_GAIN: f64 = 4.5;
const METER_SMOOTHING: f64 = 0.55;

/// The three playout buses. `Voice` is mixed by this module. A watched stream
/// (`System`) and the settings preview (`Preview`) keep their own engines and
/// lifetimes; their gains are separate values here so a caller cannot fold one
/// into another, and only the voice bus is applied by this module today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    Voice,
    System,
    Preview,
}

impl Bus {
    fn index(self) -> usize {
        match self {
            Bus::Voice => 0,
            Bus::System => 1,
            Bus::Preview => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source {
    /// Per-participant gain in 0..=2 (0–200 %).
    pub gain: f64,
    /// Local mute / deafen. Kept apart from `gain`: unmuting restores the
    /// stored percentage instead of a remembered zero.
    pub muted: bool,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
        }
    }
}

/// A bounded per-source block queue.
#[derive(Default, Debug)]
pub struct SourceQueue {
    blocks: VecDeque<Vec<i16>>,
    pushed: u64,
    last_push: u64,
    seen: u64,
    dropped: u64,
    starved: u64,
}

impl SourceQueue {
    /// Queue one 10 ms block. A source that runs ahead of the mixer loses its
    /// oldest block instead of growing latency.
    pub fn push(&mut self, block: Vec<i16>) {
        if self.blocks.len() >= SOURCE_BLOCKS {
            self.blocks.pop_front();
            self.dropped += 1;
        }
        self.pushed += 1;
        self.last_push = self.seen;
        self.blocks.push_back(block);
    }

    /// Fill `out` with the next block and return whether a block was queued.
    /// A missing block is silence; it counts as starvation only while the
    /// source was recently delivering (a source that is simply quiet between
    /// packets must not inflate the underrun counter).
    pub fn pop_into(&mut self, out: &mut [i16]) -> bool {
        self.seen += 1;
        match self.blocks.pop_front() {
            Some(mut block) => {
                // A short block (a boundary in the source stream) is padded
                // with silence; a long one is truncated to the quantum.
                block.resize(out.len(), 0);
                out.copy_from_slice(&block[..out.len()]);
                true
            }
            None => {
                out.fill(0);
                if self.pushed > 0 && self.seen.saturating_sub(self.last_push) <= STARVE_BLOCKS {
                    self.starved += 1;
                }
                false
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn starved(&self) -> u64 {
        self.starved
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }
}

/// All sources of one bus, keyed by the receiver track id the media engine
/// reports. Ordering is deterministic so a mix cannot depend on hash order.
#[derive(Default, Debug)]
pub struct SourcePool {
    queues: BTreeMap<String, SourceQueue>,
}

impl SourcePool {
    pub fn ensure(&mut self, id: &str) {
        if !self.queues.contains_key(id) && self.queues.len() < 64 {
            self.queues.insert(id.to_owned(), SourceQueue::default());
        }
    }

    pub fn push(&mut self, id: &str, block: Vec<i16>) {
        self.ensure(id);
        if let Some(queue) = self.queues.get_mut(id) {
            queue.push(block);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.queues.remove(id);
    }

    pub fn clear(&mut self) {
        self.queues.clear();
    }

    pub fn len(&self) -> usize {
        self.queues.len()
    }

    pub fn dropped(&self) -> u64 {
        self.queues.values().map(SourceQueue::dropped).sum()
    }

    pub fn starved(&self) -> u64 {
        self.queues.values().map(SourceQueue::starved).sum()
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.queues.keys()
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut SourceQueue)> {
        self.queues.iter_mut()
    }
}

/// Peak limiter with an instant attack and a per-block release. The attack is
/// per block (one gain for the whole block) so a transient cannot clip the i16
/// conversion; the release is applied once per block that is no longer limited.
#[derive(Debug)]
pub struct Limiter {
    gain: f64,
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Limiter {
    pub fn new() -> Self {
        Self { gain: 1.0 }
    }

    /// Scale one f64 block in place. Returns whether the block was limited and
    /// whether it released gain towards unity (for the published counters).
    pub fn process(&mut self, block: &mut [f64]) -> (bool, bool) {
        let peak = block
            .iter()
            .fold(0.0_f64, |peak, value| peak.max(value.abs()));
        let mut limited = false;
        if peak > LIMIT_CEILING {
            let target = LIMIT_CEILING / peak;
            if target < self.gain {
                self.gain = target;
                limited = true;
            }
        }
        for value in block.iter_mut() {
            *value *= self.gain;
        }
        let mut released = false;
        if !limited && self.gain < 1.0 {
            self.gain = (self.gain + (1.0 - self.gain) * LIMIT_RELEASE).min(1.0);
            released = true;
        }
        (limited, released)
    }

    #[cfg(test)]
    pub fn gain(&self) -> f64 {
        self.gain
    }
}

/// Post-mix measurement of one block.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MixOutcome {
    /// Peak absolute level after the limiter, 0..=1.
    pub peak: f64,
    /// RMS after the limiter, 0..=1.
    pub rms: f64,
    /// Whether this block was reduced by the limiter.
    pub limited: bool,
    /// Number of sources that contributed audio to this block.
    pub sources: usize,
}

/// Block-time percentiles and transport counters. Milliseconds are not used:
/// the mix must fit in a 10 ms quantum, so microseconds are the useful unit.
#[derive(Default, Debug)]
pub struct MixStats {
    latencies: VecDeque<u32>,
    blocks: u64,
    underruns: u64,
    dropped: u64,
}

impl MixStats {
    pub fn record_latency(&mut self, micros: u32) {
        if self.latencies.len() == LATENCY_WINDOW {
            self.latencies.pop_front();
        }
        self.latencies.push_back(micros);
    }

    pub fn record_block(&mut self) {
        self.blocks += 1;
    }

    pub fn add_underruns(&mut self, count: u64) {
        self.underruns = self.underruns.saturating_add(count);
    }

    pub fn add_dropped(&mut self, count: u64) {
        self.dropped = self.dropped.saturating_add(count);
    }

    pub fn p50_micros(&self) -> u32 {
        self.percentile(50)
    }

    /// Nearest-rank p95: the smallest sample that is at least 95 % of the
    /// window. An empty window is zero, never a panic.
    pub fn p95_micros(&self) -> u32 {
        self.percentile(95)
    }

    fn percentile(&self, percent: usize) -> u32 {
        if self.latencies.is_empty() {
            return 0;
        }
        let mut sorted: Vec<u32> = self.latencies.iter().copied().collect();
        sorted.sort_unstable();
        let rank = (percent * sorted.len()).div_ceil(100).max(1);
        sorted[rank - 1]
    }

    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// A snapshot of the playout path for the UI and the report.
///
/// Provenance: `p95_micros`/`blocks`/`limiter_releases` are measured by the
/// mixer thread on the host CPU; `underruns`/`dropped` are counted by the
/// bounded queues; `latency_ms` is the number of whole 10 ms blocks the device
/// still holds (`buffered_blocks * 10`), not a wall-clock measurement. The
/// unit tests exercise the mix on synthetic blocks; nothing here is a
/// measurement of a physical output device.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayoutStats {
    pub level: f32,
    pub peak: f32,
    pub underruns: u64,
    pub dropped: u64,
    /// Measured mix time per block: nearest-rank p95 of the rolling window.
    pub p95_micros: u32,
    /// Whole 10 ms blocks currently queued in the device sink.
    pub latency_ms: u16,
    /// Blocks the limiter reduced, and blocks where it released gain.
    pub limited_blocks: u64,
    pub limiter_releases: u64,
    pub blocks: u64,
    /// Successful software-sink reopens in this session (resume/recovery).
    pub reconnects: u32,
    /// The software sink is open. `false` means the room is on the platform
    /// ADM (or has no live path) and the numeric gain is not applied.
    pub running: bool,
    /// Recovery state machine, not a guess: the UI and the journal show it.
    pub phase: PlayoutPhase,
}

/// Lifecycle of the software playout as the recovery machine sees it.
///
/// An open sink is `Unstable` until it proves itself: it must live for
/// `STABLE_AFTER` and write `STABLE_BLOCKS` blocks without a driver error. Only
/// then does it become `Stable` and clear the failure budget, so a sink that
/// opens and immediately dies still spends the shared budget and can reach the
/// ADM fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayoutPhase {
    /// No sink; the next bounded open attempt is scheduled.
    #[default]
    Opening,
    /// Sink open, stability not yet proven.
    Unstable,
    /// Sink proved stable; the failure budget is clear.
    Stable,
    /// The budget was spent; the platform ADM owns the room.
    AdmFallback,
}

impl PlayoutPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Opening => "открытие",
            Self::Unstable => "проверка стабильности",
            Self::Stable => "стабильный микшер",
            Self::AdmFallback => "системный ADM",
        }
    }
}

/// The bus gains, source gains, limiter and meter of one playout path.
pub struct Mixer {
    bus: [f64; 3],
    sources: BTreeMap<String, Source>,
    limiter: Limiter,
    stats: MixStats,
    scratch: Vec<f64>,
    block: Vec<i16>,
    level: f64,
    peak: f64,
    limited_blocks: u64,
    limiter_releases: u64,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            bus: [1.0; 3],
            sources: BTreeMap::new(),
            limiter: Limiter::default(),
            stats: MixStats::default(),
            scratch: Vec::new(),
            block: Vec::new(),
            level: 0.0,
            peak: 0.0,
            limited_blocks: 0,
            limiter_releases: 0,
        }
    }
}

impl Mixer {
    pub fn set_bus_gain(&mut self, bus: Bus, gain: f64) {
        self.bus[bus.index()] = gain.clamp(0.0, 2.0);
    }

    pub fn bus_gain(&self, bus: Bus) -> f64 {
        self.bus[bus.index()]
    }

    pub fn set_source(&mut self, id: &str, source: Source) {
        if self.sources.len() < 64 || self.sources.contains_key(id) {
            self.sources.insert(
                id.to_owned(),
                Source {
                    gain: source.gain.clamp(0.0, 2.0),
                    muted: source.muted,
                },
            );
        }
    }

    pub fn source(&self, id: &str) -> Option<Source> {
        self.sources.get(id).copied()
    }

    pub fn remove_source(&mut self, id: &str) {
        self.sources.remove(id);
    }

    pub fn clear_sources(&mut self) {
        self.sources.clear();
    }

    /// Mix one block of every source in `pool` through `bus`.
    ///
    /// Each source block is consumed even when it is muted or at 0 % so a mute
    /// cannot build up a backlog; the mix is then limited and converted to
    /// i16. `out` defines the block length.
    pub fn render(&mut self, bus: Bus, pool: &mut SourcePool, out: &mut [i16]) -> MixOutcome {
        if self.scratch.len() < out.len() {
            self.scratch.resize(out.len(), 0.0);
        }
        if self.block.len() < out.len() {
            self.block.resize(out.len(), 0);
        }
        let mix = &mut self.scratch[..out.len()];
        mix.fill(0.0);
        let mut sources = 0;
        for (id, queue) in pool.iter_mut() {
            let source = self.sources.get(id).copied().unwrap_or_default();
            let gain = if source.muted { 0.0 } else { source.gain };
            let block = &mut self.block[..out.len()];
            let present = queue.pop_into(block);
            if gain == 0.0 {
                continue;
            }
            if present {
                sources += 1;
            }
            for (value, sample) in mix.iter_mut().zip(block.iter()) {
                *value += f64::from(*sample) / 32768.0 * gain;
            }
        }
        let bus_gain = self.bus[bus.index()];
        for value in mix.iter_mut() {
            *value *= bus_gain;
        }
        let (limited, released) = self.limiter.process(mix);
        if limited {
            self.limited_blocks += 1;
        }
        if released {
            self.limiter_releases += 1;
        }
        let peak = mix
            .iter()
            .fold(0.0_f64, |peak, value| peak.max(value.abs()));
        let energy: f64 = mix.iter().map(|value| value * value).sum();
        let rms = if mix.is_empty() {
            0.0
        } else {
            (energy / mix.len() as f64).sqrt()
        };
        for (slot, value) in out.iter_mut().zip(mix.iter()) {
            let scaled = (value * 32768.0).round().clamp(-32768.0, 32767.0);
            *slot = scaled as i16;
        }
        let measured = (rms * METER_GAIN).min(1.0);
        self.level = self.level * (1.0 - METER_SMOOTHING) + measured * METER_SMOOTHING;
        self.peak = peak;
        self.stats.record_block();
        MixOutcome {
            peak,
            rms,
            limited,
            sources,
        }
    }

    /// Smoothed post-mix meter (0..=1).
    pub fn level(&self) -> f32 {
        self.level as f32
    }

    /// Last post-limiter peak (0..=1).
    pub fn peak(&self) -> f32 {
        self.peak as f32
    }

    pub fn limited_blocks(&self) -> u64 {
        self.limited_blocks
    }

    pub fn limiter_releases(&self) -> u64 {
        self.limiter_releases
    }

    pub fn stats(&self) -> &MixStats {
        &self.stats
    }

    pub fn stats_mut(&mut self) -> &mut MixStats {
        &mut self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, amplitude: f64, phase: f64, len: usize) -> Vec<i16> {
        (0..len)
            .map(|index| {
                let t = index as f64 / 48_000.0;
                ((2.0 * std::f64::consts::PI * rate * t + phase).sin() * amplitude) as i16
            })
            .collect()
    }

    fn pool_with(id: &str, block: Vec<i16>) -> SourcePool {
        let mut pool = SourcePool::default();
        pool.push(id, block);
        pool
    }

    fn render(mixer: &mut Mixer, pool: &mut SourcePool) -> (Vec<i16>, MixOutcome) {
        let mut out = vec![0_i16; BLOCK_SAMPLES];
        let outcome = mixer.render(Bus::Voice, pool, &mut out);
        (out, outcome)
    }

    #[test]
    fn two_known_tones_are_summed_and_measured_after_the_mix() {
        // 400 Hz and 800 Hz are whole periods in the 480-sample block, so the
        // block RMS is the long-term RMS instead of a partial-period estimate.
        let a = tone(400.0, 8192.0, 0.0, BLOCK_SAMPLES);
        let b = tone(800.0, 4096.0, 0.7, BLOCK_SAMPLES);
        let mut mixer = Mixer::default();
        mixer.set_source("a", Source::default());
        mixer.set_source("b", Source::default());
        let mut pool = SourcePool::default();
        pool.push("a", a.clone());
        pool.push("b", b.clone());
        let (out, outcome) = render(&mut mixer, &mut pool);
        for index in 0..BLOCK_SAMPLES {
            let expected = i32::from(a[index]) + i32::from(b[index]);
            assert!(
                (i32::from(out[index]) - expected).abs() <= 1,
                "sample {index}: {} != {expected}",
                out[index]
            );
        }
        let expected_rms = ((8192.0_f64.powi(2) + 4096.0_f64.powi(2)) / 2.0).sqrt() / 32768.0;
        assert!(
            (outcome.rms - expected_rms).abs() < 5e-4,
            "RMS {} vs {expected_rms}",
            outcome.rms
        );
        assert_eq!(outcome.sources, 2);
        assert!(!outcome.limited, "the sum stays under the ceiling");
        // The meter is the same display scale as the WinMM preview, smoothed
        // per block; after several identical blocks it follows the mix.
        let mut level = 0.0_f32;
        for _ in 0..20 {
            let mut next = SourcePool::default();
            next.push("a", a.clone());
            next.push("b", b.clone());
            let (_, outcome) = render(&mut mixer, &mut next);
            assert!((outcome.rms - expected_rms).abs() < 5e-4);
            level = mixer.level();
        }
        let expected_level = (expected_rms * METER_GAIN).min(1.0) as f32;
        assert!(
            (level - expected_level).abs() < 0.01,
            "post-mix meter follows the RMS: {level} vs {expected_level}"
        );
        assert!(mixer.peak() > 0.3);
    }

    #[test]
    fn two_hundred_percent_doubles_a_source_and_the_limiter_caps_a_hot_mix() {
        let source_tone = tone(400.0, 8000.0, 0.0, BLOCK_SAMPLES);
        let mut mixer = Mixer::default();
        mixer.set_source(
            "a",
            Source {
                gain: 2.0,
                muted: false,
            },
        );
        let (out, outcome) = render(&mut mixer, &mut pool_with("a", source_tone.clone()));
        let peak = out
            .iter()
            .map(|s| i32::from(*s).unsigned_abs())
            .max()
            .unwrap();
        let single = source_tone
            .iter()
            .map(|s| i32::from(*s).unsigned_abs())
            .max()
            .unwrap();
        assert!(
            (peak as f64 - 2.0 * single as f64).abs() < 4.0,
            "200 % must double the delivered amplitude: {peak} vs {single}"
        );
        assert!(!outcome.limited);

        // A full-scale tone on top of another full-scale tone would wrap
        // without the limiter.
        let full = tone(1000.0, 32767.0, 0.0, BLOCK_SAMPLES);
        let mut hot = Mixer::default();
        hot.set_source("a", Source::default());
        hot.set_source("b", Source::default());
        let mut pool = SourcePool::default();
        pool.push("a", full.clone());
        pool.push("b", full);
        let (out, outcome) = render(&mut hot, &mut pool);
        let peak = out
            .iter()
            .map(|s| i32::from(*s).unsigned_abs())
            .max()
            .unwrap();
        assert!(outcome.limited, "two full-scale tones must hit the limiter");
        assert!(
            f64::from(peak) <= LIMIT_CEILING * 32768.0 + 1.0,
            "the limited peak {peak} stays under the ceiling"
        );
        assert_eq!(hot.limited_blocks(), 1);
        // The gain is held for the block and released only afterwards.
        assert!(hot.limiter.gain() > 0.4 && hot.limiter.gain() < 0.5);
    }

    #[test]
    fn a_local_mute_keeps_the_saved_gain_and_restores_it_on_unmute() {
        let tone = tone(400.0, 8000.0, 0.0, BLOCK_SAMPLES);
        let mut mixer = Mixer::default();
        mixer.set_source(
            "a",
            Source {
                gain: 1.5,
                muted: true,
            },
        );
        let (out, _) = render(&mut mixer, &mut pool_with("a", tone.clone()));
        assert!(out.iter().all(|sample| *sample == 0), "a mute is silence");
        assert_eq!(
            mixer.source("a"),
            Some(Source {
                gain: 1.5,
                muted: true
            })
        );
        mixer.set_source(
            "a",
            Source {
                gain: 1.5,
                muted: false,
            },
        );
        let (restored, _) = render(&mut mixer, &mut pool_with("a", tone.clone()));
        let peak = restored
            .iter()
            .map(|s| i32::from(*s).unsigned_abs())
            .max()
            .unwrap();
        let source_peak = tone
            .iter()
            .map(|s| i32::from(*s).unsigned_abs())
            .max()
            .unwrap();
        assert!(
            (peak as f64 - 1.5 * source_peak as f64).abs() < 4.0,
            "unmuting must bring back the stored 150 %, not 100 %"
        );
    }

    #[test]
    fn a_zero_percent_source_is_silent_but_still_keeps_streaming() {
        let mut mixer = Mixer::default();
        mixer.set_source(
            "a",
            Source {
                gain: 0.0,
                muted: false,
            },
        );
        let mut pool = SourcePool::default();
        pool.push("a", tone(440.0, 12000.0, 0.0, BLOCK_SAMPLES));
        let (out, _) = render(&mut mixer, &mut pool);
        assert!(out.iter().all(|sample| *sample == 0));
        // The block was consumed, so an unmute starts at the current audio
        // instead of replaying the backlog.
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.queues.get("a").unwrap().len(), 0);
    }

    #[test]
    fn a_deafened_bus_is_silent_and_restores_the_source_gain() {
        let tone = tone(400.0, 8000.0, 0.0, BLOCK_SAMPLES);
        let mut mixer = Mixer::default();
        mixer.set_source(
            "a",
            Source {
                gain: 1.25,
                muted: false,
            },
        );
        mixer.set_bus_gain(Bus::Voice, 0.0);
        let (out, _) = render(&mut mixer, &mut pool_with("a", tone.clone()));
        assert!(out.iter().all(|sample| *sample == 0));
        assert_eq!(mixer.source("a").unwrap().gain, 1.25);
        mixer.set_bus_gain(Bus::Voice, 1.0);
        let (restored, _) = render(&mut mixer, &mut pool_with("a", tone));
        assert!(restored.iter().any(|sample| *sample != 0));
        // The buses are separate values.
        assert_eq!(mixer.bus_gain(Bus::System), 1.0);
        assert_eq!(mixer.bus_gain(Bus::Preview), 1.0);
        mixer.set_bus_gain(Bus::System, 1.5);
        assert_eq!(mixer.bus_gain(Bus::Voice), 1.0);
    }

    #[test]
    fn p95_uses_nearest_rank_and_an_empty_window_is_zero() {
        let mut stats = MixStats::default();
        assert_eq!(stats.p95_micros(), 0);
        for value in [10, 20, 30, 40, 100] {
            stats.record_latency(value);
        }
        // ceil(0.95 * 5) = 5 -> the largest sample.
        assert_eq!(stats.p95_micros(), 100);
        assert_eq!(stats.p50_micros(), 30);
        let mut many = MixStats::default();
        for value in 1..=100 {
            many.record_latency(value);
        }
        assert_eq!(many.p95_micros(), 95, "ceil(95) is the 95th sample");
        assert_eq!(many.p50_micros(), 50);
    }

    #[test]
    fn an_overfull_source_queue_drops_the_oldest_and_counts_it() {
        let mut pool = SourcePool::default();
        for _ in 0..(SOURCE_BLOCKS + 2) {
            pool.push("a", vec![1_i16; BLOCK_SAMPLES]);
        }
        assert_eq!(pool.queues.get("a").unwrap().len(), SOURCE_BLOCKS);
        assert_eq!(pool.dropped(), 2);
        let mut mixer = Mixer::default();
        let mut out = vec![0_i16; BLOCK_SAMPLES];
        mixer.render(Bus::Voice, &mut pool, &mut out);
        assert_eq!(pool.queues.get("a").unwrap().len(), SOURCE_BLOCKS - 1);
    }

    #[test]
    fn a_recently_active_source_that_stops_is_counted_as_underrun() {
        let mut queue = SourceQueue::default();
        queue.push(vec![7_i16; BLOCK_SAMPLES]);
        let mut out = vec![0_i16; BLOCK_SAMPLES];
        assert!(queue.pop_into(&mut out));
        for _ in 0..STARVE_BLOCKS {
            assert!(!queue.pop_into(&mut out));
        }
        assert_eq!(
            queue.starved(),
            STARVE_BLOCKS - 1,
            "the block delivered for this tick is not itself a missing block"
        );
        // Once the window is over the source is quiet, not starved.
        assert!(!queue.pop_into(&mut out));
        assert_eq!(queue.starved(), STARVE_BLOCKS - 1);
    }

    #[test]
    fn an_empty_pool_is_silence_without_counting_an_underrun() {
        let mut mixer = Mixer::default();
        let mut pool = SourcePool::default();
        let (out, outcome) = render(&mut mixer, &mut pool);
        assert!(out.iter().all(|sample| *sample == 0));
        assert_eq!(outcome.sources, 0);
        assert_eq!(mixer.stats().underruns(), 0);
        assert_eq!(mixer.stats().blocks(), 1);
    }

    #[test]
    fn the_source_cap_bounds_the_mixer() {
        let mut mixer = Mixer::default();
        for index in 0..80 {
            mixer.set_source(&format!("source-{index}"), Source::default());
        }
        assert_eq!(mixer.sources.len(), 64);
        let mut pool = SourcePool::default();
        for index in 0..80 {
            pool.ensure(&format!("source-{index}"));
        }
        assert_eq!(pool.len(), 64);
    }

    /// The release parameter is a documented profile, not an unexplained
    /// constant: measure how long a deep duck takes to recover to 99 %.
    #[test]
    fn the_limiter_release_profile_is_measured() {
        let mut limiter = Limiter::new();
        let mut hot = vec![2.0_f64; BLOCK_SAMPLES];
        assert!(limiter.process(&mut hot).0, "2.0 hits the ceiling");
        let start = limiter.gain();
        let mut blocks = 0_u32;
        while limiter.gain() < 0.99 && blocks < 10_000 {
            let mut quiet = vec![0.0_f64; BLOCK_SAMPLES];
            let (limited, released) = limiter.process(&mut quiet);
            assert!(!limited);
            assert!(released);
            blocks += 1;
        }
        let millis = blocks * 10;
        println!(
            "limiter release {start:.3} -> {:.3}: {blocks} blocks / {millis} ms",
            limiter.gain()
        );
        assert!(
            (1_800..=2_200).contains(&millis),
            "release profile is {millis} ms, expected about 2 s"
        );
        // The same profile must hold for every block after the first release.
        assert!(limiter.gain() >= 0.99);
    }

    /// Publish the actual block-time profile of this host. It is the number the
    /// journal cites; it is not a measurement of a physical sink.
    #[test]
    fn the_mix_block_profile_is_measured() {
        let a = tone(400.0, 8192.0, 0.0, BLOCK_SAMPLES);
        let b = tone(800.0, 4096.0, 0.7, BLOCK_SAMPLES);
        let mut mixer = Mixer::default();
        mixer.set_source("a", Source::default());
        mixer.set_source("b", Source::default());
        let mut out = vec![0_i16; BLOCK_SAMPLES];
        for _ in 0..1_000 {
            let mut pool = SourcePool::default();
            pool.push("a", a.clone());
            pool.push("b", b.clone());
            let started = std::time::Instant::now();
            mixer.render(Bus::Voice, &mut pool, &mut out);
            mixer
                .stats_mut()
                .record_latency(started.elapsed().as_micros() as u32);
        }
        let p50 = mixer.stats().p50_micros();
        let p95 = mixer.stats().p95_micros();
        println!(
            "mix profile: {} blocks, p50 {p50} us, p95 {p95} us, limiter releases {}",
            mixer.stats().blocks(),
            mixer.limiter_releases()
        );
        assert_eq!(mixer.stats().blocks(), 1_000);
        assert!(
            p95 < 2_000,
            "one 10 ms block cannot take longer than the quantum: {p95} us"
        );
    }
}
