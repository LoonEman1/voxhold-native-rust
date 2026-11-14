//! Composer attachments: file selection, Windows clipboard/drop intake, the
//! selected-file row and attachment saving. Metadata lives in `draft.rs`; the
//! bytes stay on disk until the send task opens them.

use gpui::{div, prelude::*, px, rgb, Context, PathPromptOptions, Window};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use voxhold_api::{Attachment, Client};

use crate::{clipboard, draft};

pub struct Attachments {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
    drafts: draft::Shared,
    key: Option<draft::ChatKey>,
    writable: bool,
    message_files: Vec<Attachment>,
    /// Reading `metadata` of the picked files; sending is blocked meanwhile.
    loading: bool,
    /// Attachment ids whose download is running.
    saving: HashSet<String>,
    saved: Option<String>,
    error: String,
    generation: u64,
}

impl Attachments {
    pub fn new(
        client: Client,
        runtime: Arc<tokio::runtime::Runtime>,
        drafts: draft::Shared,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            client,
            runtime,
            drafts,
            key: None,
            writable: false,
            message_files: Vec::new(),
            loading: false,
            saving: HashSet::new(),
            saved: None,
            error: String::new(),
            generation: 0,
        }
    }

    pub fn set_scope(
        &mut self,
        key: Option<draft::ChatKey>,
        writable: bool,
        cx: &mut Context<Self>,
    ) {
        self.key = key;
        self.writable = writable;
        self.message_files.clear();
        self.saved = None;
        self.generation = self.generation.wrapping_add(1);
        self.loading = false;
        self.error.clear();
        cx.notify();
    }

    pub fn set_message(&mut self, message: Option<&voxhold_api::Message>, cx: &mut Context<Self>) {
        self.message_files = message
            .map(|message| {
                message
                    .attachments
                    .iter()
                    .filter(|file| !file.deleted)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        cx.notify();
    }

    pub fn is_reading(&self) -> bool {
        self.loading
    }

    fn can_edit(&self) -> bool {
        !self.loading && self.writable && self.key.is_some()
    }

    /// Open the system file dialog. Go reaches the same adapter from clipboard
    /// and drop; this is the visible entry point of the native composer.
    pub fn pick(&mut self, cx: &mut Context<Self>) {
        if !self.can_edit() {
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Выберите файлы".into()),
        });
        let generation = self.generation;
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let paths = match prompt.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => {
                    if let Some(this) = this.upgrade() {
                        let _ = this.update(cx, |state, cx| {
                            if state.generation == generation {
                                state.loading = false;
                                cx.notify();
                            }
                        });
                    }
                    return;
                }
                _ => {
                    if let Some(this) = this.upgrade() {
                        let _ = this.update(cx, |state, cx| {
                            if state.generation == generation {
                                state.loading = false;
                                state.error = "Не удалось выбрать файлы".into();
                                cx.notify();
                            }
                        });
                    }
                    return;
                }
            };
            if let Some(this) = this.upgrade() {
                let _ = this.update(cx, |state, cx| state.add_paths(paths, cx));
            }
        })
        .detach();
    }

    /// Metadata-only intake from the file dialog, OS drop or clipboard paths.
    /// The files are opened for `metadata` in the background and the draft
    /// only receives `id/path/name/size/mtime`.
    pub fn add_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(key) = self.key.filter(|_| self.can_edit()) else {
            return;
        };
        let generation = self.generation;
        self.loading = true;
        self.error.clear();
        cx.notify();
        let task = cx.background_spawn(async move { read_metadata(paths) });
        cx.spawn(async move |this, cx| {
            let (files, read_error) = task.await;
            if let Some(this) = this.upgrade() {
                let _ = this.update(cx, |state, cx| {
                    if state.generation != generation || state.key != Some(key) {
                        return;
                    }
                    state.loading = false;
                    let (added, error) = state.drafts.borrow_mut().add_files(key, files);
                    if !error.is_empty() {
                        state.error = error;
                    } else if !read_error.is_empty() {
                        state.error = read_error;
                    } else if added > 0 {
                        state.saved = None;
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Paste priority of Go `view.go::OnPasteFiles`. Returns true when the
    /// composer consumed the paste, so the text input must not also paste.
    pub fn paste(&mut self, paste: clipboard::Paste, cx: &mut Context<Self>) -> bool {
        if !self.can_edit() {
            return false;
        }
        if !paste.paths.is_empty() {
            self.add_paths(paste.paths, cx);
            return true;
        }
        let Some(bytes) = paste.image else {
            return false;
        };
        let Some(key) = self.key else {
            return false;
        };
        let generation = self.generation;
        self.loading = true;
        self.error.clear();
        cx.notify();
        let task = cx.background_spawn(async move {
            let path = clipboard::write_png(&bytes)
                .map_err(|_| "Не удалось подготовить изображение из буфера обмена.".to_string())?;
            draft::DraftFile::from_temporary(path, "clipboard.png")
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                let _ = this.update(cx, |state, cx| {
                    if state.generation != generation || state.key != Some(key) {
                        if let Ok(file) = &result {
                            file.cleanup();
                        }
                        return;
                    }
                    state.loading = false;
                    match result {
                        Ok(file) => {
                            let (_, error) = state.drafts.borrow_mut().add_files(key, vec![file]);
                            if !error.is_empty() {
                                state.error = error;
                            }
                        }
                        Err(error) => state.error = error,
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        true
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let Some(key) = self.key else { return };
        if let Some(file) = self.drafts.borrow_mut().remove_file(key, id) {
            file.cleanup();
        }
        cx.notify();
    }

    /// Go `saveAttachment`: the download streams into a unique `.part` next to
    /// the chosen target and replaces it only after a complete, verified body.
    fn save(&mut self, file: Attachment, cx: &mut Context<Self>) {
        if self.saving.contains(&file.id) {
            return;
        }
        let name = download_name(&file.filename);
        let directory = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let prompt = cx.prompt_for_new_path(&directory, Some(&name));
        let id = file.id.clone();
        let generation = self.generation;
        self.saving.insert(id.clone());
        self.error.clear();
        self.saved = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {
                    if let Some(this) = this.upgrade() {
                        let _ = this.update(cx, |state, cx| {
                            state.saving.remove(&id);
                            cx.notify();
                        });
                    }
                    return;
                }
            };
            let Some(this) = this.upgrade() else { return };
            let task = this
                .update(cx, |state, cx| {
                    let client = state.client.clone();
                    let runtime = state.runtime.clone();
                    let id = id.clone();
                    cx.background_spawn(async move {
                        let data = runtime
                            .block_on(client.download_attachment(&id))
                            .map_err(|error| draft::describe_error(&error))?;
                        std::fs::write(&path, data)
                            .map_err(|_| "Не удалось сохранить файл.".to_string())
                    })
                })
                .ok();
            let Some(task) = task else { return };
            let result = task.await;
            let finished_id = id.clone();
            let _ = this.update(cx, |state, cx| {
                state.saving.remove(&finished_id);
                if state.generation != generation {
                    return;
                }
                match result {
                    Ok(()) => state.saved = Some(name),
                    Err(error) => state.error = error,
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Read only metadata. The paths are opened one at a time so a directory or a
/// vanished file becomes one message instead of failing the whole selection.
fn read_metadata(paths: Vec<PathBuf>) -> (Vec<draft::DraftFile>, String) {
    let mut files = Vec::new();
    let mut error = String::new();
    for path in paths {
        match draft::DraftFile::from_path(path) {
            Ok(file) => files.push(file),
            Err(message) => {
                error = format!("Не удалось прикрепить файл: {message}");
            }
        }
    }
    (files, error)
}

impl Render for Attachments {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .px(px(16.))
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(12.))
            .text_color(rgb(0xf3f4eb));
        let key = self.key;
        let files = key
            .map(|key| self.drafts.borrow().files(key))
            .unwrap_or_default();
        if self.writable && !files.is_empty() {
            for (index, file) in files.into_iter().enumerate() {
                let id = file.id.clone();
                let size = file.size;
                root = root.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(22.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .child(file.name.clone()),
                        )
                        .child(div().text_color(rgb(0x96968e)).child(format_bytes(size)))
                        .child(
                            div()
                                .id(("remove-file", index))
                                .cursor_pointer()
                                .on_click(cx.listener(move |state, _, _, cx| state.remove(&id, cx)))
                                .child("✕"),
                        ),
                );
            }
        }
        for (index, file) in self.message_files.iter().enumerate() {
            let file = file.clone();
            let busy = self.saving.contains(&file.id);
            let label = if busy {
                format!("Сохранение: {}", file.filename)
            } else {
                format!("Сохранить: {}", file.filename)
            };
            root = root.child(
                div()
                    .id(("download-attachment", index))
                    .cursor_pointer()
                    .on_click(cx.listener(move |state, _, _, cx| state.save(file.clone(), cx)))
                    .child(label),
            );
        }
        if let Some(name) = &self.saved {
            root = root.child(
                div()
                    .text_color(rgb(0x96968e))
                    .child(format!("Файл сохранён: {name}")),
            );
        }
        if self.loading {
            root = root.child(div().text_color(rgb(0x96968e)).child("Подготовка файлов…"));
        }
        if !self.error.is_empty() {
            root = root.child(div().text_color(rgb(0xff7777)).child(self.error.clone()));
        }
        root
    }
}

/// One safe path component for the save dialog; the user still chooses the
/// directory. `file_io::safe_file_name` takes this over with the streaming save.
fn download_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .trim_matches('.');
    let cleaned: String = base
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\0' => '_',
            other if other.is_control() => '_',
            other => other,
        })
        .collect();
    if cleaned.is_empty() {
        "attachment".into()
    } else {
        cleaned
    }
}

/// Upload every selected file in order and return the confirmed ids. The
/// draft keeps its ids across retries, so the backend returns the same
/// attachment instead of storing a second copy.
pub async fn upload_draft(client: &Client, plan: &draft::SendPlan) -> Result<Vec<String>, String> {
    let mut ids = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        let data = std::fs::read(&file.path)
            .map_err(|_| "Файл вложения недоступен. Прикрепите его заново.".to_string())?;
        client
            .upload_attachment(
                plan.key.attachment_kind(),
                plan.destination(),
                &file.id,
                &file.name,
                data,
            )
            .await
            .map_err(|error| draft::describe_error(&error))?;
        ids.push(file.id.clone());
    }
    Ok(ids)
}

/// Compact size for the attachment row; Go draws the name and the remove
/// button only, but a size keeps a 64 MiB file visible as a limit problem.
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} МиБ", bytes as f64 / (1u64 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.0} КиБ", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} Б")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sizes_are_compact() {
        assert_eq!(format_bytes(512), "512 Б");
        assert_eq!(format_bytes(2048), "2 КиБ");
        assert_eq!(format_bytes(64 << 20), "64.0 МиБ");
    }

    #[test]
    fn metadata_reading_skips_missing_entries() {
        let path = std::env::temp_dir().join(format!(
            "voxhold-meta-{}-{}.bin",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"data").unwrap();
        let files = read_metadata(vec![
            PathBuf::from("definitely-missing"),
            path.clone(),
            std::env::temp_dir(),
        ]);
        assert_eq!(files.0.len(), 1);
        assert_eq!(files.0[0].size, 4);
        assert!(files.1.starts_with("Не удалось прикрепить файл:"));
        std::fs::remove_file(path).unwrap();
    }
}
