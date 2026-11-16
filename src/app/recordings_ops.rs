//! Network and lifecycle operations for server recordings.
//!
//! The list, delete and ticket calls never run on the UI thread. Every answer
//! checks the server, the dialog generation and the access epoch before it
//! touches the snapshot, the busy row or the preview cache. Previews stop when
//! the dialog closes or the access generation changes.

use super::*;
use crate::recordings;
use voxhold_api::Recording;

pub(super) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

/// The full ownership token of one download or delete attempt. A callback may
/// touch `busy`, `confirm` or `error` only while every part still matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecordingOp {
    pub(super) attempt: u64,
    pub(super) dialog: u64,
    pub(super) server: i64,
    pub(super) session: u64,
    pub(super) epoch: u64,
    pub(super) id: String,
}

impl Voxhold {
    pub(super) fn next_recording_op(&mut self, id: &str) -> Option<RecordingOp> {
        let dialog = self.recordings_dialog.as_ref()?;
        self.recordings_op_seq = self.recordings_op_seq.wrapping_add(1);
        Some(RecordingOp {
            attempt: self.recordings_op_seq,
            dialog: dialog.generation,
            server: dialog.server_id,
            session: self.session_generation,
            epoch: self.recording_access_epoch,
            id: id.to_owned(),
        })
    }

    /// Does this attempt still own the open modal and its busy row?
    pub(super) fn recording_op_owned(&self, op: &RecordingOp) -> bool {
        self.session_generation == op.session
            && self.recording_access_epoch == op.epoch
            && self.server == op.server
            && self.recordings_dialog.as_ref().is_some_and(|dialog| {
                dialog.generation == op.dialog
                    && dialog.server_id == op.server
                    && dialog.epoch == op.epoch
                    && dialog.busy == op.id
                    && dialog.busy_attempt == op.attempt
            })
    }

    /// The server snapshot still belongs to the same selected server and
    /// session, even if the modal has closed or access changed.
    pub(super) fn recording_snapshot_owned(&self, op: &RecordingOp) -> bool {
        self.session_generation == op.session
            && self.server == op.server
            && self.recordings.server_id == op.server
    }

    /// Release `busy` only when this attempt still owns it.
    pub(super) fn finish_recording_op(&mut self, op: &RecordingOp) -> bool {
        if !self.recording_op_owned(op) {
            return false;
        }
        if let Some(dialog) = self.recordings_dialog.as_mut() {
            dialog.busy.clear();
            dialog.busy_attempt = 0;
            dialog.confirm.clear();
        }
        true
    }
}

impl Voxhold {
    pub(super) fn toggle_recordings(
        &mut self,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.recordings_dialog.is_some() {
            self.close_recordings(true, window, cx);
            return;
        }
        if self.server == 0 {
            return;
        }
        // Go `toggleRecordings`: one modal and no stale overlays under it.
        self.show_search = false;
        self.show_inbox = false;
        self.message_menu = None;
        self.member_menu = None;
        self.forward = None;
        self.editing = None;
        self.confirm_delete = None;
        self.close_settings(false, window, cx);
        self.close_thread(window, cx);
        self.recordings_voice_entry = source == "voice";
        #[cfg(feature = "voxhold-media")]
        if self.recordings_voice_entry {
            if let Some(voice) = &self.voice {
                self.recordings_return_focus = Some(voice.read(cx).recordings_focus());
            }
        }
        if self.recordings_return_focus.is_none() {
            self.recordings_return_focus = Some(self.recordings_entry_focus.clone());
        }
        self.recordings_dialog = Some(recordings_dialog::DialogState::new(
            self.server,
            self.recordings_dialog_seq.wrapping_add(1),
            self.recording_access_epoch,
        ));
        self.recordings_dialog_seq = self.recordings_dialog_seq.wrapping_add(1);
        self.refresh_recordings(0, window, cx);
        let close = self.recordings_close_focus.clone();
        window.defer(cx, move |window, _| window.focus(&close));
        cx.notify();
    }

