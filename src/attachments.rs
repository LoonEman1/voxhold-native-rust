use gpui::{div, prelude::*, px, rgb, Context, PathPromptOptions, Window};
use std::{collections::HashMap, io::Read, path::PathBuf, sync::Arc};
use voxhold_api::{Attachment, Client, Message};
const FILE_LIMIT: usize = 64 << 20;
const DRAFT_LIMIT: usize = 128 << 20;
#[derive(Clone)]
struct DraftFile {
    id: String,
    name: String,
    data: Arc<Vec<u8>>,
}
#[derive(Default)]
struct Draft {
    files: Vec<DraftFile>,
    send_key: String,
    signature: String,
}
/// Attachments are kept per destination. A send result must be consumed by
/// the parent to update history and clear its composer after successful send.
pub struct Attachments {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
    server: i64,
    channel: i64,
    writable: bool,
    drafts: HashMap<(i64, i64), Draft>,
    message_files: Vec<Attachment>,
    busy: bool,
    generation: u64,
    error: String,
    sent: Option<Message>,
    revision: u64,
}
impl Attachments {
    pub fn new(
        client: Client,
        runtime: Arc<tokio::runtime::Runtime>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            client,
            runtime,
            server: 0,
            channel: 0,
            writable: false,
            drafts: HashMap::new(),
            message_files: vec![],
            busy: false,
            generation: 0,
            error: String::new(),
            sent: None,
            revision: 0,
        }
    }
    pub fn set_scope(&mut self, server: i64, channel: i64, writable: bool, cx: &mut Context<Self>) {
        self.server = server;
        self.channel = channel;
        self.writable = writable;
        self.message_files.clear();
        self.generation += 1;
        self.busy = false;
        self.error.clear();
        cx.notify();
    }
    pub fn set_message(&mut self, message: Option<&Message>, cx: &mut Context<Self>) {
        self.message_files = message
            .map(|m| {
                m.attachments
                    .iter()
                    .filter(|a| !a.deleted)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        cx.notify();
    }
    pub fn has_files(&self) -> bool {
        self.drafts
            .get(&(self.server, self.channel))
            .is_some_and(|d| !d.files.is_empty())
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn take_sent(&mut self) -> Option<Message> {
        self.sent.take()
    }
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    fn pick(&mut self, cx: &mut Context<Self>) {
        if self.busy || !self.writable || self.channel <= 0 {
            return;
        }
        let key = (self.server, self.channel);
        let generation = self.generation;
        let used: usize = self
            .drafts
            .values()
            .flat_map(|d| d.files.iter())
            .map(|f| f.data.len())
            .sum();
        let count = self.drafts.get(&key).map_or(0, |d| d.files.len());
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Выберите файлы".into()),
        });
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let paths = match prompt.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => {
                    if let Some(e) = this.upgrade() {
                        let _ = e.update(cx, |s, cx| {
                            if s.generation == generation {
                                s.busy = false;
                                cx.notify();
                            }
                        });
                    }
                    return;
                }
                _ => {
                    if let Some(e) = this.upgrade() {
                        let _ = e.update(cx, |s, cx| {
                            if s.generation == generation {
                                s.busy = false;
                                s.error = "Не удалось выбрать файлы".into();
                                cx.notify();
                            }
                        });
                    }
                    return;
                }
            };
            let Some(e) = this.upgrade() else { return };
            let task = e
                .update(cx, |_, cx| {
                    cx.background_spawn(async move { read_files(paths, count, used) })
                })
                .ok();
            let Some(task) = task else { return };
            let (files, error) = task.await;
            let _ = e.update(cx, |s, cx| {
                if s.generation != generation {
                    return;
                }
                s.busy = false;
                if !files.is_empty() {
                    let d = s.drafts.entry(key).or_default();
                    d.files.extend(files);
                    d.send_key.clear();
                    d.signature.clear();
                }
                s.error = error;
                cx.notify();
            });
        })
        .detach();
    }
    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(d) = self.drafts.get_mut(&(self.server, self.channel)) {
            d.files.retain(|f| f.id != id);
            d.send_key.clear();
            d.signature.clear();
        }
        cx.notify();
    }
    /// Parent calls this instead of plain send when has_files() is true. Message
    /// IDs and send key survive retries until text, reply or attachments change.
    pub fn send(&mut self, content: &str, reply: i64, cx: &mut Context<Self>) {
        if self.busy || !self.writable || self.channel <= 0 {
            return;
        }
        let key = (self.server, self.channel);
        let Some(d) = self.drafts.get_mut(&key) else {
            return;
        };
        if d.files.is_empty() || content.chars().count() > 5120 {
            return;
        }
        let signature = format!("{}\u{0}{}", reply, content.trim());
        if d.send_key.is_empty() || d.signature != signature {
            d.send_key = uuid::Uuid::new_v4().simple().to_string();
            d.signature = signature;
        }
        let files = d.files.clone();
        let send_key = d.send_key.clone();
        let content = content.trim().to_owned();
        let generation = self.generation;
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        self.busy = true;
        self.error.clear();
        cx.notify();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(async move {
                    let kind = if key.0 == 0 {
                        "conversations"
                    } else {
                        "channels"
                    };
                    let mut ids = Vec::new();
                    for f in files {
                        client
                            .upload_attachment(kind, key.1, &f.id, &f.name, (*f.data).clone())
                            .await?;
                        ids.push(f.id);
                    }
                    if key.0 == 0 {
                        client
                            .create_conversation_message(key.1, &content, &send_key, &ids)
                            .await
                    } else {
                        client
                            .send_attached_message(key.0, key.1, &content, &send_key, &ids, reply)
                            .await
                    }
                })
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(e) = this.upgrade() {
                let _ = e.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    s.busy = false;
                    match result {
                        Ok(m) => {
                            s.drafts.remove(&key);
                            s.sent = Some(m);
                            s.revision += 1
                        }
                        Err(e) => s.error = e,
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn download(&mut self, file: Attachment, cx: &mut Context<Self>) {
        if self.busy || file.deleted {
            return;
        }
        let name = PathBuf::from(&file.filename)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let directory = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let prompt = cx.prompt_for_new_path(&directory, Some(&name));
        let generation = self.generation;
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(p))) => p,
                _ => {
                    if let Some(e) = this.upgrade() {
                        let _ = e.update(cx, |s, cx| {
                            if s.generation == generation {
                                s.busy = false;
                                cx.notify();
                            }
                        });
                    }
                    return;
                }
            };
            let Some(e) = this.upgrade() else { return };
            let task = e
                .update(cx, |s, cx| {
                    let client = s.client.clone();
                    let runtime = s.runtime.clone();
                    cx.background_spawn(async move {
                        let data = runtime
                            .block_on(client.download_attachment(&file.id))
                            .map_err(|e| e.to_string())?;
                        std::fs::write(path, data).map_err(|e| e.to_string())
                    })
                })
                .ok();
            let Some(task) = task else { return };
            let result = task.await;
            let _ = e.update(cx, |s, cx| {
                if s.generation != generation {
                    return;
                }
                s.busy = false;
                s.error = result.err().unwrap_or_default();
                cx.notify();
            });
        })
        .detach();
    }
}
fn read_files(paths: Vec<PathBuf>, count: usize, used: usize) -> (Vec<DraftFile>, String) {
    let mut files = Vec::new();
    let mut total = used;
    let mut error = String::new();
    for path in paths {
        if count + files.len() >= 10 {
            error = "Можно прикрепить не более 10 файлов.".into();
            break;
        }
        let file = match std::fs::File::open(&path) {
            Ok(v) => v,
            Err(_) => {
                error = "Не удалось открыть файл.".into();
                continue;
            }
        };
        let size = match file.metadata() {
            Ok(m) if m.is_file() && m.len() > 0 && m.len() <= FILE_LIMIT as u64 => m.len() as usize,
            _ => {
                error = "Файл пуст, недоступен или превышает 64 МиБ.".into();
                continue;
            }
        };
        if total + size > DRAFT_LIMIT {
            error = "Всего в черновиках допускается до 128 МиБ файлов.".into();
            break;
        }
        let mut data = Vec::new();
        if file
            .take((FILE_LIMIT + 1) as u64)
            .read_to_end(&mut data)
            .is_err()
            || data.is_empty()
            || data.len() > FILE_LIMIT
            || total + data.len() > DRAFT_LIMIT
        {
            error = "Не удалось прочитать файл в допустимых пределах.".into();
            continue;
        }
        total += data.len();
        files.push(DraftFile {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            data: Arc::new(data),
        });
    }
    (files, error)
}
impl Render for Attachments {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .text_color(rgb(0xf3f4eb));
        if self.writable {
            root = root.child(
                div()
                    .id("attachment-pick")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.pick(cx)))
                    .child("Прикрепить файлы"),
            );
        }
        if let Some(d) = self.drafts.get(&(self.server, self.channel)) {
            for (index, f) in d.files.iter().enumerate() {
                let id = f.id.clone();
                root = root.child(
                    div().flex().gap_2().child(f.name.clone()).child(
                        div()
                            .id(("remove-attachment", index))
                            .cursor_pointer()
                            .on_click(cx.listener(move |s, _, _, cx| s.remove(&id, cx)))
                            .child("Удалить"),
                    ),
                );
            }
        }
        for (index, file) in self.message_files.iter().enumerate() {
            let file = file.clone();
            let label = format!("Сохранить: {}", file.filename);
            root = root.child(
                div()
                    .id(("download-attachment", index))
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, _, cx| s.download(file.clone(), cx)))
                    .child(label),
            );
        }
        root.child(if self.busy {
            "Обработка файла…"
        } else {
            ""
        })
        .child(div().text_color(rgb(0xff7777)).child(self.error.clone()))
    }
}
