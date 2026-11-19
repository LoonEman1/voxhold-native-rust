//! The one local preference that survives a restart: whether to remember the
//! instance address, and the canonical origin itself.
//!
//! Only a canonical origin is ever written. Credentials belong to `W09g` and
//! are deliberately absent from this file. The JSON shape is stable:
//! `{"remember_instance":true,"instance":"https://host:port"}`; with the box
//! unchecked the origin is dropped and the file is `{"remember_instance":false}`.

use std::path::{Path, PathBuf};

/// Readable failure text shown by the address step. A failed write never
/// cancels the confirmed origin or the transition to the login step.
pub const SAVE_ERROR: &str = "Не удалось сохранить адрес. Проверьте доступ к файлу настроек.";

/// Local instance preference. One owner for both the checkbox and the origin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstancePrefs {
    pub remember: bool,
    pub instance: Option<String>,
}

impl InstancePrefs {
    /// `%APPDATA%\Voxhold\instance.json`, with the same fallbacks as the
    /// workspace layout file.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("Voxhold").join("instance.json"))
    }

    /// Missing file, broken JSON and an invalid origin all resolve to the
    /// default (unchecked, no address); nothing here may panic at startup.
    pub fn load(path: &Path) -> Self {
        if path.as_os_str().is_empty() {
            return Self::default();
        }
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        Self::parse(&bytes)
    }

    /// Write atomically. `remember == false` stores no origin at all; with the
    /// box checked only a canonical origin is accepted.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        let body = self.document()?;
        write_atomic(path, body.as_bytes())
    }

    fn document(&self) -> std::io::Result<String> {
        if !self.remember {
            return Ok("{\"remember_instance\":false}".to_string());
        }
        let raw = self.instance.as_deref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "remembered instance has no origin",
            )
        })?;
        let origin = canonical_origin(raw).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "remembered instance is not a valid origin",
            )
        })?;
        let quoted = serde_json::to_string(&origin)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        Ok(format!(
            "{{\"remember_instance\":true,\"instance\":{quoted}}}"
        ))
    }

    fn parse(bytes: &[u8]) -> Self {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            return Self::default();
        };
        let Some(remember) = value.get("remember_instance").and_then(|v| v.as_bool()) else {
            return Self::default();
        };
        if !remember {
            return Self::default();
        }
        let Some(raw) = value.get("instance").and_then(|v| v.as_str()) else {
            return Self::default();
        };
        match canonical_origin(raw) {
            Some(instance) => Self {
                remember: true,
                instance: Some(instance),
            },
            None => Self::default(),
        }
    }
}

/// The same normalization the login form and the HTTP/WS client use. A raw
/// value that is not a valid instance is rejected instead of being guessed.
pub fn canonical_origin(raw: &str) -> Option<String> {
    voxhold_api::Client::new(raw)
        .ok()
        .map(|client| client.origin())
}

