//! Window lifecycle: the close path, the realtime teardown and the history
//! list factory. They live here so `mod.rs` stays focused on the view state and
//! the constructor.

use super::*;

impl Drop for Voxhold {
    fn drop(&mut self) {
        // A logout cleanup that is still queued must reach a result before the
        // window disappears; otherwise the next launch could restore the
        // session the user just ended.
        self.finish_pending_cleanup();
        // The window is going away, so the debounced voice save will never get
        // its timer: write the newest snapshot now (atomic, one small file).
        self.voice_settings.flush();
        // Closing the window ends the local work but is not a logout: no
        // server revocation is sent, and the session is simply dropped. The
        // sends and timers that belong to the window are cancelled with it.
        for (_, pending) in self.pending_sends.drain() {
            pending.abort.abort();
        }
        self.unconfirmed_sends.clear();
        self.stop_session_clock();
        self.stop_realtime();
    }
}

impl Voxhold {
    pub(super) fn stop_realtime(&mut self) {
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        self.events = None;
        self.event_subscription = None;
        self.realtime_generation = self.realtime_generation.wrapping_add(1);
        // The retry status belongs to the socket that just ended. Replacing the
        // shared link drops the old task's handle: its later writes cannot
        // repaint the banner in a new session.
        self.realtime_link = None;
        self.realtime_reconnecting = false;
        self.online_by_server.clear();
    }

    pub(super) fn new_history_list(alignment: ListAlignment, cx: &Context<Self>) -> ListState {
        let history_list = ListState::new(0, alignment, px(400.));
        let weak = cx.entity().downgrade();
        history_list.set_scroll_handler(move |_, window, cx| {
            if let Some(view) = weak.upgrade() {
                view.update(cx, |s, cx| {
                    // The list is mutably borrowed while this callback runs, so
                    // never touch `ListState` here. Both follow-ups recompute
                    // their range from a later frame.
                    s.scroll_epoch = s.scroll_epoch.wrapping_add(1);
                    if !s.loading_older && !s.older_scheduled && s.has_more {
                        s.schedule_older_check(window, cx);
                    }
                });
            }
        });
        history_list
    }
}
