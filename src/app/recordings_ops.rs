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
            self.recordings.event_seq.wrapping_add(1),
            self.recording_access_epoch,
        ));
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
        for image in dialog.renew_preview_scope() {
            let _ = window.drop_image(image);
        }
        if restore_focus {
            #[cfg(feature = "voxhold-media")]
            if self.recordings_voice_entry {
                self.recordings_return_focus = Some(
                    self.voice
                        .as_ref()
                        .map(|voice| voice.read(cx).recordings_focus())
                        .unwrap_or_else(|| self.recordings_entry_focus.clone()),
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
        let Some(dialog) = self.recordings_dialog.as_mut() else {
            return;
        };
        if dialog.epoch == epoch {
            return;
        }
        dialog.epoch = epoch;
        self.recordings_preview_tasks.clear();
        for image in dialog.renew_preview_scope() {
            let _ = window.drop_image(image);
        }
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
        let Some(generation) = self.recordings_dialog.as_ref().map(|p| p.generation) else {
            return;
        };
        let server = self.server;
        if !self
            .recordings
            .owns(&record, self.self_user_id, &self.auth.username)
            || recordings::live(&record, now)
            || !(record.status == voxhold_api::RECORDING_READY
                || record.status == voxhold_api::RECORDING_UPLOADING)
            || recordings::expired(&record, now)
        {
            return;
        }
        if let Some(dialog) = self.recordings_dialog.as_mut() {
            if !dialog.busy.is_empty() {
                return;
            }
            dialog.busy = record.id.clone();
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
                let current = s
                    .recordings_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.generation == generation);
                if !current || s.server != server {
                    return;
                }
                match result {
                    Ok(deleted) => {
                        let _ = s.recordings.apply_event(deleted, now_secs());
                    }
                    Err(error) => {
                        // A 403/404/409 means the row moved on the server;
                        // let the authoritative list correct the snapshot.
                        if matches!(error.status(), Some(403 | 404 | 409)) {
                            s.refresh_recordings(0, window, cx);
                        }
                        if let Some(dialog) = s.recordings_dialog.as_mut() {
                            dialog.error = crate::draft::describe_error(&error);
                        }
                    }
                }
                if let Some(dialog) = s.recordings_dialog.as_mut() {
                    if dialog.busy == id {
                        dialog.busy.clear();
                    }
                    dialog.confirm.clear();
                }
                cx.notify();
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
            && self.recordings.confirmed_epoch == self.recording_access_epoch
            && !self.recordings.loading
            && self.recordings.error.is_empty()
    }
}
