//! Resource loader for the shipped `assets/` directory.
//!
//! The Windows bundle puts `assets/` next to the EXE. In a development tree the
//! same files are found through `CARGO_MANIFEST_DIR` or the current directory.
//! A missing file returns `None`, so the UI can draw a fallback instead of
//! failing to start.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

pub struct Assets {
    root: PathBuf,
}

impl Assets {
    pub fn discover() -> Self {
        Self {
            root: discover_root(),
        }
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Diagnostic helper: returns the absolute path a resource would use.
    #[allow(dead_code)]
    pub fn path(&self, resource: &str) -> PathBuf {
        self.root.join(resource)
    }
}

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let path = self.root.join(path);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(Cow::Owned(bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        let dir = self.root.join(path);
        let mut entries = Vec::new();
        if let Ok(read) = std::fs::read_dir(dir) {
            for entry in read.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    entries.push(name.to_string().into());
                }
            }
        }
        entries.sort();
        Ok(entries)
    }
}

fn discover_root() -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("assets"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("assets"));
            }
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    candidates.push(PathBuf::from("assets"));
    candidates
        .into_iter()
        .find(|path| path.join("icons").is_dir())
        .unwrap_or_else(|| PathBuf::from("assets"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource;

    #[test]
    fn finds_the_repository_assets_in_development() {
        let assets = Assets::discover();
        assert!(assets.path("icons/hash.svg").is_file());
        assert!(assets.load("icons/hash.svg").unwrap().is_some());
        assert!(assets.load("icons/missing.svg").unwrap().is_none());
    }
}
