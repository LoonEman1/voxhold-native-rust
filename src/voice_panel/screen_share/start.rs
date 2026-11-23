//! Start and stop intents of the screen publication.
//!
//! Kept beside the state in `screen_share.rs`: both read the same gate,
//! selection and confirmed restart source; this file only turns them into one
//! publish or stop command.

use super::*;

impl VoicePanel {
    pub(in crate::voice_panel) fn start_screen_share(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.screen.busy {
            return;
        }
        if self.screen.audio_availability.is_none() {
            self.screen.error = AUDIO_PENDING_REASON.into();
            cx.notify();
            return;
        }
        if let Err(reason) = self.share_gate() {
            // The live quality dialog is allowed to pass the "already live"
            // gate: it replaces the current attempt only through a confirmed
            // stop+start. Every other gate (join, permission, slot, capture)
            // still blocks it.
            let restart_bypass = self.screen.restart && reason == "Трансляция уже идёт.";
            if !restart_bypass {
                // An active publication is not an error for this button; its
                // card owns the status and the stop action.
                if !self.share_active() {
                    self.screen.error = reason.to_owned();
                }
                cx.notify();
                return;
            }
        }
        let Some(id) = self.screen.selected else {
            self.screen.error = "Выберите монитор или окно.".into();
            cx.notify();
            return;
        };
        let source = if self.screen.restart {
            // The restart is locked to the source the engine already accepted.
            // The current snapshot may be empty, stale or describe a renamed
            // row; none of that may move the restart to another source.
            if self.screen.sharing.phase != Phase::Live {
                self.screen.error = "Трансляция уже не идёт; откройте выбор экрана заново.".into();
                self.screen.open = false;
                self.screen.restart = false;
                self.screen.focus_placed = false;
                self.screen.clear_previews(window);
                cx.notify();
                return;
            }
            let Some(live) = self.screen.restart_source().cloned() else {
                self.screen.error = SOURCE_GONE_REASON.into();
                cx.notify();
                return;
            };
            if live.source_id() != id || self.screen.selected_identity != live.identity() {
                self.screen.error = SOURCE_GONE_REASON.into();
                cx.notify();
                return;
            }
            live
        } else {
            // The dialog may be stale: build the request from the row that
            // exists now, and refuse an absent, duplicated or re-used id.
            let source = self
                .screen
                .source_for(id)
                .filter(|_| id.kind == self.screen.kind)
                .cloned();
            let Some(source) = source else {
                self.screen.error = SOURCE_GONE_REASON.into();
                cx.notify();
                return;
            };
            if source.identity().is_none() || !self.screen.selection_matches(&source) {
                self.screen.error = SOURCE_CHANGED_REASON.into();
                cx.notify();
                return;
            }
            source
        };
        let codec = match self.codec_resolution() {
            Ok(codec) => codec,
            Err(reason) => {
                self.screen.error = reason;
                cx.notify();
                return;
            }
        };
        let quality = self.screen.quality_for_source(&source);
        let request = PublishRequest {
            source,
            quality,
            codec,
            audio: self.screen.audio_requested(),
            audio_scope: self.screen.effective_audio_scope(
                self.screen
                    .selected
                    .map(|id| id.kind)
                    .unwrap_or(self.screen.kind),
            ),
        };
        // The live dialog may resolve back to the payload that is already on
        // air (for example «Автоматически» after an explicit codec). Nothing
        // has to be stopped or restarted then: no preflight, no stop, no start,
        // and the dialog simply closes. Only the preference may have changed.
        if self.screen.restart
            && self.screen.restart_change_to(&request) == Some(QualityChange::Unchanged)
        {
            self.remember_live_preferences(cx);
            self.close_screen_share(window, cx);
            return;
        }
        let Some(handle) = &self.handle else {
            self.screen.error = "Голосовое соединение уже закрыто.".into();
            cx.notify();
            return;
        };
        let slot_available = self.active_publishers_in_room() < MAX_CHANNEL_PUBLISHERS;
        // Classify with the exact request above, including the codec Start just
        // resolved. A codec or profile change is a restart; only the bitrates
        // reach the live sender.
        let live_bitrate = self.screen.restart
            && self.screen.restart_change_to(&request) == Some(QualityChange::LiveBitrate);
        // Every quality change gets its own tag, so a delayed answer of an
        // older Apply cannot clear the busy state of this one.
        let quality_op = if self.screen.restart {
            self.screen.quality_op = self.screen.quality_op.wrapping_add(1);
            Some(self.screen.quality_op)
        } else {
            None
        };
        if let Some(op) = quality_op {
            // A bitrate-only change goes to the live sender; the worker
            // repeats the classification before it applies anything.
            if live_bitrate {
                handle.apply_screen_quality(op, request.clone(), self.can_stream, slot_available);
            } else {
                handle.restart_screen_share(op, request.clone(), self.can_stream, slot_available);
            }
        } else {
            handle.publish_screen(
                self.screen.picker,
                request.clone(),
                self.can_stream,
                slot_available,
            );
        }
        if self.screen.restart {
            self.stage_live_preferences(request);
        } else {
            self.remember_stream_preferences(cx);
        }
        // The request becomes confirmed when the media layer reports Live.
        self.screen.busy = true;
        // A restart without a live bitrate starts a preflight; closing the
        // dialog while it runs must cancel it instead of stopping the stream.
        // The pending operation tag is the only confirmation this dialog needs.
        self.screen.preflight = self.screen.restart && !live_bitrate;
        self.screen.pending_quality_op = quality_op;
        // The previous attempt's error belongs to its own answer. A stale
        // `Sharing` snapshot must not turn this Start into an instant failure.
        self.screen.sharing.error = None;
        self.screen.error.clear();
        cx.notify();
    }

    pub(in crate::voice_panel) fn stop_screen_share(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = &self.handle {
            handle.stop_screen_share();
        }
        // A stop ends the picker session: no capture may start another card.
        self.screen.clear_previews(window);
        // The on-air card may already hold the last shown frame, and the phase
        // update can arrive coalesced with the stop. Release the picture here
        // as well instead of relying on that update.
        self.screen.clear_preview(window);
        cx.notify();
    }
}
