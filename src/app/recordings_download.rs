//! Windows save path for server recordings. The picker runs first; only
//! then is a ticket requested, and the body streams through the same atomic
//! `.part` helper as attachments.

use super::recordings_ops::{now_secs, RecordingOp};
use super::*;
use crate::{file_io, recordings};
use std::path::PathBuf;
use voxhold_api::Recording;

impl Voxhold {
    pub(super) fn download_recording(
        &mut self,
        record: Recording,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.client.is_none() {
            return;
        }
        let now = now_secs();
        if !recordings::ready(&record, now)
            || self.recordings.confirmed_epoch != Some(self.recording_access_epoch)
        {
            return;
        }
        let Some(dialog) = self.recordings_dialog.as_ref() else {
            return;
        };
        if !dialog.busy.is_empty() || dialog.server_id != self.server {
            return;
        }
        let Some(op) = self.next_recording_op(&record.id) else {
            return;
        };
        if let Some(dialog) = self.recordings_dialog.as_mut() {
            dialog.busy = record.id.clone();
            dialog.busy_attempt = op.attempt;
            dialog.error.clear();
        }
        let id = record.id.clone();
        let filename = record.filename();
        let directory = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        cx.notify();
        let _ = window;
        let prompt = cx.prompt_for_new_path(&directory, Some(&filename));
        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => Some(path),
                _ => None,
            };
            let Some(this) = this.upgrade() else { return };
            let _ = this.update(cx, |s, cx| s.resolve_recording_pick(op, path, cx));
        })
        .detach();
    }

    /// Apply the picker answer. A stale answer is ignored completely: it may
    /// not clear another attempt's busy row, error or confirmation and it may
    /// not request a ticket.
    pub(super) fn resolve_recording_pick(
        &mut self,
        op: RecordingOp,
        path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if !self.recording_op_owned(&op) {
            return;
        }
        let Some(path) = path else {
            if self.finish_recording_op(&op) {
                cx.notify();
            }
            return;
        };
        let now = now_secs();
        let record = self
            .recordings
            .records
            .iter()
            .find(|record| {
                record.id == op.id
                    && record.server_id == op.server
                    && recordings::ready(record, now)
            })
            .cloned();
        if self.recordings.confirmed_epoch != Some(op.epoch) || record.is_none() {
            if self.finish_recording_op(&op) {
                cx.notify();
            }
            return;
        }
        let record = record.unwrap();
        self.begin_recording_download(op, record, path, cx);
    }

    fn begin_recording_download(
        &mut self,
        op: RecordingOp,
        record: Recording,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let runtime = self.runtime.clone();
        let id = record.id.clone();
        let expect = record.size_bytes;
        let task = cx.background_spawn(async move {
            let ticket = runtime
                .block_on(client.recording_download_ticket(&id))
                .map_err(|error| crate::draft::describe_error(&error))?;
            let mut save = file_io::PartFile::create(&path)?;
            let expected = (expect > 0).then_some(expect as u64);
            let result =
                runtime.block_on(
                    client.download_recording_to(&ticket, expect, &mut |chunk| save.write(chunk)),
                );
            match result {
                Ok(_) => save.finish(expected).map(|_| ()),
                Err(error) => {
                    save.abort();
                    Err(crate::draft::describe_error(&error))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let Some(this) = this.upgrade() else { return };
            let _ = this.update(cx, |s, cx| {
                // A streaming save may finish after the dialog was closed;
                // only the attempt that still owns the open modal publishes
                // the result.
                if s.finish_recording_op(&op) {
                    if let Err(error) = result {
                        if let Some(dialog) = s.recordings_dialog.as_mut() {
                            dialog.error = error;
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
