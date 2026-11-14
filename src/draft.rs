//! Composer drafts ported from Go `internal/workspace/model.go` and
//! `attachments.go`.
//!
//! A draft belongs to one chat key: a channel, a personal conversation or a
//! thread inside a channel. It keeps the text, the reply target, the selected
//! files as `id/path/name/size/mtime` metadata, and the idempotency key of the
//! send currently associated with that composition. Bytes never live here, so
//! a 60 MiB attachment costs a few hundred bytes of draft state.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// Go `maxAttachmentSize`.
pub const MAX_FILE_BYTES: u64 = 64 << 20;
/// Go `maxDraftAttachmentBytes`, shared by all drafts of the account.
pub const MAX_DRAFT_BYTES: u64 = 128 << 20;
/// Go `MaxContent`.
pub const MAX_CONTENT: usize = 5120;
/// Go `len(d.Attachments) >= 10`.
pub const MAX_FILES: usize = 10;

/// A chat identified by its scope, exactly like Go `ChannelKey` plus the
/// composer's thread root (`0` for the main timeline).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChatKey {
    /// `0` means a personal conversation.
    pub server: i64,
    pub channel: i64,
    pub root: i64,
}

impl ChatKey {
    pub fn channel(server: i64, channel: i64) -> Self {
        Self {
            server,
            channel,
            root: 0,
        }
    }

    pub fn conversation(id: i64) -> Self {
        Self {
            server: 0,
            channel: id,
            root: 0,
        }
    }

    #[allow(dead_code)] // thread drafts are part of the key model (R04 UI)
    pub fn thread(server: i64, channel: i64, root: i64) -> Self {
        Self {
            server,
            channel,
            root,
        }
    }

    pub fn is_conversation(self) -> bool {
        self.server == 0
    }

    /// Backend route segment for the attachment upload.
    pub fn attachment_kind(self) -> &'static str {
        if self.is_conversation() {
            "conversations"
        } else {
            "channels"
        }
    }
}

/// One selected attachment. `path` is the only owner of the bytes; the draft
/// stores what `os.Stat` reported when the user attached the file so the send
/// can detect a replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftFile {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    /// `mtime` in nanoseconds since the Unix epoch, like Go `UnixNano`.
    pub modified: Option<u64>,
    /// A clipboard picture written by this client; removed with the draft.
    pub temporary: bool,
}

fn modified_nanos(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos() as u64)
}

