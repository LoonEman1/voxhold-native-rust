//! Deterministic portable packages.
//!
//! A package is a ZIP archive with sorted entries, a fixed timestamp
//! (1980-01-01), a fixed compression level, no directory entries, no extra
//! fields and unix mode 0644. Those choices make `sha256(zip)` stable across
//! runs and machines, which is what deltas and manifest checks rely on.
//!
//! `package.json` inside the archive lists every payload file with its size
//! and SHA-256. It is not listed in its own file table.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipArchive, ZipWriter};

use crate::error::{Result, UpdateError};
use crate::manifest::MAX_PACKAGE_BYTES;

/// Name of the package index inside the archive.
pub const PACKAGE_JSON: &str = "package.json";
const FIXED_TIMESTAMP: DateTime = DateTime::DEFAULT;

/// One payload file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Parsed `package.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIndex {
    pub version: String,
    pub files: Vec<PackageFile>,
}

impl PackageIndex {
    /// Build an index and sort its files by path.
    pub fn new(version: impl Into<String>, mut files: Vec<PackageFile>) -> Self {
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Self {
            version: version.into(),
            files,
        }
    }

    /// Parse and validate a `package.json` document.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let index: Self = serde_json::from_slice(bytes)
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
        index.validate()?;
        Ok(index)
    }

    /// Canonical serialization used inside the archive.
    pub fn to_json(&self) -> Vec<u8> {
        let sorted = Self::new(self.version.clone(), self.files.clone());
        serde_json::to_vec(&sorted).expect("package index serialization cannot fail")
    }

    /// Validate paths, sizes and digests.
    pub fn validate(&self) -> Result<()> {
        semver::Version::parse(&self.version)
            .map_err(|_| UpdateError::InvalidVersion(self.version.clone()))?;
        let mut seen = BTreeSet::new();
        for file in &self.files {
            validate_relative_path(&file.path)?;
            if file.path == PACKAGE_JSON {
                return Err(UpdateError::InvalidPackage(
                    "package.json must not list itself".into(),
                ));
            }
            crate::manifest::validate_sha256("package file sha256", &file.sha256)?;
            if file.size > MAX_PACKAGE_BYTES {
                return Err(UpdateError::InvalidPackage(format!(
                    "{} is too large",
                    file.path
                )));
            }
            if !seen.insert(file.path.clone()) {
                return Err(UpdateError::InvalidPackage(format!(
                    "duplicate path {}",
                    file.path
                )));
            }
        }
        Ok(())
    }

    /// Look up a file entry.
    pub fn get(&self, path: &str) -> Option<&PackageFile> {
        self.files.iter().find(|file| file.path == path)
    }
}

/// A package archive plus its parsed index.
#[derive(Debug, Clone)]
pub struct PackedPackage {
    pub bytes: Vec<u8>,
    pub index: PackageIndex,
    pub sha256: String,
}

impl PackedPackage {
    pub fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
}

/// Pack a directory into a deterministic ZIP.
pub fn pack(dir: &Path, version: &str) -> Result<Vec<u8>> {
    Ok(pack_with_index(dir, version)?.bytes)
}

/// Pack a directory and return the index alongside the archive bytes.
pub fn pack_with_index(dir: &Path, version: &str) -> Result<PackedPackage> {
    semver::Version::parse(version).map_err(|_| UpdateError::InvalidVersion(version.into()))?;
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(current) = stack.pop() {
        let mut entries = std::fs::read_dir(&current)
            .map_err(|error| UpdateError::io(&current, error))?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|error| UpdateError::io(&current, error))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let file_type = entry
                .file_type()
                .map_err(|error| UpdateError::io(entry.path(), error))?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                paths.push(entry.path());
            } else {
                return Err(UpdateError::InvalidPackage(format!(
                    "{} is not a regular file",
                    entry.path().display()
                )));
            }
        }
    }
    paths.sort_by(|left, right| relative_name(dir, left).cmp(&relative_name(dir, right)));
    for path in paths {
        let relative = relative_name(dir, &path);
        validate_relative_path(&relative)?;
        if relative == PACKAGE_JSON {
            return Err(UpdateError::InvalidPackage(
                "the payload must not contain package.json".into(),
            ));
        }
        let bytes = std::fs::read(&path).map_err(|error| UpdateError::io(&path, error))?;
        if bytes.len() as u64 > MAX_PACKAGE_BYTES {
            return Err(UpdateError::TooLarge {
                path,
                limit: MAX_PACKAGE_BYTES,
                actual: bytes.len() as u64,
            });
        }
        files.push(PackageFile {
            path: relative,
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        });
    }
    let index = PackageIndex::new(version, files);
    pack_index(dir, &index)
}

