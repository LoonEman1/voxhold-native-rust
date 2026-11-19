//! Delta packages between two portable packages.
//!
//! `delta.json` describes the change; payload entries live under `files/`
//! (full replacement) and `patches/` (block patch for large binaries). A
//! delta is only produced when its ZIP is smaller than the target package,
//! and applying it must reproduce the target package byte for byte.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Result, UpdateError};
use crate::manifest::{validate_sha256, MAX_PACKAGE_BYTES};
use crate::package::{
    pack_from_map, read_zip_entries, sha256_hex, validate_relative_path, PackageFile, PackageIndex,
    PACKAGE_JSON,
};
use crate::patch::{apply_patch, create_patch};

/// Delta schema implemented by this crate.
pub const DELTA_SCHEMA: u32 = 1;
/// File name of the delta description inside the delta ZIP.
pub const DELTA_JSON: &str = "delta.json";

/// How a file is represented in the delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeltaMode {
    /// Take the file from the base package unchanged.
    Copy,
    /// Store the complete target file.
    Replace,
    /// Apply a block patch to the base file.
    Patch,
}

/// Metadata for a block patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchMeta {
    pub base_size: u64,
    pub base_sha256: String,
}

/// One changed or copied file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaFile {
    pub path: String,
    pub mode: DeltaMode,
    pub size: u64,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub patch: Option<PatchMeta>,
}

/// Parsed `delta.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaManifest {
    pub schema: u32,
    pub from_version: String,
    pub to_version: String,
    pub base_package_sha256: String,
    pub target_package_sha256: String,
    pub deletes: Vec<String>,
    pub files: Vec<DeltaFile>,
}

