//! Windows save path for server recordings. The picker runs first; only
//! then is a ticket requested, and the body streams through the same atomic
//! `.part` helper as attachments.

use super::recordings_ops::now_secs;
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
        let Some(dialog) = self.recordings_dialog.as_mut() else {
            return;
        };
        if !dialog.busy.is_empty() {
            return;
        }
        dialog.busy = record.id.clone();
        dialog.error.clear();
        let generation = dialog.generation;
        let server = dialog.server_id;
        let epoch = self.recording_access_epoch;
        let id = record.id.clone();
        let filename = record.filename();
        let expect = record.size_bytes;
        let directory = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        cx.notify();
        let _ = window;
        let prompt = cx.prompt_for_new_path(&directory, Some(&filename));
        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                _ => {
                    if let Some(this) = this.upgrade() {
                        let _ = this.update(cx, |s, cx| {
                            if let Some(dialog) = s.recordings_dialog.as_mut() {
                                if dialog.busy == id {
                                    dialog.busy.clear();
                                }
                            }
                            cx.notify();
                        });
                    }
                    return;
                }
            };
            let Some(this) = this.upgrade() else { return };
            let live = this
                .update(cx, |s, _| {
                    s.recordings_dialog.as_ref().is_some_and(|dialog| {
                        dialog.generation == generation
                            && dialog.server_id == server
                            && dialog.busy == id
                    }) && s.recording_access_epoch == epoch
                        && s.server == server
                })
                .unwrap_or(false);
            if !live {
                let _ = this.update(cx, |s, cx| {
                    if let Some(dialog) = s.recordings_dialog.as_mut() {
                        if dialog.busy == id {
                            dialog.busy.clear();
                        }
                    }
                    cx.notify();
                });
                return;
            }
            let task = this
                .update(cx, |state, cx| {
                    let Some(client) = state.client.clone() else {
                        return None;
                    };
                    let runtime = state.runtime.clone();
                    let id = id.clone();
                    Some(cx.background_spawn(async move {
                        let ticket = runtime
                            .block_on(client.recording_download_ticket(&id))
                            .map_err(|error| crate::draft::describe_error(&error))?;
                        let mut save = file_io::PartFile::create(&path)?;
                        let expected = (expect > 0).then_some(expect as u64);
                        let result = runtime.block_on(client.download_recording_to(
                            &ticket,
                            expect,
                            &mut |chunk| save.write(chunk),
                        ));
                        match result {
                            Ok(_) => save.finish(expected).map(|_| ()),
                            Err(error) => {
                                save.abort();
                                Err(crate::draft::describe_error(&error))
                            }
                        }
                    }))
                })
                .ok()
                .flatten();
            let Some(task) = task else { return };
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if let Some(dialog) = s.recordings_dialog.as_mut() {
                    if dialog.generation != generation || dialog.server_id != server {
                        return;
                    }
                    if dialog.busy == id {
                        dialog.busy.clear();
                    }
                    if let Err(error) = result {
                        dialog.error = error;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
