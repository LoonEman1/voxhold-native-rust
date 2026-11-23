//! Event-driven work of the voice panel.
//!
//! The panel used to poll every 100 ms for its whole lifetime. Now three tasks
//! cover three separate reasons to run, and two of them are real deadlines:
//!
//! - [`spawn`] waits on the panel's shared wake and polls only when the media
//!   runtime published something: an update (speaking level, reconnect, a
//!   stream error, a screen snapshot), a prepared player picture or a local
//!   preview. In an idle call the task sleeps and the panel never polls.
//! - [`spawn_clock`] is the one-second live-clock tick. It polls only while a
//!   painted card shows a running stream or a call exists (a handle or a call
//!   channel); after Leave/Stop or with an empty room it keeps sleeping
//!   without polling. A watcher who is not in the call still needs this tick,
//!   because the room list keeps the M:SS label of a foreign stream alive, but
//!   only while the window is not minimized: a hidden label needs no repaint,
//!   and activation wakes the panel for the current second.
//! - [`spawn_frames`] is the ten-frames-per-second player deadline. It exists
//!   only between `watch` and `close_player` and never polls while the window
//!   is minimized; activating the window wakes the event task again.
//!
//! Every task ends when the panel entity is gone or an update fails, so a
//! closed window cannot leave a task polling a freed entity. The clock and the
//! player task have their own deadlines, but the event task has no timer: an
//! idle panel that is released wakes it once through [`VoicePanel::drop`], it
//! sees the failed upgrade and ends.

use super::*;
use std::time::{Duration, Instant};
use voxhold_media::runtime::StreamFrame;

/// The live clock and the fallback tick of an active call.
pub(super) const CLOCK_INTERVAL: Duration = Duration::from_secs(1);
/// The player frame deadline. It caps the redraw at the previous ~10 frames/s,
/// whatever rate the decoder and the update queue produce.
pub(super) const FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// The number of event tasks that have not finished. A production build
/// compiles it to a zero-sized value, so the task carries no extra state; a
/// test reads the counter after releasing the panel to prove that nothing is
/// left parked.
#[derive(Clone, Default)]
pub(super) struct EventTasks {
    #[cfg(test)]
    count: Arc<std::sync::atomic::AtomicUsize>,
}

impl EventTasks {
    /// Enter one event task. The guard counts it until the loop ends or the
    /// executor drops the future.
    fn enter(&self) -> EventTaskGuard {
        #[cfg(test)]
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        EventTaskGuard {
            #[cfg(test)]
            count: self.count.clone(),
        }
    }

    /// Handle to the live count that stays readable after the panel is gone.
    #[cfg(test)]
    pub(super) fn running(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.count.clone()
    }
}

