use super::*;

impl VoicePanel {
    pub(crate) fn watch_from_sidebar(
        &mut self,
        publication: Publication,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.access_confirmed || publication.channel_id != self.room {
            return;
        }
        self.join(window, cx);
        if self.call_channel == self.room && self.handle.is_some() {
            self.sidebar_watch = Some(publication);
            if self.room_painted {
                self.watch_after_join(window, cx);
            }
        }
    }

    pub(super) fn watch_after_join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.access_confirmed
            || !self.room_painted
            || matches!(self.phase, CallPhase::Idle | CallPhase::Failed)
        {
            self.sidebar_watch = None;
            return;
        }
        if self.phase != CallPhase::Connected {
            return;
        }
        if let Some(pending) = &self.sidebar_watch {
            let publication = self
                .room_streams()
                .iter()
                .find(|item| item.publisher_connection_id == pending.publisher_connection_id)
                .cloned();
            if let Some(publication) = publication {
                self.watch(publication, window, cx);
            } else if !self
                .room_streams
                .iter()
                .any(|item| item.publisher_connection_id == pending.publisher_connection_id)
            {
                self.sidebar_watch = None;
            }
        }
    }

    /// Whether a painted room card shows a running stream. Such a card owns a
    /// live `M:SS` label even when this client is not in the call.
    fn room_stream_clock(&self) -> bool {
        self.room_painted
            && self
                .room_streams()
                .iter()
                .any(|item| item.started_at_ms > 0)
    }

    /// Whether the one-second tick has a reason to poll: a call, an open
    /// handle, or a painted room list with a running foreign stream. Without
    /// any of them an idle panel must not poll.
    pub(super) fn live_clock_due(&self) -> bool {
        self.call_active() || self.handle.is_some() || self.room_stream_clock()
    }

    pub(super) fn live_clock_changed(&mut self) -> bool {
        let next = self
            .room_stream_clock()
            .then(|| voice_room::now_millis() / 1000);
        let changed = self.live_clock_second != next;
        self.live_clock_second = next;
        changed
    }
}
