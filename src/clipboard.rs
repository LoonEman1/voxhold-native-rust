//! Windows clipboard adapter for the composer, following Go
//! `view.go::OnPasteFiles`: clipboard file paths win, then an image, and only
//! then the ordinary text paste handled by the input widget.

use std::path::PathBuf;

/// Result of a composer paste attempt.
#[derive(Debug, Default)]
pub struct Paste {
    pub paths: Vec<PathBuf>,
    /// `PNG` clipboard payload that still has to be written to a temporary
    /// file before it can join a draft.
    pub image: Option<Vec<u8>>,
}

impl Paste {
    /// Go pastes attached paths first, otherwise the picture, otherwise the
    /// regular text. A non-empty result means the composer handled the paste.
    pub fn is_file_paste(&self) -> bool {
        !self.paths.is_empty() || self.image.is_some()
    }
}

#[cfg(windows)]
mod platform {
    use super::Paste;
    use std::{os::windows::ffi::OsStringExt, path::PathBuf, ptr};

    const CF_HDROP: u32 = 15;
    const MAX_CLIPBOARD_IMAGE: usize = 64 << 20;
    const MAX_DROP_PATHS: u32 = 100;
    const MAX_PATH_CHARS: u32 = 32_767;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: *mut core::ffi::c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut core::ffi::c_void;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn DragQueryFileW(
            drop: *mut core::ffi::c_void,
            index: u32,
            file: *mut u16,
            chars: u32,
        ) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalSize(handle: *mut core::ffi::c_void) -> usize;
        fn GlobalLock(handle: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn GlobalUnlock(handle: *mut core::ffi::c_void) -> i32;
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Open the clipboard for one short read. The UI never waits on another
    /// window's clipboard: a failed open means "nothing to paste".
    fn with_clipboard<T>(read: impl FnOnce() -> T) -> Option<T> {
        unsafe {
            if OpenClipboard(ptr::null_mut()) == 0 {
                return None;
            }
            let value = read();
            CloseClipboard();
            Some(value)
        }
    }

    fn hdrop_paths(handle: *mut core::ffi::c_void) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        unsafe {
            let mut count = DragQueryFileW(handle, u32::MAX, ptr::null_mut(), 0);
            count = count.min(MAX_DROP_PATHS);
            for index in 0..count {
                let chars = DragQueryFileW(handle, index, ptr::null_mut(), 0);
                if chars == 0 || chars > MAX_PATH_CHARS {
                    continue;
                }
                let mut buffer = vec![0u16; chars as usize + 1];
                let written =
                    DragQueryFileW(handle, index, buffer.as_mut_ptr(), buffer.len() as u32);
                if written == 0 {
                    continue;
                }
                buffer.truncate(written as usize);
                paths.push(PathBuf::from(std::ffi::OsString::from_wide(&buffer)));
            }
        }
        paths
    }

    fn files() -> Vec<PathBuf> {
        with_clipboard(|| unsafe {
            let handle = GetClipboardData(CF_HDROP);
            if handle.is_null() {
                Vec::new()
            } else {
                hdrop_paths(handle)
            }
        })
        .unwrap_or_default()
    }

    fn png() -> Option<Vec<u8>> {
        let format = unsafe { RegisterClipboardFormatW(wide("PNG").as_ptr()) };
        if format == 0 {
            return None;
        }
        with_clipboard(|| unsafe {
            if IsClipboardFormatAvailable(format) == 0 {
                return None;
            }
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let size = GlobalSize(handle);
            if size == 0 || size > MAX_CLIPBOARD_IMAGE {
                return None;
            }
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                return None;
            }
            let mut bytes = vec![0u8; size];
            ptr::copy_nonoverlapping(pointer as *const u8, bytes.as_mut_ptr(), size);
            GlobalUnlock(handle);
            Some(bytes)
        })
        .flatten()
    }

    pub fn read() -> Paste {
        let paths = files();
        if !paths.is_empty() {
            return Paste { paths, image: None };
        }
        Paste {
            paths: Vec::new(),
            image: png(),
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Paste;

    pub fn read() -> Paste {
        Paste::default()
    }
}

pub fn read() -> Paste {
    platform::read()
}

/// Write a clipboard picture to a unique file owned by the draft. The caller
/// creates it through `DraftFile::from_temporary`, which removes it with the
/// draft; a write error also removes the partial file here.
pub fn write_png(bytes: &[u8]) -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "voxhold-clipboard-{}-{}.png",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    if let Err(error) = std::fs::write(&path, bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_paste_wins_over_the_picture() {
        let paste = Paste {
            paths: vec![PathBuf::from("C:/tmp/a.txt")],
            image: Some(vec![1]),
        };
        assert!(paste.is_file_paste());
        assert!(Paste::default().is_file_paste() == false);
        assert!(Paste {
            paths: Vec::new(),
            image: Some(vec![1])
        }
        .is_file_paste());
    }

    #[test]
    fn clipboard_pictures_get_unique_files_and_can_be_cleaned() {
        let first = write_png(&[0x89, b'P', b'N', b'G']).unwrap();
        let second = write_png(&[0x89, b'P', b'N', b'G']).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), [0x89, b'P', b'N', b'G']);
        std::fs::remove_file(&first).unwrap();
        std::fs::remove_file(&second).unwrap();
    }
}