impl DraftFile {
    /// Go `newAttachment`: only a regular, non-empty file up to 64 MiB.
    pub fn from_path(path: PathBuf) -> Result<Self, String> {
        let metadata = std::fs::metadata(&path).map_err(|_| {
            "Не удалось прикрепить файл: Файл пуст, недоступен или превышает 64 МиБ.".to_string()
        })?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
            return Err("Файл пуст, недоступен или превышает 64 МиБ.".into());
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.is_empty() {
            return Err("Не удалось прикрепить файл: недопустимое имя.".into());
        }
        Ok(Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            path,
            name,
            size: metadata.len(),
            modified: modified_nanos(&metadata),
            temporary: false,
        })
    }

    /// A picture read from the Windows clipboard. The caller writes it to a
    /// temporary file first; it is owned by the draft and removed with it.
    pub fn from_temporary(path: PathBuf, name: &str) -> Result<Self, String> {
        let mut file = Self::from_path(path)?;
        file.name = name.to_string();
        file.temporary = true;
        Ok(file)
    }

    /// Go `uploadDraftAttachment`: reopen the same file and refuse to send if
    /// it was deleted, replaced or modified after it was attached.
    pub fn open_verified(&self) -> Result<std::fs::File, String> {
        let file = std::fs::File::open(&self.path)
            .map_err(|_| "Файл вложения недоступен. Прикрепите его заново.".to_string())?;
        let metadata = file
            .metadata()
            .map_err(|_| "Файл вложения недоступен. Прикрепите его заново.".to_string())?;
        if metadata.len() != self.size
            || self
                .modified
                .is_some_and(|value| modified_nanos(&metadata) != Some(value))
        {
            return Err("Файл изменился после добавления. Прикрепите его заново.".into());
        }
        Ok(file)
    }

    /// Remove a temporary clipboard file. User files stay untouched.
    pub fn cleanup(&self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Draft {
    pub text: String,
    pub reply: i64,
    pub files: Vec<DraftFile>,
    /// Idempotency key for the next send of this exact composition.
    pub send_key: String,
    /// Bumped by every intentional change of text, reply or files.
    pub revision: u64,
}

/// The immutable snapshot handed to the background send task.
#[derive(Clone, Debug)]
pub struct SendPlan {
    pub key: ChatKey,
    /// Trimmed content sent to the server.
    pub content: String,
    /// Raw composer text at the moment of the send; the late result must not
    /// clear a draft the user edited meanwhile.
    pub composer_text: String,
    pub reply: i64,
    pub files: Vec<DraftFile>,
    pub send_key: String,
    pub revision: u64,
}

impl SendPlan {
    /// Attachment upload destination (the channel or conversation id).
    pub fn destination(&self) -> i64 {
        self.key.channel
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    /// The composition did not change while the send was in flight.
    Cleared,
    /// The user edited the draft; the sent files were removed, the rest stays.
    Kept,
}

pub type Shared = std::rc::Rc<std::cell::RefCell<Drafts>>;

#[derive(Default)]
pub struct Drafts {
    entries: HashMap<ChatKey, Draft>,
}

impl Drafts {
    pub fn shared() -> Shared {
        std::rc::Rc::new(std::cell::RefCell::new(Self::default()))
    }

    #[allow(dead_code)] // used by tests and by the thread reply UI in R04
    pub fn get(&self, key: ChatKey) -> Option<&Draft> {
        self.entries.get(&key)
    }

    pub fn text(&self, key: ChatKey) -> String {
        self.entries
            .get(&key)
            .map(|draft| draft.text.clone())
            .unwrap_or_default()
    }

    pub fn files(&self, key: ChatKey) -> Vec<DraftFile> {
        self.entries
            .get(&key)
            .map(|draft| draft.files.clone())
            .unwrap_or_default()
    }

    #[allow(dead_code)] // the reply strip is wired with message actions in R04
    pub fn reply(&self, key: ChatKey) -> i64 {
        self.entries.get(&key).map_or(0, |draft| draft.reply)
    }

    #[allow(dead_code)] // kept for late-result checks of the reply strip
    pub fn revision(&self, key: ChatKey) -> u64 {
        self.entries.get(&key).map_or(0, |draft| draft.revision)
    }

    /// Total resident bytes of all selected files, like Go
    /// `Model.attachmentBytes`.
    pub fn bytes(&self) -> u64 {
        self.entries
            .values()
            .flat_map(|draft| draft.files.iter())
            .map(|file| file.size.min(MAX_FILE_BYTES))
            .sum()
    }

    #[allow(dead_code)] // convenience mirror of `files().is_empty()`
    pub fn has_files(&self, key: ChatKey) -> bool {
        self.entries
            .get(&key)
            .is_some_and(|draft| !draft.files.is_empty())
    }

    /// Replace the draft text. Any change invalidates the idempotency key,
    /// because the next send is a different composition (Go `SetDraft`).
    pub fn set_text(&mut self, key: ChatKey, text: &str) -> bool {
        let draft = self.entries.entry(key).or_default();
        if draft.text == text {
            return false;
        }
        draft.text = text.to_string();
        draft.revision += 1;
        draft.send_key.clear();
        true
    }

    /// Go `setReply`.
    #[allow(dead_code)] // the reply target arrives with R04 message actions
    pub fn set_reply(&mut self, key: ChatKey, reply: i64) -> bool {
        let draft = self.entries.entry(key).or_default();
        if draft.reply == reply {
            return false;
        }
        draft.reply = reply;
        draft.revision += 1;
        draft.send_key.clear();
        true
    }

    /// Add metadata for already validated files. Mirrors Go
    /// `AddAttachmentPaths`: accepted files stay even when a later one hits a
    /// limit, and the caller shows the message.
    pub fn add_files(&mut self, key: ChatKey, files: Vec<DraftFile>) -> (usize, String) {
        let mut available = MAX_DRAFT_BYTES.saturating_sub(self.bytes());
        let mut added = 0;
        let mut error = String::new();
        let mut accepted = Vec::new();
        for file in files {
            let count = self.entries.get(&key).map_or(0, |draft| draft.files.len()) + added;
            if count >= MAX_FILES {
                error = "Можно прикрепить не более 10 файлов.".into();
                break;
            }
            if file.size > available {
                error = "Всего в черновиках допускается до 128 МиБ файлов.".into();
                break;
            }
            available = available.saturating_sub(file.size);
            added += 1;
            accepted.push(file);
        }
        if added > 0 {
            let draft = self.entries.entry(key).or_default();
            draft.files.extend(accepted);
            draft.revision += 1;
            draft.send_key.clear();
        }
        (added, error)
    }

    /// Remove one file and return it so the caller can release a temporary
    /// clipboard copy.
    pub fn remove_file(&mut self, key: ChatKey, id: &str) -> Option<DraftFile> {
        let draft = self.entries.get_mut(&key)?;
        let position = draft.files.iter().position(|file| file.id == id)?;
        let file = draft.files.remove(position);
        draft.revision += 1;
        draft.send_key.clear();
        Some(file)
    }

    /// Validate the current composition and freeze a `SendPlan`. The
    /// idempotency key is created once and stays with the draft until the
    /// composition changes or the send succeeds.
    pub fn begin_send(&mut self, key: ChatKey, composer_text: &str) -> Result<SendPlan, String> {
        self.set_text(key, composer_text);
        let draft = self.entries.entry(key).or_default();
        validate_send(&draft.text, &draft.files)?;
        if draft.send_key.is_empty() {
            draft.send_key = uuid::Uuid::new_v4().simple().to_string();
        }
        Ok(SendPlan {
            key,
            content: draft.text.trim().to_string(),
            composer_text: composer_text.to_string(),
            reply: draft.reply,
            files: draft.files.clone(),
            send_key: draft.send_key.clone(),
            revision: draft.revision,
        })
    }

    /// Apply a confirmed send. Go compares `Draft.Revision`; the raw composer
    /// text is compared as well because the UI can change between two sends.
    pub fn finish_send(
        &mut self,
        key: ChatKey,
        sent_revision: u64,
        sent_text: &str,
        sent_files: &[String],
        composer_now: &str,
    ) -> Finish {
        let Some(draft) = self.entries.get_mut(&key) else {
            return Finish::Cleared;
        };
        if draft.revision == sent_revision && sent_text == composer_now {
            let files = std::mem::take(&mut draft.files);
            for file in &files {
                file.cleanup();
            }
            draft.text.clear();
            draft.reply = 0;
            draft.send_key.clear();
            draft.revision += 1;
            return Finish::Cleared;
        }
        draft.text = composer_now.to_string();
        let removed: Vec<_> = draft
            .files
            .iter()
            .filter(|file| sent_files.contains(&file.id))
            .cloned()
            .collect();
        draft.files.retain(|file| !sent_files.contains(&file.id));
        for file in &removed {
            file.cleanup();
        }
        draft.send_key.clear();
        Finish::Kept
    }

    pub fn clear_server(&mut self, server: i64) {
        let keys: Vec<_> = self
            .entries
            .keys()
            .copied()
            .filter(|key| key.server == server)
            .collect();
        for key in keys {
            self.clear(key);
        }
    }

    pub fn clear(&mut self, key: ChatKey) {
        if let Some(draft) = self.entries.remove(&key) {
            for file in &draft.files {
                file.cleanup();
            }
        }
    }

    pub fn clear_all(&mut self) {
        for (_, draft) in self.entries.drain() {
            for file in &draft.files {
                file.cleanup();
            }
        }
    }
}

impl Drop for Drafts {
    fn drop(&mut self) {
        self.clear_all();
    }
}

/// Go `ValidateContent`, shared with the send button state.
pub fn validate_content(text: &str) -> Result<(), String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Введите сообщение.".into());
    }
    if trimmed.contains('\0') || text.contains('\0') {
        return Err("Сообщение содержит недопустимый символ.".into());
    }
    if trimmed.chars().count() > MAX_CONTENT {
        return Err("Максимум 5120 символов.".into());
    }
    Ok(())
}