impl DeltaManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let delta: Self = serde_json::from_slice(bytes)
            .map_err(|error| UpdateError::InvalidDelta(error.to_string()))?;
        delta.validate()?;
        Ok(delta)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("delta serialization cannot fail")
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != DELTA_SCHEMA {
            return Err(UpdateError::InvalidDelta(format!(
                "unsupported schema {}",
                self.schema
            )));
        }
        semver::Version::parse(&self.from_version)
            .map_err(|_| UpdateError::InvalidVersion(self.from_version.clone()))?;
        semver::Version::parse(&self.to_version)
            .map_err(|_| UpdateError::InvalidVersion(self.to_version.clone()))?;
        validate_sha256("delta.base_package_sha256", &self.base_package_sha256)?;
        validate_sha256("delta.target_package_sha256", &self.target_package_sha256)?;
        let mut deletes = BTreeSet::new();
        for path in &self.deletes {
            validate_relative_path(path)?;
            if !deletes.insert(path.clone()) {
                return Err(UpdateError::InvalidDelta(format!(
                    "duplicate delete {path}"
                )));
            }
        }
        let mut files = BTreeSet::new();
        for file in &self.files {
            validate_relative_path(&file.path)?;
            validate_sha256("delta file sha256", &file.sha256)?;
            if file.size > MAX_PACKAGE_BYTES {
                return Err(UpdateError::InvalidDelta(format!(
                    "{} is too large",
                    file.path
                )));
            }
            if !files.insert(file.path.clone()) {
                return Err(UpdateError::InvalidDelta(format!(
                    "duplicate file {}",
                    file.path
                )));
            }
            if deletes.contains(&file.path) {
                return Err(UpdateError::InvalidDelta(format!(
                    "{} is both deleted and written",
                    file.path
                )));
            }
            match file.mode {
                DeltaMode::Patch => {
                    let meta = file.patch.as_ref().ok_or_else(|| {
                        UpdateError::InvalidDelta(format!(
                            "{} uses patch mode without metadata",
                            file.path
                        ))
                    })?;
                    validate_sha256("delta patch base sha256", &meta.base_sha256)?;
                    if meta.base_size > MAX_PACKAGE_BYTES {
                        return Err(UpdateError::InvalidDelta(format!(
                            "{} patch base is too large",
                            file.path
                        )));
                    }
                }
                DeltaMode::Copy | DeltaMode::Replace => {
                    if file.patch.is_some() {
                        return Err(UpdateError::InvalidDelta(format!(
                            "{} has patch metadata without patch mode",
                            file.path
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Payload entry name for a file.
    pub fn payload_name(file: &DeltaFile) -> String {
        match file.mode {
            DeltaMode::Patch => format!("patches/{}", file.path),
            DeltaMode::Copy | DeltaMode::Replace => format!("files/{}", file.path),
        }
    }
}

/// Create a delta ZIP from a base package to a target package.
pub fn create(base_zip: &[u8], target_zip: &[u8]) -> Result<Vec<u8>> {
    let base_entries = read_zip_entries(base_zip)?;
    let target_entries = read_zip_entries(target_zip)?;
    let base_index = package_index(&base_entries, base_zip)?;
    let target_index = package_index(&target_entries, target_zip)?;

    let base_files = crate::package::file_map(&base_index);
    let target_files = crate::package::file_map(&target_index);

    let mut deletes = Vec::new();
    for path in base_files.keys() {
        if !target_files.contains_key(path) {
            deletes.push(path.clone());
        }
    }

    let mut files = Vec::new();
    let mut payload: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (path, target_file) in &target_files {
        let base_file = base_files.get(path);
        if let Some(base_file) = base_file {
            if base_file.size == target_file.size && base_file.sha256 == target_file.sha256 {
                files.push(DeltaFile {
                    path: path.clone(),
                    mode: DeltaMode::Copy,
                    size: target_file.size,
                    sha256: target_file.sha256.clone(),
                    patch: None,
                });
                continue;
            }
        }
        let target_bytes = target_entries.get(path).ok_or_else(|| {
            UpdateError::InvalidPackage(format!("target package is missing {path}"))
        })?;
        let base_bytes = base_entries.get(path);
        let patch = base_bytes
            .filter(|base| base.len() as u64 <= MAX_PACKAGE_BYTES)
            .and_then(|base| create_patch(base, target_bytes));
        if let (Some(patch_bytes), Some(base_bytes)) = (patch, base_bytes) {
            let entry = DeltaFile {
                path: path.clone(),
                mode: DeltaMode::Patch,
                size: target_file.size,
                sha256: target_file.sha256.clone(),
                patch: Some(PatchMeta {
                    base_size: base_bytes.len() as u64,
                    base_sha256: sha256_hex(base_bytes),
                }),
            };
            payload.insert(DeltaManifest::payload_name(&entry), patch_bytes);
            files.push(entry);
        } else {
            let entry = DeltaFile {
                path: path.clone(),
                mode: DeltaMode::Replace,
                size: target_file.size,
                sha256: target_file.sha256.clone(),
                patch: None,
            };
            payload.insert(DeltaManifest::payload_name(&entry), target_bytes.clone());
            files.push(entry);
        }
    }

    let delta = DeltaManifest {
        schema: DELTA_SCHEMA,
        from_version: base_index.version.clone(),
        to_version: target_index.version.clone(),
        base_package_sha256: sha256_hex(base_zip),
        target_package_sha256: sha256_hex(target_zip),
        deletes,
        files,
    };
    delta.validate()?;

    let mut entries = payload;
    entries.insert(DELTA_JSON.to_string(), delta.to_json());
    let bytes = crate::package::write_zip_entries(&entries)?;
    if bytes.len() >= target_zip.len() {
        return Err(UpdateError::InvalidDelta(
            "delta is not smaller than the target package".into(),
        ));
    }
    Ok(bytes)
}

/// Read `delta.json` from a delta ZIP.
pub fn read_manifest(delta_zip: &[u8]) -> Result<DeltaManifest> {
    let entries = read_zip_entries(delta_zip)?;
    let raw = entries
        .get(DELTA_JSON)
        .ok_or_else(|| UpdateError::InvalidDelta("delta.json is missing".into()))?;
    DeltaManifest::parse(raw)
}

/// Apply a delta ZIP to a base package, producing the target package.
pub fn apply(base_zip: &[u8], delta_zip: &[u8]) -> Result<Vec<u8>> {
    let delta_entries = read_zip_entries(delta_zip)?;
    let delta_raw = delta_entries
        .get(DELTA_JSON)
        .ok_or_else(|| UpdateError::InvalidDelta("delta.json is missing".into()))?;
    let delta = DeltaManifest::parse(delta_raw)?;

    let base_entries = read_zip_entries(base_zip)?;
    let base_index = package_index(&base_entries, base_zip)?;
    if base_index.version != delta.from_version {
        return Err(UpdateError::InvalidDelta(format!(
            "delta expects base version {}, got {}",
            delta.from_version, base_index.version
        )));
    }
    if sha256_hex(base_zip) != delta.base_package_sha256 {
        return Err(UpdateError::DeltaBaseMismatch);
    }

    let mut contents: BTreeMap<String, Vec<u8>> = base_entries
        .into_iter()
        .filter(|(path, _)| path != PACKAGE_JSON)
        .collect();
    let mut files: BTreeMap<String, PackageFile> = crate::package::file_map(&base_index);

    for path in &delta.deletes {
        if contents.remove(path).is_none() {
            return Err(UpdateError::InvalidDelta(format!(
                "delete of unknown path {path}"
            )));
        }
        files.remove(path);
    }

    let expected_payload: BTreeSet<String> = delta
        .files
        .iter()
        .filter(|file| file.mode != DeltaMode::Copy)
        .map(DeltaManifest::payload_name)
        .collect();
    let actual_payload: BTreeSet<String> = delta_entries
        .keys()
        .filter(|name| name.as_str() != DELTA_JSON)
        .cloned()
        .collect();
    if expected_payload != actual_payload {
        return Err(UpdateError::InvalidDelta(
            "delta payload does not match its file table".into(),
        ));
    }

    for file in &delta.files {
        validate_relative_path(&file.path)?;
        let bytes = match file.mode {
            DeltaMode::Copy => {
                let base = contents.get(&file.path).ok_or_else(|| {
                    UpdateError::InvalidDelta(format!("copy of unknown path {}", file.path))
                })?;
                if base.len() as u64 != file.size || sha256_hex(base) != file.sha256 {
                    return Err(UpdateError::InvalidDelta(format!(
                        "copy of {} does not match the base",
                        file.path
                    )));
                }
                base.clone()
            }
            DeltaMode::Replace => {
                let payload = delta_entries
                    .get(&DeltaManifest::payload_name(file))
                    .ok_or_else(|| {
                        UpdateError::InvalidDelta(format!("missing payload for {}", file.path))
                    })?;
                if payload.len() as u64 != file.size || sha256_hex(payload) != file.sha256 {
                    return Err(UpdateError::HashMismatch {
                        path: PathBuf::from(&file.path),
                    });
                }
                payload.clone()
            }
            DeltaMode::Patch => {
                let base = contents.get(&file.path).ok_or_else(|| {
                    UpdateError::InvalidDelta(format!("patch of unknown path {}", file.path))
                })?;
                let meta = file.patch.as_ref().ok_or_else(|| {
                    UpdateError::InvalidDelta(format!("missing patch metadata for {}", file.path))
                })?;
                if base.len() as u64 != meta.base_size || sha256_hex(base) != meta.base_sha256 {
                    return Err(UpdateError::DeltaBaseMismatch);
                }
                let payload = delta_entries
                    .get(&DeltaManifest::payload_name(file))
                    .ok_or_else(|| {
                        UpdateError::InvalidDelta(format!("missing patch for {}", file.path))
                    })?;
                let patched = apply_patch(base, payload, file.size as usize)?;
                if sha256_hex(&patched) != file.sha256 {
                    return Err(UpdateError::HashMismatch {
                        path: PathBuf::from(&file.path),
                    });
                }
                patched
            }
        };
        contents.insert(file.path.clone(), bytes);
        files.insert(
            file.path.clone(),
            PackageFile {
                path: file.path.clone(),
                size: file.size,
                sha256: file.sha256.clone(),
            },
        );
    }

    let target_index = PackageIndex::new(delta.to_version.clone(), files.into_values().collect());
    let packed = pack_from_map(&target_index, &contents)?;
    if packed.sha256 != delta.target_package_sha256 {
        return Err(UpdateError::DeltaTargetMismatch);
    }
    Ok(packed.bytes)
}

fn package_index(entries: &BTreeMap<String, Vec<u8>>, _zip: &[u8]) -> Result<PackageIndex> {
    let raw = entries
        .get(PACKAGE_JSON)
        .ok_or_else(|| UpdateError::InvalidPackage("package.json is missing".into()))?;
    PackageIndex::parse(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::{pack_with_index, unpack};
    use std::fs;

    fn write_package(root: &std::path::Path, version: &str, exe: &[u8], note: &str) -> Vec<u8> {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("Voxhold.exe"), exe).unwrap();
        fs::write(root.join("README.md"), note).unwrap();
        pack_with_index(root, version).unwrap().bytes
    }

    fn binary(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.extend_from_slice(&state.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    #[test]
    fn round_trip_reproduces_target_hash() {
        let temp = tempfile::tempdir().unwrap();
        let base_dir = temp.path().join("base");
        let target_dir = temp.path().join("target");
        let exe = binary(512 * 1024, 0x1234_5678);
        let base = write_package(&base_dir, "1.0.0", &exe, "one");
        let mut updated = exe.clone();
        updated[200 * 1024..200 * 1024 + 64].fill(0xEE);
        let target = write_package(&target_dir, "1.1.0", &updated, "two");

        let delta = create(&base, &target).unwrap();
        assert!(delta.len() < target.len() / 10, "delta {}", delta.len());
        let restored = apply(&base, &delta).unwrap();
        assert_eq!(sha256_hex(&restored), sha256_hex(&target));

        let out = temp.path().join("out");
        unpack(&restored, &out).unwrap();
        assert_eq!(fs::read(out.join("README.md")).unwrap(), b"two");
    }

    #[test]
    fn rejects_wrong_base_and_tampered_payload() {
        let temp = tempfile::tempdir().unwrap();
        let exe = binary(128 * 1024, 0xABCD_EF01);
        let base = write_package(&temp.path().join("base"), "1.0.0", &exe, "one");
        let mut updated = exe.clone();
        updated[1000..1064].fill(0x11);
        let target = write_package(&temp.path().join("target"), "1.1.0", &updated, "two");
        let delta = create(&base, &target).unwrap();

        let other = write_package(
            &temp.path().join("other"),
            "0.9.0",
            &binary(128 * 1024, 0x5555_5555),
            "one",
        );
        assert!(matches!(
            apply(&other, &delta),
            Err(UpdateError::DeltaBaseMismatch) | Err(UpdateError::InvalidDelta(_))
        ));

        let mut tampered = delta.clone();
        let position = tampered.len() / 2;
        tampered[position] ^= 0x01;
        assert!(apply(&base, &tampered).is_err());
    }

    #[test]
    fn copy_mode_reuses_base_files() {
        let temp = tempfile::tempdir().unwrap();
        let exe = binary(256 * 1024, 0x0BAD_F00D);
        let base = write_package(&temp.path().join("base"), "1.0.0", &exe, "same");
        let target = write_package(&temp.path().join("target"), "1.1.0", &exe, "changed");
        let delta = create(&base, &target).unwrap();
        let manifest = read_manifest(&delta).unwrap();
        let readme = manifest
            .files
            .iter()
            .find(|file| file.path == "README.md")
            .unwrap();
        assert_eq!(readme.mode, DeltaMode::Replace);
        let executable = manifest
            .files
            .iter()
            .find(|file| file.path == "Voxhold.exe")
            .unwrap();
        assert_eq!(executable.mode, DeltaMode::Copy);
    }
}
