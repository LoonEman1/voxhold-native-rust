//! Resource loader for the shipped `assets/` directory.
//!
//! The Windows bundle puts `assets/` next to the EXE. In a development tree the
//! same files are found through `CARGO_MANIFEST_DIR` or the current directory.
//! A missing file can be looked up in the other resource locations.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

pub struct Assets {
    roots: Vec<PathBuf>,
}

impl Assets {
    pub fn discover() -> Self {
        Self {
            roots: discover_roots(),
        }
    }

    pub fn path(&self, resource: &str) -> Option<PathBuf> {
        let path = Path::new(resource);
        if !path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        self.roots
            .iter()
            .map(|root| root.join(path))
            .find(|path| path.is_file())
    }
}

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(path) = self.path(path) {
            return Ok(Some(Cow::Owned(std::fs::read(path)?)));
        }
        Ok(None)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        let dir = self
            .roots
            .iter()
            .map(|root| root.join(path))
            .find(|dir| dir.is_dir());
        let mut entries = Vec::new();
        if let Some(read) = dir.and_then(|dir| std::fs::read_dir(dir).ok()) {
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

fn discover_roots() -> Vec<PathBuf> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource;

    #[test]
    fn finds_the_repository_assets_in_development() {
        let assets = Assets::discover();
        assert!(assets.path("icons/hash.svg").is_some());
        assert!(assets.load("icons/hash.svg").unwrap().is_some());
        assert!(assets.load("icons/missing.svg").unwrap().is_none());
    }
}
