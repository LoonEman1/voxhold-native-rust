//! Application of coalesced screen-share updates.
//!
//! The queue keeps only the newest `ScreenShare` snapshot, so a fast preflight
//! failure can arrive without its intermediate `preflighting` sample. The
//! dialog therefore binds `busy` to the operation tag it sent, never to an
//! observed intermediate state. A result of an older operation is ignored.

use super::*;

impl VoicePanel {
    /// Apply one runtime update. Returns whether the panel must repaint.
    pub(in crate::voice_panel) fn apply_screen_update(
        &mut self,
        update: VoiceUpdate,
        window: &mut Window,
    ) -> bool {
        match update {
            VoiceUpdate::ScreenShare(sharing) => {
                let previous = self.screen.sharing.phase;
                self.screen.sharing = sharing;
                let phase = self.screen.sharing.phase;
                let status = self.screen.sharing.quality_status.clone();
                let pending = self.screen.pending_quality_op;
                let current = status.clone().filter(|status| Some(status.op) == pending);

                // Only the result of the operation this dialog waits for may
                // change its busy state, its error or its visibility.
                let mut failed_current = false;
                if let Some(status) = current {
                    match status.outcome {
                        QualityOutcome::Preflighting => {
                            self.screen.preflight = self.screen.restart;
                            self.screen.busy = true;
                        }
                        QualityOutcome::Applying => {
                            // The command reached the engine, but the senders
                            // have not confirmed it. Keep the dialog open and
                            // busy; the confirmed values still describe the
                            // stream that is on air.
                            self.screen.busy = true;
                            self.screen.preflight = false;
                        }
                        QualityOutcome::Failed(message) => {
                            failed_current = true;
                            self.screen.busy = false;
                            self.screen.pending_quality_op = None;
                            self.screen.preflight = false;
                            self.screen.error = message;
                            // The refused change never replaced the on-air
                            // configuration, so the dialog returns to the
                            // confirmed values and the user may retry.
                            self.screen.restore_confirmed_quality();
                        }
                        QualityOutcome::Cancelled => {
                            self.screen.busy = false;
                            self.screen.pending_quality_op = None;
                            self.screen.preflight = false;
                            // A session or device loss keeps its reason in the
                            // snapshot; the dialog stays open and must explain
                            // it. `None` is the user closing the dialog, which
                            // adds no error.
                            if let Some(message) = self.screen.sharing.error.clone() {
                                self.screen.error = message;
                            }
                        }
                        QualityOutcome::Committed | QualityOutcome::Applied => {
                            self.screen.busy = false;
                            self.screen.pending_quality_op = None;
                            self.screen.preflight = false;
                            if self.screen.open {
                                self.close_screen_dialog(window);
                            }
                        }
                    }
                }
                let preflighting_current = self.screen.sharing.preflighting
                    && pending.is_some_and(|op| {
                        status.as_ref().is_some_and(|status| {
                            status.op == op && status.outcome == QualityOutcome::Preflighting
                        })
                    });
                if preflighting_current {
                    // The worker confirmed this dialog's preflight. Keep the
                    // cancel intent alive even when a stale state cleared
                    // `busy` in between.
                    self.screen.preflight = self.screen.restart;
                    self.screen.busy = true;
                }
                // The stop of a quality change is committed; the card now
                // shows the restart and the dialog must not allow a second
                // action. A stale restart snapshot of an older operation never
                // closes a newer dialog.
                let restarting_ours = self.screen.sharing.restarting
                    && self.screen.open
                    && pending
                        .is_none_or(|op| status.as_ref().is_some_and(|status| status.op == op));
                if restarting_ours {
                    self.close_screen_dialog(window);
                }
                if matches!(phase, Phase::Idle | Phase::Failed) {
                    self.screen.restart = false;
                    self.screen.preflight = false;
                    self.screen.pending_quality_op = None;
                }
                if matches!(phase, Phase::Publishing | Phase::Live)
                    && self.screen.open
                    && pending.is_none()
                    && !self.screen.sharing.preflighting
                    && !failed_current
                    && (self.screen.busy || previous != phase)
                {
                    // Either a normal Start committed the attempt or the phase
                    // really changed: the room card owns the status now.
                    self.close_screen_dialog(window);
                }
                if phase == Phase::Failed {
                    self.screen.busy = false;
                    self.screen.focus_placed = false;
                    self.screen.error = self.screen.sharing.error.clone().unwrap_or_default();
                    // A failed attempt has no picker any more; reopening the
                    // dialog calls `begin_picker` again.
                    if self.screen.open {
                        self.screen.open = false;
                        self.screen.clear_previews(window);
                    }
                }
                if phase == Phase::Idle {
                    if self.screen.open {
                        // The session ended (or a new one replaced the worker)
                        // while the dialog was open; its picker id is stale.
                        self.screen.open = false;
                        self.screen.busy = false;
                        self.screen.focus_placed = false;
                        self.screen.clear_previews(window);
                        if previous != Phase::Idle {
                            self.screen.error =
                                "Соединение было восстановлено, выберите экран снова.".into();
                        }
                    }
                    // Every Idle snapshot drops the on-air picture. The state
                    // updates are coalesced, so `previous` being Idle does not
                    // prove that no card is held: Stop may have released the
                    // thumbnails while this update still carried the card.
                    self.screen.clear_preview(window);
                }
                // The gate can change without a picker click: the attempt is
                // now open or closed, and the worker needs the current value.
                self.sync_screen_access();
                true
            }
            VoiceUpdate::ScreenSources {
                generation,
                kind,
                sources,
                error,
            } => {
                match kind {
                    SourceKind::Monitor if generation >= self.screen.monitor_generation => {
                        self.screen.monitor_generation = generation;
                        self.screen.monitor_loading = false;
                        self.screen.monitor_error = error.unwrap_or_default();
                        self.screen.retain_selection(SourceKind::Monitor, &sources);
                        self.screen
                            .retain_preview_snapshot(window, SourceKind::Monitor, &sources);
                        // The non-active kind still refreshes its cache; the
                        // picker reads it when the user switches tabs.
                        self.screen.monitors = sources;
                    }
                    SourceKind::Window if generation >= self.screen.window_generation => {
                        self.screen.window_generation = generation;
                        self.screen.window_loading = false;
                        self.screen.window_error = error.unwrap_or_default();
                        self.screen.retain_selection(SourceKind::Window, &sources);
                        self.screen
                            .retain_preview_snapshot(window, SourceKind::Window, &sources);
                        self.screen.windows = sources;
                    }
                    _ => return false,
                }
                true
            }
            _ => false,
        }
    }

    fn close_screen_dialog(&mut self, window: &mut Window) {
        self.screen.open = false;
        self.screen.restart = false;
        self.screen.busy = false;
        self.screen.preflight = false;
        self.screen.pending_quality_op = None;
        self.screen.focus_placed = false;
        self.screen.clear_previews(window);
    }
}