    pub(super) fn close_recordings(
        &mut self,
        restore_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut dialog) = self.recordings_dialog.take() else {
            return;
        };
        dialog.generation = dialog.generation.wrapping_add(1);
        self.recordings_preview_tasks.clear();
        self.abort_recording_previews();
        for image in dialog.renew_preview_scope() {
            let _ = window.drop_image(image);
        }
        if restore_focus {
            #[cfg(feature = "voxhold-media")]
            if self.recordings_voice_entry {
                let voice_focus = self.voice.as_ref().and_then(|voice| {
                    let voice = voice.read(cx);
                    voice
                        .recordings_button_mounted()
                        .then(|| voice.recordings_focus())
                });
                self.recordings_return_focus = Some(
                    voice_focus.unwrap_or_else(|| self.recordings_entry_focus.clone()),
                );
            }
            if let Some(focus) = self.recordings_return_focus.take() {
                window.focus(&focus);
            }
        } else {
            self.recordings_return_focus = None;
        }
        self.recordings_voice_entry = false;
        cx.notify();
    }

    /// Called when the selected server starts or ends its session.
    pub(super) fn reset_recordings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_recordings(false, window, cx);
        self.recording_access_epoch = self.recording_access_epoch.wrapping_add(1);
        self.recordings.reset();
        self.recordings
            .set_access_epoch(self.recording_access_epoch);
    }

    /// A reconnect/member/channel change unconfirms both the server snapshot
    /// and every private preview, then re-reads a snapshot that was in use.
    pub(super) fn invalidate_recordings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.recording_access_epoch = self.recording_access_epoch.wrapping_add(1);
        self.recordings
            .set_access_epoch(self.recording_access_epoch);
        self.sync_recording_dialog_scope(window, cx);
        if self.recordings.loaded && self.server != 0 {
            self.refresh_recordings(0, window, cx);
        }
        cx.notify();
    }

    pub(super) fn sync_recording_dialog_scope(
        &mut self,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let epoch = self.recording_access_epoch;
        let images = {
            let Some(dialog) = self.recordings_dialog.as_mut() else {
                return;
            };
            if dialog.epoch == epoch {
                return;
            }
            dialog.epoch = epoch;
            // Rights changed: the old attempt may still stream a file, but it
            // no longer owns this dialog's busy row or delete confirmation.
            dialog.busy.clear();
            dialog.busy_attempt = 0;
            dialog.confirm.clear();
            dialog.pressed = None;
            dialog.renew_preview_scope()
        };
        self.recordings_preview_tasks.clear();
        self.abort_recording_previews();
        for image in images {
            let _ = window.drop_image(image);
        }
    }

    pub(super) fn abort_recording_previews(&mut self) {
        for request in self.recordings_preview_aborts.drain(..) {
            request.abort.abort();
        }
    }

    /// Keep the keyboard ring inside the dialog when the focused action was
    /// removed by a WS update, an access change or a finished operation.
    pub(super) fn ensure_recording_dialog_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.recordings_dialog.as_ref() else {
            return;
        };
        let Some(key) = dialog.focused_key.clone() else {
            return;
        };
        let present = match key.as_str() {
            "rec-close" | "rec-dismiss" => true,
            "rec-retry" => !self.recordings.error.is_empty() && !self.recordings.loading,
            _ => dialog.action_rect(&key).is_some(),
        };
        if present {
            return;
        }
        if let Some(dialog) = self.recordings_dialog.as_mut() {
            dialog.focused_key = None;
            dialog.pressed = None;
            dialog.scroll_to = None;
        }
        window.focus(&self.recordings_close_focus);
        cx.notify();
    }

    /// `refreshRecordings(attempt)`: one authoritative list for the selected
    /// server. Events and access changes that win the race trigger a bounded
    /// retry instead of applying an older snapshot.
    pub(super) fn refresh_recordings(
        &mut self,
        attempt: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let server = self.server;
        if server == 0 {
            return;
        }
        let epoch = self.recording_access_epoch;
        let request = self.recordings.begin_refresh(server, epoch, attempt);
        let runtime = self.runtime.clone();
        let task =
            cx.background_spawn(async move { runtime.block_on(client.list_recordings(server)) });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.finish_recording_list(result, request, window, cx);
            });
        })
        .detach();
    }

    /// Apply one list answer and start the bounded retry when an event or an
    /// access change won the race. Extracted so the retry branch is testable
    /// without holding a real request open.
    pub(super) fn finish_recording_list(
        &mut self,
        result: std::result::Result<voxhold_api::RecordingList, voxhold_api::Error>,
        request: recordings::ListRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let outcome =
            self.recordings
                .apply_list(result, request, self.recording_access_epoch, now_secs());
        if outcome == recordings::ListOutcome::Retry
            && self.recordings.active.is_none()
            && self.server == request.server_id
        {
            let next = if request.attempt < 3 {
                request.attempt + 1
            } else {
                0
            };
            self.refresh_recordings(next, window, cx);
        }
        self.sync_recording_dialog_scope(window, cx);
        self.ensure_recording_dialog_focus(window, cx);
        cx.notify();
    }

    pub(super) fn delete_recording(
        &mut self,
        record: Recording,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let now = now_secs();
        let Some(dialog) = self.recordings_dialog.as_ref() else {
            return;
        };
        if !dialog.busy.is_empty() {
            return;
        }
        let server = dialog.server_id;
        if server != self.server
            || !self
                .recordings
                .owns(&record, self.self_user_id, &self.auth.username)
            || recordings::live(&record, now)
            || !(record.status == voxhold_api::RECORDING_READY
                || record.status == voxhold_api::RECORDING_UPLOADING)
            || recordings::expired(&record, now)
        {
            return;
        }
        let Some(op) = self.next_recording_op(&record.id) else {
            return;
        };
        if let Some(dialog) = self.recordings_dialog.as_mut() {
            dialog.busy = record.id.clone();
            dialog.busy_attempt = op.attempt;
            dialog.confirm.clear();
            dialog.error.clear();
        }
        let id = record.id.clone();
        let runtime = self.runtime.clone();
        let request_id = id.clone();
        let task = cx.background_spawn(async move {
            let answer = runtime.block_on(client.delete_recording(&request_id))?;
            Ok::<_, voxhold_api::Error>(answer)
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                let mut changed = false;
                match &result {
                    Ok(deleted) => {
                        // The tombstone belongs to the server model, not to the
                        // window. A closed modal or a changed access epoch may
                        // still accept a matching confirmation for this server
                        // and session.
                        if s.recording_snapshot_owned(&op)
                            && deleted.id == id
                            && deleted.server_id == server
                            && deleted.status == voxhold_api::RECORDING_DELETED
                        {
                            changed |= s.recordings.apply_event(deleted.clone(), now_secs());
                        }
                    }
                    Err(error) => {
                        // A 403/404/409 means the row moved on the server;
                        // let the authoritative list correct the snapshot.
                        if s.session_generation == op.session
                            && s.server == server
                            && matches!(error.status(), Some(403 | 404 | 409))
                        {
                            s.refresh_recordings(0, window, cx);
                            changed = true;
                        }
                    }
                }
                // Only the attempt that still owns the open modal may publish
                // an error or clear its busy row.
                if s.finish_recording_op(&op) {
                    if let Err(error) = &result {
                        if let Some(dialog) = s.recordings_dialog.as_mut() {
                            dialog.error = crate::draft::describe_error(error);
                        }
                    }
                    changed = true;
                }
                if changed {
                    cx.notify();
                }
                s.ensure_recording_dialog_focus(window, cx);
            });
        })
        .detach();
    }

    /// A preview may start only for a list confirmed for this access epoch.
    pub(super) fn recordings_visible_for_preview(&self) -> bool {
        let Some(dialog) = self.recordings_dialog.as_ref() else {
            return false;
        };
        dialog.epoch == self.recording_access_epoch
            && self.recordings.confirmed_epoch == Some(self.recording_access_epoch)
            && !self.recordings.loading
            && self.recordings.error.is_empty()
    }

    /// The shared gate for both starting a preview GET and reading its cache.
    pub(super) fn recording_preview_allowed(&self, id: &str, version: &str) -> bool {
        if version.is_empty() || !self.recordings_visible_for_preview() {
            return false;
        }
        let now = now_secs();
        self.recordings.records.iter().any(|record| {
            record.id == id
                && record.server_id == self.server
                && record.preview_version.as_deref() == Some(version)
                && recordings::ready(record, now)
        })
    }
}
