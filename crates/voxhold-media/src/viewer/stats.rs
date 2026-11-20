//! Frozen-picture detection for one viewing attempt.
//!
//! WebRTC can stay `connected` while the picture stops moving. The browser
//! samples inbound RTP every two seconds; the viewer follows that cadence and
//! compares cumulative counters instead of pixels:
//!
//! - decoded frames advanced: the picture is moving, reset recovery counters;
//! - RTP advanced but no frame was decoded: the decoder waits for a keyframe,
//!   so ask the server/SFU for one and leave ICE alone;
//! - no RTP at all while the transport is connected: use the existing ICE
//!   recovery path.
//!
//! A missing report is counted separately. It says nothing about the encoder
//! and must never be mistaken for a series of identical reports.

use super::*;

/// The browser samples inbound video statistics every two seconds. The guard
/// uses the same cadence so one slow decode does not start recovery.
///
/// These thresholds still need validation on real sources: a mostly static
/// screen may legitimately skip frames for several seconds, and an encoder may
/// stop sending RTP entirely while nothing changes.
pub const STALL_WINDOW: Duration = Duration::from_secs(2);
/// Consecutive sampled windows without a decoded frame before the picture is
/// treated as frozen. Two windows (four seconds) tolerate ordinary static
/// content while still recovering a genuinely stuck decoder quickly.
pub const STALL_WINDOWS: u8 = 2;
/// Server/SFU keyframe requests before the same publication is watched again.
/// A lost keyframe is cheap to repair; re-watching resets the whole peer.
pub const MAX_KEYFRAME_REQUESTS: u8 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Sample {
    bytes: u64,
    frames: u32,
}

/// Counters of the engine installed for the active attempt.
///
/// `baseline` is the value at the previous 2 s evaluation; `latest` is the
/// newest report. `reports` counts reports since that evaluation, so a poller
/// that returns nothing is not confused with a poller returning the same
/// cumulative counters.
#[derive(Clone, Debug, Default)]
pub(super) struct StatsWatch {
    baseline: Option<Sample>,
    pub(super) latest: Option<Sample>,
    next: Option<Instant>,
    pub(super) reports: u32,
    pub(super) idle_windows: u8,
    pub(super) missing_windows: u8,
    keyframes: u8,
}

impl StatsWatch {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            next: Some(now + STALL_WINDOW),
            ..Self::default()
        }
    }

    /// A decoded frame proves that the decoder is alive; the next window still
    /// has to show progress before the reconnect budget is restored.
    pub(super) fn on_decoded(&mut self) {
        self.idle_windows = 0;
        self.keyframes = 0;
    }

    fn observe(&mut self, bytes: u64, frames: u32, now: Instant) {
        let sample = Sample { bytes, frames };
        if self.baseline.is_none() {
            self.baseline = Some(sample);
        }
        self.latest = Some(sample);
        self.reports = self.reports.saturating_add(1);
        if self.next.is_none() {
            self.next = Some(now + STALL_WINDOW);
        }
    }
}

/// What one evaluated stats window showed, if anything.
enum Slow {
    /// RTP advanced, decoded frames did not.
    Decoder,
    /// Neither RTP nor decoded frames advanced.
    Transport,
}

impl Viewer {
    /// One inbound video stats report of a viewing attempt.
    ///
    /// The attempt id is stamped by the engine that produced the report. A
    /// report of an engine that was already replaced is ignored even when the
    /// selection id did not change (a reconnect to the same publication).
    pub fn on_video_stats(
        &mut self,
        attempt: u64,
        bytes_received: u64,
        frames_decoded: u32,
        now: Instant,
    ) -> Vec<Outcome> {
        let Some(active) = &mut self.active else {
            return Vec::new();
        };
        if active.attempt != attempt {
            return Vec::new();
        }
        active.stats.observe(bytes_received, frames_decoded, now);
        Vec::new()
    }

    /// Evaluate one two-second window. Called from `tick`, so the state machine
    /// never reads the clock itself.
    pub(super) fn poll_stats(&mut self, now: Instant) -> Vec<Outcome> {
        let slow = {
            let Some(active) = &mut self.active else {
                return Vec::new();
            };
            let watch = &mut active.stats;
            let Some(next) = watch.next else {
                watch.next = Some(now + STALL_WINDOW);
                return Vec::new();
            };
            if now < next {
                return Vec::new();
            }
            watch.next = Some(now + STALL_WINDOW);
            if watch.reports == 0 {
                watch.missing_windows = watch.missing_windows.saturating_add(1);
                return Vec::new();
            }
            watch.reports = 0;
            watch.missing_windows = 0;
            let Some(sample) = watch.latest else {
                return Vec::new();
            };
            let Some(baseline) = watch.baseline.replace(sample) else {
                return Vec::new();
            };
            if sample.frames > baseline.frames {
                // A whole window of decoded frames is the healthy signal. Only
                // this restores the reconnect budget, so one frame after a
                // reconnect cannot hide a picture that freezes again.
                watch.idle_windows = 0;
                watch.keyframes = 0;
                self.retries = 0;
                return Vec::new();
            }
            if !active.connected || active.recovering || self.deadline.is_some() {
                // Before the first picture or during an existing recovery the
                // start/recovery deadline owns the attempt.
                return Vec::new();
            }
            watch.idle_windows = watch.idle_windows.saturating_add(1);
            if watch.idle_windows < STALL_WINDOWS {
                return Vec::new();
            }
            watch.idle_windows = 0;
            Some(if sample.bytes > baseline.bytes {
                Slow::Decoder
            } else {
                Slow::Transport
            })
        };
        match slow {
            Some(Slow::Decoder) => self.recover_picture(now),
            Some(Slow::Transport) => self.on_engine_state("disconnected", now),
            None => Vec::new(),
        }
    }

    /// A frozen picture whose transport still delivers RTP: ask for a keyframe
    /// in server mode. After the bounded keyframe budget the same publication is
    /// watched again. P2P has no keyframe gate, so its existing restart signal
    /// is the recovery.
    fn recover_picture(&mut self, now: Instant) -> Vec<Outcome> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let selection = active.selection;
        let p2p = active.publication.mode == "p2p";
        let server = active.publication.server_id;
        let channel = active.publication.channel_id;
        let keyframes = active.stats.keyframes;
        if p2p {
            return self.on_engine_state("disconnected", now);
        }
        if keyframes < MAX_KEYFRAME_REQUESTS {
            if let Some(active) = &mut self.active {
                active.stats.keyframes += 1;
            }
            let signal = self.signal(
                "stream.recovery_request",
                json!({
                    "server_id": server,
                    "channel_id": channel,
                    "action": "keyframe",
                }),
            );
            return vec![
                Outcome::Status {
                    selection,
                    text: "Восстанавливаем изображение…".into(),
                },
                Outcome::Send(signal),
            ];
        }
        // The keyframe requests did not reach the encoder or did not help.
        // Re-watch the exact same publication; the selection id may stay the
        // same, so the attempt (and its fresh stats) is rebuilt from `intent`.
        if self.retries >= MAX_RETRIES {
            return self.fail("Не удалось восстановить изображение", now);
        }
        self.retries += 1;
        let mut out = vec![
            Outcome::Status {
                selection,
                text: "Восстанавливаем трансляцию…".into(),
            },
            Outcome::Connected {
                selection,
                connected: false,
            },
        ];
        out.extend(self.close_active(now));
        out
    }
}
