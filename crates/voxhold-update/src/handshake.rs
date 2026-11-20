//! Start confirmation written by the new version.
//!
//! The helper starts the new executable with [`ARG`] pointing at a handshake
//! path and [`OPERATION_ARG`] carrying the operation id. The new version writes
//! this file once its window exists; the helper accepts it only when the
//! operation id and the version match the operation it is applying. An older
//! application ignores [`OPERATION_ARG`] and writes no id, which an older helper
//! still accepts; a new helper treats the missing id as "not confirmed" and
//! rolls back instead of silently accepting an unknown start.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, UpdateError};

/// Argument that carries the handshake path.
pub const ARG: &str = "--update-handshake";
/// Argument that carries the operation id.
pub const OPERATION_ARG: &str = "--update-operation";
/// Schema of the handshake document.
pub const SCHEMA: u32 = 1;

/// Contents of the handshake file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    pub schema: u32,
    pub version: String,
    #[serde(default)]
    pub operation: Option<String>,
}

impl Handshake {
    /// Build a handshake for `version` and an optional operation id.
    pub fn new(version: impl Into<String>, operation: Option<String>) -> Self {
        Self {
            schema: SCHEMA,
            version: version.into(),
            operation,
        }
    }

    /// `true` when this document confirms `operation`.
    ///
    /// The version is informational: the operation id is unique per swap, so a
    /// build whose package version differs from `VOXHOLD_BUILD_VERSION` can
    /// still confirm its own start.
    pub fn confirms(&self, operation: &str) -> bool {
        self.schema == SCHEMA && self.operation.as_deref() == Some(operation)
    }
}

/// Read a handshake. A missing, malformed or foreign file is `None`.
pub fn read(path: &Path) -> Option<Handshake> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `true` when `path` confirms `operation`.
pub fn confirms(path: &Path, operation: &str) -> bool {
    read(path).is_some_and(|handshake| handshake.confirms(operation))
}

/// Write the handshake atomically.
pub fn write(path: &Path, version: &str, operation: Option<&str>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| UpdateError::io(parent, error))?;
    }
    let document = Handshake::new(version, operation.map(str::to_string));
    let bytes = serde_json::to_vec_pretty(&document).expect("handshake serialization");
    let temporary = path.with_extension("handshake.tmp");
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| UpdateError::io(path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_requires_the_same_operation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("handshake.ok");
        write(&path, "1.2.3", Some("op-1")).unwrap();
        assert!(confirms(&path, "op-1"));
        assert!(!confirms(&path, "op-2"));
        // The operation id, not the build version, is the confirmation key.
        write(&path, "9.9.9", Some("op-1")).unwrap();
        assert!(confirms(&path, "op-1"));

        write(&path, "1.2.3", None).unwrap();
        assert!(!confirms(&path, "op-1"));
    }

    #[test]
    fn malformed_and_missing_files_do_not_confirm() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("handshake.ok");
        assert!(!confirms(&path, "op-1"));
        std::fs::write(&path, b"not json").unwrap();
        assert!(!confirms(&path, "op-1"));
    }
}
