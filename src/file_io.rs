//! Streaming attachment save ported from Go `cmd/voxhold/main.go`
//! (`saveAttachment`) and `attachment_save_windows.go`.
//!
//! The download writes every chunk to a unique `.part` file next to the
//! chosen target. The target is replaced only after the body ended, the
//! expected size matched and the file handle was closed, so a broken transfer
//! or a locked destination never truncates the previous file.

use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

/// A unique temporary file in the destination directory. Dropping it without
/// `finish` removes the partial file and leaves the old target untouched.
pub struct PartFile {
    target: PathBuf,
    part: PathBuf,
    file: Option<File>,
    written: u64,
    finished: bool,
}

impl PartFile {
    pub fn create(target: &Path) -> Result<Self, String> {
        let directory = target.parent().filter(|path| !path.as_os_str().is_empty());
        let directory = directory.unwrap_or_else(|| Path::new("."));
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let part = directory.join(format!(".{name}.{}.part", uuid::Uuid::new_v4().simple()));
        let file = File::create(&part)
            .map_err(|_| "Не удалось сохранить файл: папка недоступна для записи.".to_string())?;
        Ok(Self {
            target: target.to_path_buf(),
            part,
            file: Some(file),
            written: 0,
            finished: false,
        })
    }

    /// Write one streamed chunk. The caller runs this outside the UI thread.
    pub fn write(&mut self, chunk: &[u8]) -> Result<(), String> {
        let Some(file) = self.file.as_mut() else {
            return Err("Загрузка уже завершена.".into());
        };
        file.write_all(chunk)
            .map_err(|_| "Не удалось записать файл на диск.".to_string())?;
        self.written = self.written.saturating_add(chunk.len() as u64);
        Ok(())
    }

    /// Bytes written so far; used by the download progress and by tests.
    #[allow(dead_code)]
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Close, verify the expected size and replace the target atomically.
    pub fn finish(mut self, expected: Option<u64>) -> Result<u64, String> {
        if let Some(expected) = expected {
            if self.written != expected {
                return Err("Размер загруженного файла не совпадает с ожидаемым.".to_string());
            }
        }
        if self.written == 0 {
            return Err("Сервер вернул пустой файл.".to_string());
        }
        if let Some(file) = self.file.take() {
            file.sync_all()
                .map_err(|_| "Не удалось записать файл на диск.".to_string())?;
            drop(file);
        }
        replace_file(&self.part, &self.target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                "Не удалось заменить файл. Закройте его в другой программе.".to_string()
            } else {
                "Не удалось заменить файл.".to_string()
            }
        })?;
        self.finished = true;
        Ok(self.written)
    }

    /// Remove the temporary file without touching the target.
    pub fn abort(mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.part);
        self.finished = true;
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.file.take();
        let _ = std::fs::remove_file(&self.part);
    }
}

/// Go `replaceDownloadedFile`: `MoveFileEx(MOVEFILE_REPLACE_EXISTING)` on
/// Windows so an existing target is replaced in one step.
#[cfg(windows)]
fn replace_file(part: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }

    let from: Vec<u16> = part
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let to: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(part: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(part, target)
}

/// Sanitize a server-provided filename into one path component. The user still
/// chooses the directory in the save dialog.
pub fn safe_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .trim_matches('.')
        .to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "voxhold-save-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn parts(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".part"))
            .collect()
    }

    #[test]
    fn successful_save_replaces_the_existing_target() {
        let dir = temp_dir();
        let target = dir.join("photo.png");
        std::fs::write(&target, b"old").unwrap();
        let mut save = PartFile::create(&target).unwrap();
        save.write(b"new ").unwrap();
        save.write(b"content").unwrap();
        assert_eq!(save.finish(Some(11)).unwrap(), 11);
        assert_eq!(std::fs::read(&target).unwrap(), b"new content");
        assert!(parts(&dir).is_empty(), "no temporary files stay behind");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn interrupted_transfer_keeps_the_old_file_and_removes_the_part() {
        let dir = temp_dir();
        let target = dir.join("report.pdf");
        std::fs::write(&target, b"previous").unwrap();
        {
            let mut save = PartFile::create(&target).unwrap();
            save.write(b"half").unwrap();
            // The HTTP body fails here; the caller drops the save.
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"previous");
        assert!(parts(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wrong_size_keeps_the_old_file() {
        let dir = temp_dir();
        let target = dir.join("report.pdf");
        std::fs::write(&target, b"previous").unwrap();
        let mut save = PartFile::create(&target).unwrap();
        save.write(b"short").unwrap();
        assert!(save.finish(Some(99)).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"previous");
        assert!(parts(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn empty_body_is_refused() {
        let dir = temp_dir();
        let target = dir.join("empty.bin");
        let save = PartFile::create(&target).unwrap();
        assert!(save.finish(None).is_err());
        assert!(!target.exists());
        assert!(parts(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parallel_saves_to_one_target_leave_no_parts() {
        let dir = temp_dir();
        let target = dir.join("same.bin");
        std::fs::write(&target, b"start").unwrap();
        let first_target = target.clone();
        let second_target = target.clone();
        let first = std::thread::spawn(move || {
            let mut save = PartFile::create(&first_target).unwrap();
            save.write(b"first").unwrap();
            save.finish(Some(5)).unwrap()
        });
        let second = std::thread::spawn(move || {
            let mut save = PartFile::create(&second_target).unwrap();
            save.write(b"second").unwrap();
            save.finish(Some(6)).unwrap()
        });
        first.join().unwrap();
        second.join().unwrap();
        let value = std::fs::read(&target).unwrap();
        assert!(value == b"first" || value == b"second");
        assert!(parts(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn locked_target_is_refused_and_keeps_the_old_bytes() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = temp_dir();
        let target = dir.join("locked.txt");
        std::fs::write(&target, b"previous").unwrap();
        let _lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&target)
            .unwrap();
        let mut save = PartFile::create(&target).unwrap();
        save.write(b"new").unwrap();
        assert!(save.finish(Some(3)).is_err());
        assert!(parts(&dir).is_empty(), "the .part is removed");
        drop(_lock);
        assert_eq!(std::fs::read(&target).unwrap(), b"previous");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn server_names_are_reduced_to_one_component() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("C:\\tmp\\photo.png"), "photo.png");
        assert_eq!(safe_file_name("a:b?c*"), "a_b_c_");
        assert_eq!(safe_file_name("   "), "attachment");
    }
}
