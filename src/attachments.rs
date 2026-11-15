//! Composer attachments: file selection, Windows clipboard/drop intake, the
//! selected-file row and attachment saving. Metadata lives in `draft.rs`; the
//! bytes stay on disk and are streamed by `send` in `app::composer`.

use gpui::{div, prelude::*, px, rgb, Context, PathPromptOptions, Window};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use voxhold_api::{Attachment, Client};

use crate::{clipboard, draft, file_io};

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

    /// Scope generation of the panel; a dialog result is only accepted by the
    /// same generation and chat.
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    #[cfg(test)]
    pub(crate) fn error(&self) -> &str {
        &self.error
    }

    #[cfg(test)]
    pub(crate) fn message_file_count(&self) -> usize {
        self.message_files.len()
    }

    fn can_edit(&self) -> bool {
        !self.loading && self.writable && self.key.is_some()
    }

    /// Open the system file dialog. Go reaches the same adapter from clipboard
    /// and drop; this is the visible entry point of the native composer.
    /// Drop confirmed deleted files from the save list immediately, before a
    /// history refresh can rebuild the selected message.
    pub fn remove_message_files(&mut self, ids: &[String], cx: &mut Context<Self>) {
        if self.message_files.iter().any(|file| ids.contains(&file.id)) {
            self.message_files.retain(|file| !ids.contains(&file.id));
            cx.notify();
        }
    }

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
        let expected = self.key;
        let generation = self.generation;
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match prompt.await {
                Ok(Ok(Some(paths))) => Ok(paths),
                Ok(Ok(None)) => Err(String::new()),
                _ => Err("Не удалось выбрать файлы".into()),
            };
            if let Some(this) = this.upgrade() {
                let _ = this.update(cx, |state, cx| {
                    state.finish_pick(result, expected, generation, cx)
                });
            }
        })
        .detach();
    }

    /// The dialog result belongs to the chat and scope that opened it. A
    /// switch or access change discards it without touching the new draft.
    pub(crate) fn finish_pick(
        &mut self,
        result: Result<Vec<PathBuf>, String>,
        expected: Option<draft::ChatKey>,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.generation != generation || self.key != expected {
            return;
        }
        self.loading = false;
        match result {
            Ok(paths) => self.add_paths(paths, cx),
            Err(error) => {
                if !error.is_empty() {
                    self.error = error;
                }
                cx.notify();
            }
        }
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
        // Check the draft limits before writing anything into %TEMP%; a
        // rejected picture must not leave a file behind.
        if let Err(error) = self.drafts.borrow().can_add(key, bytes.len() as u64) {
            self.error = error;
            cx.notify();
            return true;
        }
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
                            let cleanup = file.clone();
                            let (added, error) =
                                state.drafts.borrow_mut().add_files(key, vec![file]);
                            if added == 0 {
                                // The draft grew meanwhile; release the
                                // temporary clipboard copy.
                                cleanup.cleanup();
                            }
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
        let name = file_io::safe_file_name(&file.filename);
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
                    let file = file.clone();
                    let id = id.clone();
                    cx.background_spawn(async move {
                        let mut save = file_io::PartFile::create(&path)?;
                        let expected = (file.size_bytes > 0).then_some(file.size_bytes as u64);
                        let downloaded = runtime.block_on(
                            client.download_attachment_to(&id, &mut |chunk| save.write(chunk)),
                        );
                        match downloaded {
                            Ok(_) => save.finish(expected).map(|_| ()),
                            Err(error) => {
                                save.abort();
                                Err(draft::describe_error(&error))
                            }
                        }
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

/// Upload every selected file in order and return the confirmed ids. The
/// draft keeps its ids across retries, so the backend returns the same
/// attachment instead of storing a second copy.
pub async fn upload_draft(client: &Client, plan: &draft::SendPlan) -> Result<Vec<String>, String> {
    let mut ids = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        let handle = file.open_verified()?;
        client
            .upload_attachment_file(
                plan.key.attachment_kind(),
                plan.destination(),
                &file.id,
                &file.name,
                handle,
                file.size,
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
    use crate::test_support::{Reply, TestServer};
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

    fn upload_fixture(payload: &[u8]) -> (draft::SendPlan, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "voxhold-upload-{}-{}.bin",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, payload).unwrap();
        let file = draft::DraftFile::from_path(path.clone()).unwrap();
        let attachment_json = format!(
            "{{\"id\":\"{}\",\"filename\":\"{}\",\"mime_type\":\"application/octet-stream\",\"size_bytes\":{},\"deleted\":false}}",
            file.id, file.name, file.size
        );
        let plan = draft::SendPlan {
            key: draft::ChatKey::channel(1, 7),
            content: "файл".into(),
            composer_text: "файл".into(),
            reply: 0,
            files: vec![file],
            send_key: "0123456789abcdef0123456789abcdef".into(),
            revision: 1,
        };
        let _ = attachment_json;
        (plan, path)
    }

    #[test]
    fn upload_streams_the_file_with_a_content_length() {
        let payload: Vec<u8> = (0..300_000u32).map(|index| (index % 251) as u8).collect();
        let (plan, path) = upload_fixture(&payload);
        let attachment = format!(
            "{{\"id\":\"{}\",\"filename\":\"{}\",\"mime_type\":\"application/octet-stream\",\"size_bytes\":{},\"deleted\":false}}",
            plan.files[0].id, plan.files[0].name, plan.files[0].size
        );
        let server = TestServer::spawn(vec![Reply::new(200, &attachment)]);
        let client = Client::new(&server.base).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ids = runtime
            .block_on(upload_draft(&client, &plan))
            .expect("upload");
        assert_eq!(ids, vec![plan.files[0].id.clone()]);
        let records = server.requests();
        assert_eq!(records.len(), 1);
        let request = &records[0];
        assert_eq!(request.method, "PUT");
        assert!(request.path.starts_with(&format!(
            "/api/v1/channels/7/attachments/{}",
            plan.files[0].id
        )));
        assert!(request.path.contains("filename="));
        assert_eq!(
            request.header("content-length"),
            Some(payload.len().to_string().as_str())
        );
        assert_eq!(
            request.header("content-type"),
            Some("application/octet-stream")
        );
        assert_eq!(request.body, payload, "the body is the file, not JSON");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn conversations_route_is_used_for_personal_messages() {
        let (mut plan, path) = upload_fixture(b"direct");
        plan.key = draft::ChatKey::conversation(11);
        let attachment = format!(
            "{{\"id\":\"{}\",\"filename\":\"{}\",\"size_bytes\":4,\"deleted\":false}}",
            plan.files[0].id, plan.files[0].name
        );
        let server = TestServer::spawn(vec![Reply::new(200, &attachment)]);
        let client = Client::new(&server.base).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(upload_draft(&client, &plan)).unwrap();
        assert!(server.requests()[0]
            .path
            .starts_with("/api/v1/conversations/11/attachments/"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn refused_upload_keeps_the_draft_and_surfaces_the_status() {
        let (plan, path) = upload_fixture(b"denied");
        let server = TestServer::spawn(vec![
            Reply::new(403, "{\"error\":\"upload unavailable\"}"),
            Reply::new(429, "{\"error\":\"uploads busy\"}"),
        ]);
        let client = Client::new(&server.base).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime.block_on(upload_draft(&client, &plan)).unwrap_err();
        assert_eq!(error, "Недостаточно прав доступа.");
        // A retry repeats the same upload id; the backend answers 200 for an
        // already completed attachment.
        let retry = runtime.block_on(upload_draft(&client, &plan)).unwrap_err();
        assert_eq!(retry, "Слишком много запросов. Повторите позже.");
        let records = server.requests();
        assert_eq!(records.len(), 2);
        assert!(records[1].path.contains(&plan.files[0].id));
        assert_eq!(plan.files[0].id.len(), 32);

        // The draft itself still holds the file for the retry.
        let mut drafts = draft::Drafts::default();
        drafts.add_files(plan.key, plan.files.clone());
        assert_eq!(drafts.files(plan.key).len(), 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_file_that_changed_is_refused_before_the_request() {
        let (plan, path) = upload_fixture(b"original");
        std::fs::write(&path, b"changed content").unwrap();
        let server = TestServer::spawn(vec![]);
        let client = Client::new(&server.base).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime.block_on(upload_draft(&client, &plan)).unwrap_err();
        assert_eq!(
            error,
            "Файл изменился после добавления. Прикрепите его заново."
        );
        assert!(server.requests().is_empty(), "no request leaves the client");
        std::fs::remove_file(path).unwrap();
    }

    /// Write a file without holding it in memory.
    fn large_file(name: &str, size: usize) -> PathBuf {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "voxhold-measure-{}-{}-{name}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        let chunk = vec![0x5a_u8; 64 << 10];
        let mut left = size;
        while left > 0 {
            let take = left.min(chunk.len());
            file.write_all(&chunk[..take]).unwrap();
            left -= take;
        }
        file.sync_all().unwrap();
        path
    }

    fn plan_for(path: &std::path::Path) -> draft::SendPlan {
        let file = draft::DraftFile::from_path(path.to_path_buf()).unwrap();
        draft::SendPlan {
            key: draft::ChatKey::channel(1, 7),
            content: "большой файл".into(),
            composer_text: "большой файл".into(),
            reply: 0,
            files: vec![file],
            send_key: "0123456789abcdef0123456789abcdef".into(),
            revision: 1,
        }
    }

    /// R03 acceptance measurement: private bytes of the whole process before,
    /// during and after an 8 MiB and a 60 MiB upload and save. Run with
    /// `cargo test --release -p voxhold-native-rust measure_attachment_transfer_memory
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "memory measurement; run explicitly in release"]
    fn measure_attachment_transfer_memory() {
        use crate::test_support::{private_bytes, Reply, TestServer};
        use std::sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc,
        };

        struct Sampler {
            stop: Arc<AtomicBool>,
            peak: Arc<AtomicU64>,
            handle: Option<std::thread::JoinHandle<()>>,
        }
        impl Sampler {
            fn start() -> Self {
                let stop = Arc::new(AtomicBool::new(false));
                let peak = Arc::new(AtomicU64::new(private_bytes()));
                let running = stop.clone();
                let sampled = peak.clone();
                let handle = std::thread::spawn(move || {
                    while !running.load(Ordering::Relaxed) {
                        let value = private_bytes();
                        sampled.fetch_max(value, Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                });
                Self {
                    stop,
                    peak,
                    handle: Some(handle),
                }
            }
            fn stop(mut self) -> u64 {
                self.stop.store(true, Ordering::Relaxed);
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
                self.peak.load(Ordering::Relaxed)
            }
        }

        let runtime = tokio::runtime::Runtime::new().unwrap();
        for (mib, size) in [(8usize, 8usize << 20), (60, 60usize << 20)] {
            let source = large_file(&format!("{mib}mib.bin"), size);
            let plan = plan_for(&source);
            let attachment = format!(
                "{{\"id\":\"{}\",\"filename\":\"{}\",\"mime_type\":\"application/octet-stream\",\"size_bytes\":{},\"deleted\":false}}",
                plan.files[0].id, plan.files[0].name, size
            );
            let server = TestServer::spawn(vec![Reply::new(200, &attachment)]);
            let client = Client::new(&server.base).unwrap();
            let base = private_bytes();
            let sampler = Sampler::start();
            runtime.block_on(upload_draft(&client, &plan)).unwrap();
            let upload_peak = sampler.stop();
            let after_upload = private_bytes();

            let target = std::env::temp_dir().join(format!(
                "voxhold-measure-target-{}-{mib}mib.bin",
                std::process::id()
            ));
            let server = TestServer::spawn(vec![Reply::file(
                200,
                source.clone(),
                "application/octet-stream",
            )]);
            let client = Client::new(&server.base).unwrap();
            let sampler = Sampler::start();
            let mut save = crate::file_io::PartFile::create(&target).unwrap();
            let written = runtime
                .block_on(
                    client
                        .download_attachment_to(&plan.files[0].id, &mut |chunk| save.write(chunk)),
                )
                .unwrap();
            save.finish(Some(written)).unwrap();
            let save_peak = sampler.stop();
            let after_save = private_bytes();
            println!(
                "size_mib={mib} baseline={base} upload_peak={upload_peak} after_upload={after_upload} save_peak={save_peak} after_save={after_save} bytes={written}"
            );
            assert_eq!(written, size as u64);
            assert_eq!(std::fs::metadata(&target).unwrap().len(), size as u64);
            std::fs::remove_file(source).unwrap();
            std::fs::remove_file(target).unwrap();
        }
    }
}