/// Rebuild the package archive for an installed directory.
///
/// Only paths listed in `index` are read; unrelated user files are ignored.
/// The result is byte-identical to the archive produced by [`pack`] for the
/// same version and files.
pub fn repack_installed(dir: &Path, index: &PackageIndex) -> Result<PackedPackage> {
    index.validate()?;
    pack_index(dir, index)
}

fn pack_index(dir: &Path, index: &PackageIndex) -> Result<PackedPackage> {
    let mut contents = BTreeMap::new();
    for file in &index.files {
        let path = dir.join(Path::new(&file.path));
        let bytes = std::fs::read(&path).map_err(|error| UpdateError::io(&path, error))?;
        contents.insert(file.path.clone(), bytes);
    }
    pack_from_map(index, &contents)
}

/// Build a package archive from in-memory file contents.
///
/// Every file in `index` must be present in `contents` with the expected size
/// and digest. The archive layout matches [`pack`] byte for byte.
pub fn pack_from_map(
    index: &PackageIndex,
    contents: &BTreeMap<String, Vec<u8>>,
) -> Result<PackedPackage> {
    index.validate()?;
    let sorted = PackageIndex::new(index.version.clone(), index.files.clone());
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for file in &sorted.files {
        let bytes = contents.get(&file.path).ok_or_else(|| {
            UpdateError::InvalidPackage(format!("missing content for {}", file.path))
        })?;
        if bytes.len() as u64 != file.size || sha256_hex(bytes) != file.sha256 {
            return Err(UpdateError::HashMismatch {
                path: PathBuf::from(&file.path),
            });
        }
        writer
            .start_file(&file.path, file_options())
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
        writer
            .write_all(bytes)
            .map_err(|error| UpdateError::io(Path::new(&file.path), error))?;
    }
    let package_json = sorted.to_json();
    writer
        .start_file(PACKAGE_JSON, file_options())
        .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
    writer
        .write_all(&package_json)
        .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
    let bytes = writer
        .finish()
        .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?
        .into_inner();
    let sha256 = sha256_hex(&bytes);
    Ok(PackedPackage {
        bytes,
        index: sorted,
        sha256,
    })
}

fn file_options() -> SimpleFileOptions {
    SimpleFileOptions::DEFAULT
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(9))
        .unix_permissions(0o644)
        .system(System::Unix)
        .last_modified_time(FIXED_TIMESTAMP)
}

/// Unpack a package into `dest`, verifying every entry against `package.json`.
pub fn unpack(bytes: &[u8], dest: &Path) -> Result<PackageIndex> {
    let index = read_package_json(bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
    let canonical = index.to_json();
    let mut found = BTreeSet::new();
    let mut package_json_seen = false;
    std::fs::create_dir_all(dest).map_err(|error| UpdateError::io(dest, error))?;
    for position in 0..archive.len() {
        let entry = archive
            .by_index(position)
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
        let name = entry
            .name()
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?
            .to_string();
        if entry.is_dir() {
            return Err(UpdateError::InvalidPackage(format!(
                "directory entry {name} is not allowed"
            )));
        }
        if name == PACKAGE_JSON {
            if package_json_seen {
                return Err(UpdateError::InvalidPackage("duplicate package.json".into()));
            }
            package_json_seen = true;
            let mut raw = Vec::new();
            entry
                .take((crate::manifest::MAX_NOTES_BYTES * 16) as u64 + 1)
                .read_to_end(&mut raw)
                .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
            if raw.len() > crate::manifest::MAX_NOTES_BYTES * 16 {
                return Err(UpdateError::InvalidPackage(
                    "package.json is too large".into(),
                ));
            }
            if raw != canonical {
                return Err(UpdateError::InvalidPackage(
                    "package.json is not in canonical form".into(),
                ));
            }
            continue;
        }
        let relative = validate_relative_path(&name)?;
        let expected = index.get(&relative).ok_or_else(|| {
            UpdateError::InvalidPackage(format!("unexpected archive entry {relative}"))
        })?;
        if entry.size() != expected.size {
            return Err(UpdateError::SizeMismatch {
                path: PathBuf::from(&relative),
                expected: expected.size,
                actual: entry.size(),
            });
        }
        let mut data = Vec::new();
        entry
            .take(expected.size.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
        if data.len() as u64 != expected.size || sha256_hex(&data) != expected.sha256 {
            return Err(UpdateError::HashMismatch {
                path: PathBuf::from(&relative),
            });
        }
        let target = dest.join(Path::new(&relative));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| UpdateError::io(parent, error))?;
        }
        std::fs::write(&target, &data).map_err(|error| UpdateError::io(&target, error))?;
        found.insert(relative);
    }
    if !package_json_seen {
        return Err(UpdateError::InvalidPackage(
            "package.json is missing".into(),
        ));
    }
    for file in &index.files {
        if !found.contains(&file.path) {
            return Err(UpdateError::InvalidPackage(format!(
                "archive is missing {}",
                file.path
            )));
        }
    }
    let package_path = dest.join(PACKAGE_JSON);
    std::fs::write(&package_path, canonical)
        .map_err(|error| UpdateError::io(&package_path, error))?;
    Ok(index)
}