/// Go `validateDraft`: an attachment set allows empty text.
pub fn validate_send(text: &str, files: &[DraftFile]) -> Result<(), String> {
    if !files.is_empty() && text.trim().is_empty() {
        // Still reject NUL and an oversized body.
        if text.contains('\0') {
            return Err("Сообщение содержит недопустимый символ.".into());
        }
        if text.trim().chars().count() > MAX_CONTENT {
            return Err("Максимум 5120 символов.".into());
        }
        if files.len() > MAX_FILES {
            return Err("Можно прикрепить не более 10 файлов.".into());
        }
        return Ok(());
    }
    validate_content(text)
}

/// Displayed counter `n / 5120` once the text is close to the limit.
pub fn counter(text: &str) -> Option<String> {
    let count = text.trim().chars().count();
    (count > MAX_CONTENT - 120).then(|| format!("{count} / {MAX_CONTENT}"))
}

/// User-facing text for an upload/send failure. Go shows `HTTPError.Error()`
/// for transport errors and the local I/O text for draft problems.
pub fn describe_http(status: u16) -> &'static str {
    match status {
        401 => "Сессия истекла. Войдите снова.",
        403 => "Недостаточно прав доступа.",
        404 => "Сервер или канал больше недоступен.",
        409 => "Это сообщение уже отправлено. Обновите историю.",
        413 => "Файл превышает 64 МиБ.",
        429 => "Слишком много запросов. Повторите позже.",
        507 => "Хранилище сервера заполнено.",
        _ => "Не удалось отправить сообщение. Повторите запрос.",
    }
}

