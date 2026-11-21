//! Local on-air preview: the card's demand and the one picture it takes.
//!
//! The demand is a set of atomics shared with the capture thread, so the UI
//! never blocks on a worker and the capture callback reads the newest request
//! without a lock. Counting a frame as shown happens here, where the card
//! really takes it.

use super::*;

/// One preview picture for the local on-air card.
pub struct ScreenFrame {
    pub width: u32,
    pub height: u32,
    /// BGRA bytes; the panel hands them to `RenderImage` directly.
    pub bgra: Vec<u8>,
}

impl VoiceHandle {
    /// Ask the capture thread for a local preview `width` physical pixels wide,
    /// or stop asking with `None`. Only atomics are written: this is safe to
    /// call from the UI thread on every repaint and never reaches the worker
    /// as a command. The published stream is not affected in any way.
    pub fn set_screen_preview_demand(&self, width: Option<u32>) {
        match width {
            Some(width) => self.screen_demand.request(width),
            None => self.screen_demand.disable(),
        }
    }

    /// `(prepared, shown)` preview counters of this attempt. `prepared` counts
    /// converted pictures, `shown` counts pictures the card really took.
    pub fn screen_preview_stats(&self) -> (u64, u64) {
        (self.screen_demand.prepared(), self.screen_demand.shown())
    }

    /// The newest local preview; only the on-air card reads it. Taking a frame
    /// counts it as shown, including the case where the caller discards it
    /// because the card is no longer painted.
    pub fn take_screen_frame(&self) -> Option<ScreenFrame> {
        let frame = self.screen_preview.take()?;
        self.screen_demand.mark_shown();
        Some(ScreenFrame {
            width: frame.width,
            height: frame.height,
            bgra: frame.bgra,
        })
    }
}
