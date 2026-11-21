//! Local-preview request and its capture-thread pacing.
//!
//! Split from `screen_capture.rs` so the capture module keeps one concern per
//! file after O02 (demand) and O03 (conversion) landed together.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{MAX_PREVIEW_WIDTH, PreviewFrame};

/// The local preview is prepared at most this often while the card is shown.
pub const PREVIEW_INTERVAL: Duration = Duration::from_millis(100);

/// Whether the local preview is wanted and how wide it should be.
///
/// Shared by the on-air card (the only writer of the request), the voice
/// runtime and the capture thread. Every field is an atomic: the UI thread
/// never waits for a worker, and the capture callback reads the newest value
/// without a lock. The counters are diagnostic only and are read through
/// `VoiceHandle::screen_preview_stats`; they cover the handle lifetime and are
/// not reset when one publication replaces another.
#[derive(Default)]
pub struct PreviewDemand {
    enabled: AtomicBool,
    max_width: AtomicU32,
    prepared: AtomicU64,
    shown: AtomicU64,
}

impl PreviewDemand {
    /// The card is painted at `width` physical pixels. The request is clamped
    /// to the same bound the store enforces, so a caller cannot grow it.
    pub fn request(&self, width: u32) {
        self.max_width
            .store(width.clamp(2, MAX_PREVIEW_WIDTH), Ordering::Relaxed);
        self.enabled.store(true, Ordering::Release);
    }

    /// The card is hidden, minimized, Idle or gone. The capture keeps
    /// publishing video; only the local preview conversion stops.
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// The requested preview width. Meaningful only while enabled.
    pub fn width(&self) -> u32 {
        self.max_width.load(Ordering::Relaxed)
    }

    /// Previews really converted into a `PreviewFrame`.
    pub fn prepared(&self) -> u64 {
        self.prepared.load(Ordering::Relaxed)
    }

    /// Previews really taken by the on-air card.
    pub fn shown(&self) -> u64 {
        self.shown.load(Ordering::Relaxed)
    }

    /// Count a converted preview. Wraps the conversion so the capture callback
    /// cannot publish an uncounted picture.
    pub(crate) fn published_preview(&self, frame: Option<PreviewFrame>) -> Option<PreviewFrame> {
        if frame.is_some() {
            self.prepared.fetch_add(1, Ordering::Relaxed);
        }
        frame
    }

    /// Count one preview handed to the card.
    pub(crate) fn mark_shown(&self) {
        self.shown.fetch_add(1, Ordering::Relaxed);
    }
}

/// Capture-thread pacing of the local preview.
///
/// The clock lives here, not in [`PreviewDemand`]: the shared demand is written
/// by the UI thread and must stay a plain atomic set. One throttle belongs to
/// one capture thread and is never shared.
#[derive(Default)]
pub struct PreviewThrottle {
    last: Option<Instant>,
}

impl PreviewThrottle {
    /// `Some(width)` when this frame should prepare a preview; `None` when the
    /// demand is off or the previous one is younger than [`PREVIEW_INTERVAL`].
    /// A disabled demand also clears the clock, so showing the card again
    /// prepares the first frame immediately instead of waiting out the window.
    pub fn poll(&mut self, now: Instant, demand: &PreviewDemand) -> Option<u32> {
        if !demand.is_enabled() {
            self.last = None;
            return None;
        }
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < PREVIEW_INTERVAL)
        {
            return None;
        }
        self.last = Some(now);
        Some(demand.width().clamp(2, MAX_PREVIEW_WIDTH))
    }
}