/// Unique temporary file in the target directory, flushed and renamed over the
/// destination. A failed rename removes the temporary file and leaves the
/// previous document untouched.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "instance.json".to_string());
    let mut attempt = 0u32;
    let temp = loop {
        let candidate = dir.join(format!(".{name}.{}.{attempt}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                use std::io::Write;
                if let Err(error) = file.write_all(data).and_then(|_| file.sync_all()) {
                    drop(file);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error);
                }
                break candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
                if attempt > 16 {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    };
    let result = std::fs::rename(&temp, path);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "voxhold-instance-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join(name))
    }

    fn prefs(remember: bool, instance: Option<&str>) -> InstancePrefs {
        InstancePrefs {
            remember,
            instance: instance.map(str::to_owned),
        }
    }

    #[test]
    fn a_missing_file_is_the_default() {
        let (dir, path) = scratch("instance.json");
        assert_eq!(InstancePrefs::load(&path), InstancePrefs::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_valid_record_round_trips_with_the_stable_shape() {
        let (dir, path) = scratch("instance.json");
        let saved = prefs(true, Some("https://chat.example.org:8443"));
        saved.save(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"remember_instance\":true,\"instance\":\"https://chat.example.org:8443\"}"
        );
        assert_eq!(InstancePrefs::load(&path), saved);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_json_and_invalid_origins_are_ignored() {
        let (dir, path) = scratch("instance.json");
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(InstancePrefs::load(&path), InstancePrefs::default());

        std::fs::write(
            &path,
            b"{\"remember_instance\":true,\"instance\":\"http://example.com\"}",
        )
        .unwrap();
        assert_eq!(InstancePrefs::load(&path), InstancePrefs::default());

        std::fs::write(&path, b"{\"remember_instance\":true}").unwrap();
        assert_eq!(InstancePrefs::load(&path), InstancePrefs::default());

        std::fs::write(&path, b"{\"remember_instance\":\"yes\"}").unwrap();
        assert_eq!(InstancePrefs::load(&path), InstancePrefs::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unchecked_box_stores_no_origin() {
        let (dir, path) = scratch("instance.json");
        prefs(true, Some("https://chat.example.org"))
            .save(&path)
            .unwrap();
        prefs(false, Some("https://chat.example.org"))
            .save(&path)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"remember_instance\":false}"
        );
        assert_eq!(InstancePrefs::load(&path), InstancePrefs::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_default_https_port_and_loopback_are_canonicalized() {
        let (dir, path) = scratch("instance.json");
        prefs(true, Some("https://chat.example.org:443/"))
            .save(&path)
            .unwrap();
        assert_eq!(
            InstancePrefs::load(&path),
            prefs(true, Some("https://chat.example.org"))
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"remember_instance\":true,\"instance\":\"https://chat.example.org\"}"
        );

        prefs(true, Some("http://127.0.0.1:8080/"))
            .save(&path)
            .unwrap();
        assert_eq!(
            InstancePrefs::load(&path),
            prefs(true, Some("http://127.0.0.1:8080"))
        );

        // A hand-edited, still valid but non-canonical record is normalized on
        // load instead of being discarded.
        std::fs::write(
            &path,
            b"{\"remember_instance\":true,\"instance\":\"https://chat.example.org:443/\"}",
        )
        .unwrap();
        assert_eq!(
            InstancePrefs::load(&path),
            prefs(true, Some("https://chat.example.org"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn two_ports_are_kept_apart() {
        let (dir, path) = scratch("instance.json");
        prefs(true, Some("https://chat.example.org:8443"))
            .save(&path)
            .unwrap();
        let first = InstancePrefs::load(&path);
        prefs(true, Some("https://chat.example.org:9443"))
            .save(&path)
            .unwrap();
        let second = InstancePrefs::load(&path);
        assert_ne!(first, second);
        assert_eq!(
            first.instance.as_deref(),
            Some("https://chat.example.org:8443")
        );
        assert_eq!(
            second.instance.as_deref(),
            Some("https://chat.example.org:9443")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_invalid_or_missing_origin_is_never_written() {
        let (dir, path) = scratch("instance.json");
        assert!(prefs(true, Some("http://example.com")).save(&path).is_err());
        assert!(prefs(true, None).save(&path).is_err());
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_atomic_save_leaves_no_temporary_files() {
        let (dir, path) = scratch("instance.json");
        prefs(true, Some("https://chat.example.org"))
            .save(&path)
            .unwrap();
        prefs(true, Some("https://chat.example.org:8443"))
            .save(&path)
            .unwrap();
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["instance.json".to_string()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_empty_path_is_a_no_op() {
        assert!(prefs(true, Some("https://chat.example.org"))
            .save(Path::new(""))
            .is_ok());
        assert_eq!(InstancePrefs::load(Path::new("")), InstancePrefs::default());
    }

    #[test]
    fn a_directory_target_reports_an_error_and_keeps_the_old_document() {
        let (dir, path) = scratch("instance.json");
        prefs(true, Some("https://chat.example.org"))
            .save(&path)
            .unwrap();
        // Point the save at an existing directory: the rename cannot replace
        // it, so the previous file must stay intact.
        assert!(prefs(true, Some("https://chat.example.org:8443"))
            .save(&dir)
            .is_err());
        assert_eq!(
            InstancePrefs::load(&path),
            prefs(true, Some("https://chat.example.org"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
