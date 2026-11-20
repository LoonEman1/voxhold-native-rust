//! Frozen-picture detection for one viewing attempt.
//!
//! The browser reference (`mediaRecovery.ts`, `DecodeWatchdog`) samples inbound
//! RTP every two seconds and compares cumulative counters instead of pixels.
//! The viewer follows the same rules:
//!
//! - decoded frames advanced: the picture is moving, the window is healthy and
//!   only such a confirmed window restores the reconnect budget;
//! - RTP advanced but no frame was decoded: the decoder waits for a keyframe,
//!   so ask the server/SFU for one and leave ICE alone;
//! - neither counter advanced while the transport is connected and a frame was
//!   already decoded: the screen is static. Keep the last picture, reset the
//!   idle counters and do nothing else. Screen capture legitimately stops
//!   producing RTP on an unchanged picture, so RTP silence while the peer
//!   reports `connected` is not evidence of a transport failure and must never
//!   restart ICE or re-watch a valid view.
//!
//! A real transport loss is detected by the PeerConnection state through
//! `on_engine_state("disconnected"/"failed")`, which owns the ICE/P2P recovery
//! path. Statistics only distinguish a moving picture, a stuck decoder and a
//! static screen.
//!
//! A missing report is counted separately. It says nothing about the encoder
//! and must never be mistaken for a series of identical reports; after a
//! bounded number of consecutive missing windows it does request the existing
//! ICE recovery, because the stats source itself stopped answering.

use super::*;

/// The browser samples inbound video statistics every two seconds. The guard
/// uses the same cadence so one slow decode does not start recovery.
///
/// These thresholds still need validation on real sources: a mostly static
/// screen may legitimately skip frames for several seconds, an encoder may
/// stop sending RTP entirely while nothing changes, and a busy machine may
/// miss a stats sample or two.
pub const STALL_WINDOW: Duration = Duration::from_secs(2);
/// Consecutive sampled windows without a decoded frame before the picture is
/// treated as frozen. Two windows (four seconds) tolerate ordinary static
/// content while still recovering a genuinely stuck decoder quickly.
pub const STALL_WINDOWS: u8 = 2;
/// Server/SFU keyframe requests before the same publication is watched again.
/// A lost keyframe is cheap to repair; re-watching resets the whole peer.
pub const MAX_KEYFRAME_REQUESTS: u8 = 2;
/// Consecutive sampled windows without any report before the stats source is
/// treated as dead. The browser uses `bytesStallSamples + 2` = 6 samples
/// (twelve seconds at the two-second cadence).
pub const MISSING_WINDOWS: u8 = 6;

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

/// Which existing recovery a bounded stats window asks for.
enum Recover {
    /// RTP advanced, decoded frames did not: a stuck decoder.
    Picture,
    /// The stats source produced no report for a bounded number of windows.
    Missing,
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
        let recover = {
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
            // During a start or an existing recovery its deadline owns the
            // attempt; statistics must neither confirm nor restart it.
            let connected = active.connected && !active.recovering && self.deadline.is_none();
            if watch.reports == 0 {
                watch.missing_windows = watch.missing_windows.saturating_add(1);
                if connected && watch.missing_windows >= MISSING_WINDOWS {
                    watch.missing_windows = 0;
                    Some(Recover::Missing)
                } else {
                    None
                }
            } else {
                watch.reports = 0;
                // The next evaluation always starts from the newest report;
                // a missing window reset this counter instead.
                watch.missing_windows = 0;
                let Some(sample) = watch.latest else {
                    return Vec::new();
                };
                let Some(baseline) = watch.baseline.replace(sample) else {
                    return Vec::new();
                };
                if sample.frames > baseline.frames {
                    // A whole window of decoded frames is the healthy signal.
                    // Only this restores the reconnect budget, so advancing
                    // counters that never present a frame cannot hide a stuck
                    // decoder forever.
                    watch.idle_windows = 0;
                    watch.keyframes = 0;
                    if connected {
                        self.retries = 0;
                    }
                    None
                } else if !connected {
                    None
                } else if baseline.frames > 0 && sample.bytes == baseline.bytes {
                    // Static connected picture: an unchanged screen legally
                    // stops producing RTP. Reset the window counters, keep the
                    // last picture and leave keyframe/ICE/re-watch alone.
                    watch.idle_windows = 0;
                    None
                } else {
                    watch.idle_windows = watch.idle_windows.saturating_add(1);
                    if watch.idle_windows < STALL_WINDOWS {
                        None
                    } else {
                        watch.idle_windows = 0;
                        Some(Recover::Picture)
                    }
                }
            }
        };
        match recover {
            Some(Recover::Picture) => self.recover_picture(now),
            Some(Recover::Missing) => self.on_engine_state("disconnected", now),
            None => Vec::new(),
        }
    }

    /// A frozen picture whose transport still delivers RTP: ask for a keyframe
    /// in server mode. After the bounded keyframe budget the same publication is
    /// watched again. P2P has no keyframe gate, so its existing restart signal
    /// is the recovery.
    ///
    /// The keyframe request id is tracked separately: a server that answers
    /// that request with an `error` rejects the keyframe, not the whole view,
    /// so the bounded recovery continues instead of ending the viewer.
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
            self.track_keyframe(signal.request_id.as_deref());
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
        out.extend(self.restart(now));
        out
    }
}
