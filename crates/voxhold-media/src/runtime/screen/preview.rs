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

    /// `(prepared, shown)` preview counters since this handle was created.
    /// `prepared` counts converted pictures, `shown` counts pictures the card
    /// really painted. The counters are not reset between attempts: the same
    /// handle reuses one capture-side demand, and a late callback of a stopped
    /// attempt would make a reset of the shared atomics incorrect.
    pub fn screen_preview_stats(&self) -> (u64, u64) {
        (self.screen_demand.prepared(), self.screen_demand.shown())
    }

    /// The newest local preview; only the on-air card reads it. Taking a frame
    /// counts it as shown.
    pub fn take_screen_frame(&self) -> Option<ScreenFrame> {
        let frame = self.screen_preview.take()?;
        self.screen_demand.mark_shown();
        Some(ScreenFrame {
            width: frame.width,
            height: frame.height,
            bgra: frame.bgra,
        })
    }

    /// Drop any pending preview without counting it as shown. The card may
    /// stop painting between the capture tick and this poll; the slot is
    /// emptied anyway so a stale picture cannot reappear later, but such a
    /// frame was never shown to the user.
    pub fn discard_screen_frame(&self) -> bool {
        self.screen_preview.take().is_some()
    }
}