/// Lives exactly as long as one event task.
struct EventTaskGuard {
    #[cfg(test)]
    count: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl Drop for EventTaskGuard {
    fn drop(&mut self) {
        self.count
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Wait for the media runtime's wake, then poll once.
pub(super) fn spawn(
    wake: Arc<Notify>,
    tasks: EventTasks,
    window: &mut Window,
    cx: &mut Context<VoicePanel>,
) {
    cx.spawn_in(window, async move |this, cx| {
        let _live = tasks.enter();
        loop {
            // Await first: the wake keeps a permit when it fires before this
            // future is polled, so a push between two polls is never lost.
            wake.notified().await;
            let Some(entity) = this.upgrade() else { break };
            if entity
                .update_in(cx, |panel, window, cx| panel.poll(window, cx))
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
}

/// The one-second live clock. A call without a handle still ticks the clock,
/// and so does a painted room list with a running stream; an idle panel does
/// not poll at all, and a minimized window drops the stream reason.
pub(super) fn spawn_clock(window: &mut Window, cx: &mut Context<VoicePanel>) {
    let executor = cx.background_executor().clone();
    cx.spawn_in(window, async move |this, cx| loop {
        executor.timer(CLOCK_INTERVAL).await;
        let Some(entity) = this.upgrade() else { break };
        let updated = entity.update_in(cx, |panel, window, cx| {
            if panel.live_clock_due(window) {
                panel.poll(window, cx);
            }
        });
        if updated.is_err() {
            break;
        }
    })
    .detach();
}

impl Drop for VoicePanel {
    fn drop(&mut self) {
        // The event task has no timer. Without this wake a panel that ends in
        // idle would leave the task parked for the rest of the app lifetime.
        // The woken task upgrades its weak handle, sees `None` and ends; it
        // never touches the freed panel. `notify_one` neither blocks nor
        // panics.
        self.wake.notify_one();
    }
}

/// The player frame deadline. `epoch` belongs to one `watch`; closing the
/// player moves the epoch, and the old task stops at its next tick.
pub(super) fn spawn_frames(epoch: u64, window: &mut Window, cx: &mut Context<VoicePanel>) {
    let executor = cx.background_executor().clone();
    cx.spawn_in(window, async move |this, cx| loop {
        executor.timer(FRAME_INTERVAL).await;
        let Some(entity) = this.upgrade() else { break };
        let updated = entity.update_in(cx, |panel, window, cx| {
            if panel.frame_ticker != epoch || panel.watching_selection.is_none() {
                return false;
            }
            // A minimized window keeps the player state but draws no picture:
            // the deadline takes nothing until the window is activated.
            if !panel.window_minimized(window) {
                panel.poll(window, cx);
            }
            true
        });
        if !matches!(updated, Ok(true)) {
            break;
        }
    })
    .detach();
}

impl VoicePanel {
    /// Window activation: an activated window resumes the player deadline at
    /// once, a deactivated one still releases a held push-to-talk key.
    pub(super) fn window_activation(&mut self, active: bool) {
        if active {
            self.wake.notify_one();
        } else {
            self.release_push_to_talk();
        }
    }

    /// One poll, serialized by the GPUI entity update. The event task, the
    /// clock and the frame deadline all call this; no two polls run at once
    /// on the UI thread, so one update is applied and one picture taken.
    pub(super) fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.polls = self.polls.saturating_add(1);
        }
        self.sync_pip(window);
        self.sync_viewport();
        let clock_changed = self.live_clock_changed();
        if self.handle.is_none() {
            if clock_changed {
                cx.notify();
            }
            return;
        }
        let mut changed = clock_changed;
        let mut updates = Vec::new();
        while let Some(update) = self.handle.as_ref().and_then(VoiceHandle::try_update) {
            updates.push(update);
        }
        // The frame deadline is measured on the executor clock, so the test
        // dispatcher advances it with its fake timers.
        let frame = self.take_frame_if_due(window, cx.background_executor().now());
        for update in updates {
            changed |= self.apply_update(update, window, cx);
        }
        self.watch_after_join(window, cx);
        if let Some(frame) = frame {
            if self.watching_selection != Some(frame.selection) {
                // A frame that was already in the last-value slot when the
                // user switched selections belongs to the old player.
            } else if let Some(buffer) =
                image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
            {
                let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
                    buffer
                )]));
                self.set_player_frame(window, image);
                changed = true;
            }
        }
        // The demand follows the phase this poll just applied, not the one the
        // previous frame painted. A Stop answer clears the card in the same
        // tick; syncing before the updates would ask for one more capture
        // interval of a preview nobody shows.
        self.sync_screen_preview_demand(window);
        changed |= self.take_screen_preview(window);
        if changed {
            cx.notify();
        }
    }

    /// Take one prepared player picture if its deadline passed. `take_frame`
    /// also asks for exactly one next picture, so the conversion rate follows
    /// this deadline rather than the decoder.
    fn take_frame_if_due(&mut self, window: &Window, now: Instant) -> Option<StreamFrame> {
        if self.watching_selection.is_none() || self.window_minimized(window) {
            return None;
        }
        if self
            .last_frame_at
            .is_some_and(|last| now.saturating_duration_since(last) < FRAME_INTERVAL)
        {
            return None;
        }
        let frame = self.handle.as_ref()?.take_frame();
        if frame.is_some() {
            self.last_frame_at = Some(now);
        }
        frame
    }

    /// Whether the window is minimized. The real answer needs a platform
    /// handle that the GPUI test window does not have, so tests may override
    /// it; production always asks [`crate::window_state::is_minimized`].
    pub(super) fn window_minimized(&self, window: &Window) -> bool {
        #[cfg(test)]
        if let Some(minimized) = self.test_minimized {
            return minimized;
        }
        crate::window_state::is_minimized(window)
    }
}