/// Join a user-visible draft error with the HTTP text without leaking URLs or
/// local paths.
pub fn describe_error(error: &voxhold_api::Error) -> String {
    match error {
        voxhold_api::Error::Http(status) => describe_http(*status).to_string(),
        other => other.to_string(),
    }
}

/// Resolve a clipboard or drop path against the current directory. Kept pure so
/// the limit checks are testable without the Windows shell.
#[allow(dead_code)] // used by tests; the drop adapter passes absolute shell paths
pub fn normalize_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn relative_paths_are_resolved() {
        let resolved = normalize_path(Path::new("file.bin"));
        assert!(resolved.is_absolute());
        let absolute = std::env::temp_dir().join("absolute.bin");
        assert_eq!(normalize_path(&absolute), absolute);
    }

    fn temp_file(name: &str, bytes: usize) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "voxhold-draft-{}-{}-{name}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![7; bytes]).unwrap();
        path
    }

    fn file(name: &str, bytes: usize) -> DraftFile {
        DraftFile::from_path(temp_file(name, bytes)).unwrap()
    }

    #[test]
    fn drafts_are_isolated_by_chat_and_thread() {
        let mut drafts = Drafts::default();
        let channel = ChatKey::channel(1, 7);
        let conversation = ChatKey::conversation(7);
        let thread = ChatKey::thread(1, 7, 99);
        assert!(drafts.set_text(channel, "в канале"));
        assert!(drafts.set_text(conversation, "в личке"));
        assert!(drafts.set_text(thread, "в ветке"));
        assert_eq!(drafts.text(channel), "в канале");
        assert_eq!(drafts.text(conversation), "в личке");
        assert_eq!(drafts.text(thread), "в ветке");
        drafts.clear(conversation);
        assert_eq!(drafts.text(conversation), "");
        assert_eq!(drafts.text(channel), "в канале");
        assert!(!drafts.set_text(channel, "в канале"), "same text");
        assert_eq!(drafts.revision(channel), 1);
    }

    #[test]
    fn send_key_survives_a_retry_and_rotates_on_change() {
        let mut drafts = Drafts::default();
        let key = ChatKey::channel(1, 7);
        drafts.set_text(key, "привет");
        let first = drafts.begin_send(key, "привет").unwrap();
        let retry = drafts.begin_send(key, "привет").unwrap();
        assert_eq!(first.send_key, retry.send_key);
        assert_eq!(first.revision, retry.revision);
        assert_eq!(first.send_key.len(), 32);

        drafts.set_text(key, "привет!");
        let changed = drafts.begin_send(key, "привет!").unwrap();
        assert_ne!(changed.send_key, first.send_key);
        assert!(changed.revision > first.revision);
    }

    #[test]
    fn attachments_allow_empty_text_and_reject_bad_content() {
        let mut drafts = Drafts::default();
        let key = ChatKey::channel(1, 7);
        let plan = drafts.begin_send(key, "").unwrap_err();
        assert_eq!(plan, "Введите сообщение.");

        let file = file("only-file.bin", 4);
        drafts.add_files(key, vec![file]);
        let plan = drafts.begin_send(key, "   ").unwrap();
        assert_eq!(plan.content, "");
        assert_eq!(plan.files.len(), 1);

        assert_eq!(
            validate_content(&"я".repeat(MAX_CONTENT + 1)),
            Err("Максимум 5120 символов.".into())
        );
        assert_eq!(
            validate_content("текст\0"),
            Err("Сообщение содержит недопустимый символ.".into())
        );
        assert!(validate_content(&"я".repeat(MAX_CONTENT)).is_ok());
    }

    #[test]
    fn limits_match_go_messages() {
        let mut drafts = Drafts::default();
        let key = ChatKey::channel(1, 7);
        let ten: Vec<_> = (0..10)
            .map(|index| file(&format!("f{index}.bin"), 2))
            .collect();
        let (added, error) = drafts.add_files(key, ten);
        assert_eq!(added, 10);
        assert!(error.is_empty());
        let (added, error) = drafts.add_files(key, vec![file("eleventh.bin", 2)]);
        assert_eq!(added, 0);
        assert_eq!(error, "Можно прикрепить не более 10 файлов.");

        let mut drafts = Drafts::default();
        let key = ChatKey::channel(2, 7);
        let big = || DraftFile {
            id: uuid::Uuid::new_v4().simple().to_string(),
            path: PathBuf::from("missing"),
            name: "big.bin".into(),
            size: MAX_FILE_BYTES,
            modified: None,
            temporary: false,
        };
        let (added, error) = drafts.add_files(key, vec![big(), big()]);
        assert_eq!(added, 2);
        assert!(error.is_empty());
        let (added, error) = drafts.add_files(key, vec![file("small.bin", 2)]);
        assert_eq!(added, 0);
        assert_eq!(error, "Всего в черновиках допускается до 128 МиБ файлов.");
        // Clean the synthetic file entries without touching a real path.
        drafts.entries.get_mut(&key).unwrap().files.clear();
    }

    #[test]
    fn empty_or_missing_files_are_refused() {
        assert_eq!(
            DraftFile::from_path(PathBuf::from("no-such-file")).unwrap_err(),
            "Не удалось прикрепить файл: Файл пуст, недоступен или превышает 64 МиБ."
        );
        assert_eq!(
            DraftFile::from_path(temp_file("empty.bin", 0)).unwrap_err(),
            "Файл пуст, недоступен или превышает 64 МиБ."
        );
    }

    #[test]
    fn changed_file_is_refused_before_upload() {
        let path = temp_file("changed.bin", 8);
        let file = DraftFile::from_path(path.clone()).unwrap();
        assert!(file.open_verified().is_ok());
        std::fs::write(&path, vec![1; 9]).unwrap();
        assert_eq!(
            file.open_verified().unwrap_err(),
            "Файл изменился после добавления. Прикрепите его заново."
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            file.open_verified().unwrap_err(),
            "Файл вложения недоступен. Прикрепите его заново."
        );
    }

    #[test]
    fn late_send_clears_only_the_unchanged_draft() {
        let mut drafts = Drafts::default();
        let key = ChatKey::channel(1, 7);
        drafts.set_text(key, "отправлено");
        let plan = drafts.begin_send(key, "отправлено").unwrap();
        assert_eq!(
            drafts.finish_send(key, plan.revision, "отправлено", &[], "отправлено"),
            Finish::Cleared
        );
        assert_eq!(drafts.text(key), "");
        assert!(drafts.get(key).unwrap().send_key.is_empty());

        // The user types while the request is in flight.
        drafts.set_text(key, "новый текст");
        let plan = drafts.begin_send(key, "новый текст").unwrap();
        drafts.set_text(key, "новый текст и ещё");
        assert_eq!(
            drafts.finish_send(key, plan.revision, "новый текст", &[], "новый текст и ещё"),
            Finish::Kept
        );
        assert_eq!(drafts.text(key), "новый текст и ещё");
    }

    #[test]
    fn sent_attachments_are_removed_but_new_ones_survive() {
        let mut drafts = Drafts::default();
        let key = ChatKey::channel(1, 7);
        let first = file("first.bin", 3);
        let first_id = first.id.clone();
        let first_path = first.path.clone();
        drafts.add_files(key, vec![first]);
        let plan = drafts.begin_send(key, "с файлом").unwrap();
        drafts.add_files(key, vec![file("second.bin", 3)]);
        assert_eq!(
            drafts.finish_send(key, plan.revision, "с файлом", &[first_id], "с файлом"),
            Finish::Kept
        );
        let files = drafts.files(key);
        assert_eq!(files.len(), 1);
        assert!(files[0].name.ends_with("second.bin"));
        assert!(first_path.exists(), "the user file on disk stays untouched");
    }

    #[test]
    fn conversation_and_channel_keys_pick_the_right_route() {
        assert_eq!(ChatKey::channel(1, 7).attachment_kind(), "channels");
        assert_eq!(ChatKey::conversation(7).attachment_kind(), "conversations");
        assert!(ChatKey::conversation(7).is_conversation());
        assert!(!ChatKey::thread(1, 7, 9).is_conversation());
    }

    #[test]
    fn temporary_files_are_removed_with_the_draft() {
        let path = temp_file("clipboard.png", 4);
        let file = DraftFile::from_temporary(path.clone(), "clipboard.png").unwrap();
        assert!(file.temporary);
        let mut drafts = Drafts::default();
        let key = ChatKey::channel(1, 7);
        drafts.add_files(key, vec![file]);
        drafts.clear(key);
        assert!(!path.exists());
    }
}