/// Read and validate `package.json` from archive bytes.
pub fn read_package_json(bytes: &[u8]) -> Result<PackageIndex> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
    let entry = archive
        .by_name(PACKAGE_JSON)
        .map_err(|_| UpdateError::InvalidPackage("package.json is missing".into()))?;
    let mut raw = Vec::new();
    entry
        .take((crate::manifest::MAX_NOTES_BYTES * 16) as u64)
        .read_to_end(&mut raw)
        .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
    PackageIndex::parse(&raw)
}

/// Read `package.json` from an unpacked directory.
pub fn read_package_json_from_dir(dir: &Path) -> Result<PackageIndex> {
    let path = dir.join(PACKAGE_JSON);
    let bytes = std::fs::read(&path).map_err(|error| UpdateError::io(&path, error))?;
    PackageIndex::parse(&bytes)
}

/// SHA-256 of arbitrary bytes as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// SHA-256 of a file as lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|error| UpdateError::io(path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| UpdateError::io(path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Stable digest over the package file table (sorted by path).
///
/// This is not the ZIP hash; it identifies the payload independently of the
/// archive container.
pub fn canonical_hash(files: &[PackageFile]) -> String {
    let mut sorted: Vec<&PackageFile> = files.iter().collect();
    sorted.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    for file in sorted {
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(file.size.to_string().as_bytes());
        hasher.update([0]);
        hasher.update(file.sha256.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

/// Map an installed file table by path.
pub fn file_map(index: &PackageIndex) -> BTreeMap<String, PackageFile> {
    index
        .files
        .iter()
        .map(|file| (file.path.clone(), file.clone()))
        .collect()
}

/// Read every regular entry of a ZIP into memory.
///
/// Duplicate names, directory entries and unsafe paths are rejected. Total
/// uncompressed size is bounded by the manifest limit.
pub fn read_zip_entries(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
    let mut entries = BTreeMap::new();
    let mut total = 0u64;
    for position in 0..archive.len() {
        let entry = archive
            .by_index(position)
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
        let name = entry
            .name()
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?
            .to_string();
        if entry.is_dir() {
            return Err(UpdateError::InvalidPackage(format!(
                "directory entry {name} is not allowed"
            )));
        }
        let relative = validate_relative_path(&name)?;
        let size = entry.size();
        total = total.saturating_add(size);
        if total > MAX_PACKAGE_BYTES {
            return Err(UpdateError::TooLarge {
                path: PathBuf::from(&relative),
                limit: MAX_PACKAGE_BYTES,
                actual: total,
            });
        }
        let mut data = Vec::new();
        entry
            .take(size.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
        if data.len() as u64 != size {
            return Err(UpdateError::SizeMismatch {
                path: PathBuf::from(&relative),
                expected: size,
                actual: data.len() as u64,
            });
        }
        if entries.insert(relative.clone(), data).is_some() {
            return Err(UpdateError::InvalidPackage(format!(
                "duplicate archive entry {relative}"
            )));
        }
    }
    Ok(entries)
}

/// Write a deterministic ZIP from in-memory entries.
///
/// Entries are written in path order with the same options as [`pack`].
pub fn write_zip_entries(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(name, file_options())
            .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?;
        writer
            .write_all(bytes)
            .map_err(|error| UpdateError::io(Path::new(name), error))?;
    }
    Ok(writer
        .finish()
        .map_err(|error| UpdateError::InvalidPackage(error.to_string()))?
        .into_inner())
}

/// Reject absolute paths, traversal, backslashes and Windows device names.
pub fn validate_relative_path(path: &str) -> Result<String> {
    if path.is_empty() || path.len() > 4096 {
        return Err(UpdateError::InvalidPackage(
            "empty or oversized path".into(),
        ));
    }
    if path.starts_with('/') || path.starts_with('\\') || path.contains('\\') {
        return Err(UpdateError::InvalidPackage(format!(
            "absolute or backslash path {path}"
        )));
    }
    if path.bytes().any(|byte| byte < 0x20) {
        return Err(UpdateError::InvalidPackage(format!(
            "control character in {path}"
        )));
    }
    let mut cleaned = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(UpdateError::InvalidPackage(format!(
                "unsafe path component in {path}"
            )));
        }
        if part.contains(':') {
            return Err(UpdateError::InvalidPackage(format!(
                "drive or stream separator in {path}"
            )));
        }
        cleaned.push(part);
    }
    Ok(cleaned.join("/"))
}

fn relative_name(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sample_dir(root: &Path) {
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("Voxhold.exe"), b"binary payload").unwrap();
        fs::write(root.join("assets").join("icon.svg"), b"<svg/>").unwrap();
        fs::write(root.join("README.md"), b"readme").unwrap();
    }

    #[test]
    fn pack_is_deterministic() {
        let temp = tempfile::tempdir().unwrap();
        sample_dir(temp.path());
        let first = pack_with_index(temp.path(), "1.2.3").unwrap();
        let second = pack_with_index(temp.path(), "1.2.3").unwrap();
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.bytes, second.bytes);
        assert_eq!(first.index.files.len(), 3);
        assert_eq!(first.index.files[0].path, "README.md");
    }

    #[test]
    fn unpack_round_trips_and_repacks() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        sample_dir(&source);
        let packed = pack_with_index(&source, "1.2.3").unwrap();

        let installed = temp.path().join("installed");
        let index = unpack(&packed.bytes, &installed).unwrap();
        assert_eq!(index, packed.index);
        assert_eq!(
            fs::read(installed.join("Voxhold.exe")).unwrap(),
            b"binary payload"
        );
        assert!(installed.join(PACKAGE_JSON).exists());

        let repacked = repack_installed(&installed, &index).unwrap();
        assert_eq!(repacked.sha256, packed.sha256);
    }

    #[test]
    fn rejects_tampered_archive() {
        let temp = tempfile::tempdir().unwrap();
        sample_dir(temp.path());
        let packed = pack_with_index(temp.path(), "1.2.3").unwrap();
        let mut tampered = packed.bytes.clone();
        let position = tampered.len() / 2;
        tampered[position] ^= 0x01;
        assert!(unpack(&tampered, &temp.path().join("out")).is_err());
    }

    #[test]
    fn rejects_unsafe_paths() {
        assert!(validate_relative_path("../evil").is_err());
        assert!(validate_relative_path("/abs").is_err());
        assert!(validate_relative_path("a\\b").is_err());
        assert!(validate_relative_path("C:/x").is_err());
        assert!(validate_relative_path("a//b").is_err());
        assert_eq!(validate_relative_path("a/b.txt").unwrap(), "a/b.txt");
    }

    #[test]
    fn canonical_hash_ignores_order() {
        let a = PackageFile {
            path: "a".into(),
            size: 1,
            sha256: "a".repeat(64),
        };
        let b = PackageFile {
            path: "b".into(),
            size: 2,
            sha256: "b".repeat(64),
        };
        assert_eq!(
            canonical_hash(&[a.clone(), b.clone()]),
            canonical_hash(&[b, a])
        );
    }
}
