//! Signed update manifest (`latest.json`, schema 1).

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::{Result, UpdateError};
use crate::timefmt::parse_rfc3339_utc;
use crate::urls::validate_update_url;

/// Manifest schema implemented by this crate.
pub const SCHEMA_VERSION: u32 = 1;
/// Channel used by the public beta release.
pub const BETA_CHANNEL: &str = "beta";
/// Hard cap for a downloaded package or delta.
pub const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Cap for release notes embedded in the manifest.
pub const MAX_NOTES_BYTES: usize = 64 * 1024;
/// Cap for the number of deltas advertised by one manifest.
pub const MAX_DELTAS: usize = 32;

/// Full portable package (`full.zip`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FullPackage {
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub package_sha256: String,
}

/// Signed installer (`Setup.exe`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupPackage {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// Delta package entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaEntry {
    pub from_version: String,
    pub from_package_sha256: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// Parsed and validated update manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub channel: String,
    pub version: String,
    pub min_windows: String,
    pub published_at: String,
    pub notes: String,
    pub authenticode: bool,
    pub full: FullPackage,
    pub setup: SetupPackage,
    pub deltas: Vec<DeltaEntry>,
}

/// Result of comparing a manifest version with the installed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// The manifest advertises a newer version.
    Update(Version),
    /// The installed version is already current.
    UpToDate,
}

/// Windows version triple (`major.minor.build`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WindowsVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

impl WindowsVersion {
    pub const fn new(major: u32, minor: u32, build: u32) -> Self {
        Self {
            major,
            minor,
            build,
        }
    }

    /// Read the running Windows build.
    ///
    /// Uses the registry because `GetVersionExW` is shimmed for applications
    /// without an explicit compatibility manifest. Returns `None` on
    /// non-Windows hosts.
    pub fn current() -> Option<Self> {
        current_windows_version()
    }
}

impl FromStr for WindowsVersion {
    type Err = UpdateError;

    fn from_str(text: &str) -> Result<Self> {
        let mut parts = text.split('.');
        let major = parse_component(parts.next())?;
        let minor = parse_component(parts.next())?;
        let build = parse_component(parts.next())?;
        if parts.next().is_some() {
            return Err(UpdateError::InvalidManifest(format!(
                "windows version {text} has too many components"
            )));
        }
        Ok(Self::new(major, minor, build))
    }
}

impl fmt::Display for WindowsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.build)
    }
}

fn parse_component(part: Option<&str>) -> Result<u32> {
    let text = part.ok_or_else(|| {
        UpdateError::InvalidManifest("windows version needs major.minor.build".into())
    })?;
    text.parse::<u32>().map_err(|_| {
        UpdateError::InvalidManifest(format!("windows version component {text} is not numeric"))
    })
}

impl Manifest {
    /// Parse a manifest and validate every field without trusting the JSON.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| UpdateError::InvalidManifest(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Serialize to canonical compact JSON.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("manifest serialization cannot fail")
    }

    /// Serialize to indented JSON with a trailing newline.
    pub fn to_pretty_json(&self) -> Vec<u8> {
        let mut bytes =
            serde_json::to_vec_pretty(self).expect("manifest serialization cannot fail");
        bytes.push(b'\n');
        bytes
    }

    /// Strict field validation.
    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            return Err(UpdateError::UnsupportedSchema(self.schema));
        }
        if self.channel.trim().is_empty() || self.channel.len() > 32 {
            return Err(UpdateError::InvalidManifest("channel is empty".into()));
        }
        Version::parse(&self.version)
            .map_err(|_| UpdateError::InvalidVersion(self.version.clone()))?;
        WindowsVersion::from_str(&self.min_windows)?;
        if parse_rfc3339_utc(&self.published_at).is_none() {
            return Err(UpdateError::InvalidManifest(
                "published_at is not an RFC 3339 UTC timestamp".into(),
            ));
        }
        if self.notes.len() > MAX_NOTES_BYTES {
            return Err(UpdateError::InvalidManifest("notes are too large".into()));
        }
        validate_url(&self.full.url)?;
        validate_size("full", self.full.size)?;
        validate_sha256("full.sha256", &self.full.sha256)?;
        validate_sha256("full.package_sha256", &self.full.package_sha256)?;
        validate_url(&self.setup.url)?;
        validate_size("setup", self.setup.size)?;
        validate_sha256("setup.sha256", &self.setup.sha256)?;
        if self.deltas.len() > MAX_DELTAS {
            return Err(UpdateError::InvalidManifest("too many deltas".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for delta in &self.deltas {
            Version::parse(&delta.from_version)
                .map_err(|_| UpdateError::InvalidVersion(delta.from_version.clone()))?;
            validate_sha256("delta.from_package_sha256", &delta.from_package_sha256)?;
            validate_sha256("delta.sha256", &delta.sha256)?;
            validate_url(&delta.url)?;
            validate_size("delta", delta.size)?;
            if !seen.insert((
                delta.from_version.clone(),
                delta.from_package_sha256.clone(),
            )) {
                return Err(UpdateError::InvalidManifest(
                    "duplicate delta base entry".into(),
                ));
            }
        }
        Ok(())
    }

    /// Reject manifests from a different channel.
    pub fn check_channel(&self, expected: &str) -> Result<()> {
        if self.channel != expected {
            return Err(UpdateError::ChannelMismatch {
                expected: expected.to_string(),
                actual: self.channel.clone(),
            });
        }
        Ok(())
    }

    /// Compare the advertised version with the installed version.
    ///
    /// A lower advertised version is an error, not a silent no-op, so a
    /// compromised or misconfigured channel cannot move clients backwards.
    pub fn offer(&self, current: &Version) -> Result<Offer> {
        let candidate = Version::parse(&self.version)
            .map_err(|_| UpdateError::InvalidVersion(self.version.clone()))?;
        match candidate.cmp(current) {
            Ordering::Greater => Ok(Offer::Update(candidate)),
            Ordering::Equal => Ok(Offer::UpToDate),
            Ordering::Less => Err(UpdateError::Downgrade {
                current: current.to_string(),
                candidate: candidate.to_string(),
            }),
        }
    }

    /// Like [`Manifest::offer`], but a version the user dismissed manually is
    /// not offered again. A newer version is still offered.
    pub fn offer_with_dismissal(
        &self,
        current: &Version,
        dismissed: Option<&Version>,
    ) -> Result<Offer> {
        let offer = self.offer(current)?;
        if let (Offer::Update(candidate), Some(dismissed)) = (&offer, dismissed) {
            if candidate == dismissed {
                return Ok(Offer::UpToDate);
            }
        }
        Ok(offer)
    }

    /// Check the minimum Windows build.
    pub fn check_windows(&self, current: &WindowsVersion) -> Result<()> {
        let minimum = WindowsVersion::from_str(&self.min_windows)?;
        if current < &minimum {
            return Err(UpdateError::UnsupportedWindows {
                minimum: minimum.to_string(),
                current: current.to_string(),
            });
        }
        Ok(())
    }

    /// Pick a delta that applies to the verified installed package.
    ///
    /// A delta that is not smaller than the full package is ignored: the
    /// caller downloads the full package in that case.
    pub fn select_delta(
        &self,
        from_version: &Version,
        from_package_sha256: &str,
    ) -> Option<&DeltaEntry> {
        self.deltas.iter().find(|delta| {
            delta.from_version == from_version.to_string()
                && delta.from_package_sha256 == from_package_sha256
                && delta.size < self.full.size
        })
    }
}

fn validate_url(raw: &str) -> Result<()> {
    validate_update_url(raw).map(|_| ())
}

fn validate_size(label: &str, size: u64) -> Result<()> {
    if size == 0 {
        return Err(UpdateError::InvalidManifest(format!(
            "{label} size is zero"
        )));
    }
    if size > MAX_PACKAGE_BYTES {
        return Err(UpdateError::InvalidManifest(format!(
            "{label} size {size} exceeds {MAX_PACKAGE_BYTES}"
        )));
    }
    Ok(())
}

/// Validate a lowercase hex SHA-256 digest.
pub fn validate_sha256(label: &str, value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(UpdateError::InvalidManifest(format!(
            "{label} is not a SHA-256 hex digest"
        )));
    }
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(UpdateError::InvalidManifest(format!(
            "{label} must be lowercase hex"
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn current_windows_version() -> Option<WindowsVersion> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };

    const SUBKEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";

    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn dword(subkey: &[u16], name: &[u16]) -> Option<u32> {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut size,
            )
        };
        (status == 0).then_some(value)
    }

    fn string(subkey: &[u16], name: &[u16]) -> Option<String> {
        let mut buffer = [0u16; 64];
        let mut size = (buffer.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        let len = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    }

    let subkey = wide(SUBKEY);
    let major = dword(&subkey, &wide("CurrentMajorVersionNumber")).unwrap_or(10);
    let minor = dword(&subkey, &wide("CurrentMinorVersionNumber")).unwrap_or(0);
    let build = string(&subkey, &wide("CurrentBuildNumber"))?
        .trim()
        .parse::<u32>()
        .ok()?;
    Some(WindowsVersion::new(major, minor, build))
}

#[cfg(not(windows))]
fn current_windows_version() -> Option<WindowsVersion> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            schema: 1,
            channel: BETA_CHANNEL.into(),
            version: "0.2.0".into(),
            min_windows: "10.0.19041".into(),
            published_at: "2026-11-19T12:00:00Z".into(),
            notes: "Notes".into(),
            authenticode: true,
            full: FullPackage {
                url: "https://example.invalid/full.zip".into(),
                size: 1024,
                sha256: "a".repeat(64),
                package_sha256: "a".repeat(64),
            },
            setup: SetupPackage {
                url: "https://example.invalid/Setup.exe".into(),
                size: 512,
                sha256: "b".repeat(64),
            },
            deltas: vec![DeltaEntry {
                from_version: "0.1.0".into(),
                from_package_sha256: "c".repeat(64),
                url: "https://example.invalid/delta.zip".into(),
                size: 128,
                sha256: "d".repeat(64),
            }],
        }
    }

    #[test]
    fn round_trips_and_validates() {
        let manifest = sample();
        let bytes = manifest.to_json();
        assert_eq!(Manifest::parse(&bytes).unwrap(), manifest);
        assert!(String::from_utf8(bytes)
            .unwrap()
            .starts_with("{\"schema\":1,"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut value: serde_json::Value = serde_json::from_slice(&sample().to_json()).unwrap();
        value["extra"] = serde_json::json!(true);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(Manifest::parse(&bytes).is_err());
    }

    #[test]
    fn compares_versions_without_downgrade() {
        let manifest = sample();
        assert_eq!(
            manifest.offer(&Version::new(0, 1, 0)).unwrap(),
            Offer::Update(Version::new(0, 2, 0))
        );
        assert_eq!(
            manifest.offer(&Version::new(0, 2, 0)).unwrap(),
            Offer::UpToDate
        );
        assert!(matches!(
            manifest.offer(&Version::new(0, 3, 0)),
            Err(UpdateError::Downgrade { .. })
        ));
    }

    #[test]
    fn dismissed_version_is_not_offered_again() {
        let manifest = sample();
        let dismissed = Version::new(0, 2, 0);
        assert_eq!(
            manifest
                .offer_with_dismissal(&Version::new(0, 1, 0), Some(&dismissed))
                .unwrap(),
            Offer::UpToDate
        );
        assert_eq!(
            manifest
                .offer_with_dismissal(&Version::new(0, 1, 0), Some(&Version::new(0, 1, 9)))
                .unwrap(),
            Offer::Update(Version::new(0, 2, 0))
        );
    }

    #[test]
    fn rejects_foreign_channel() {
        let manifest = sample();
        assert!(manifest.check_channel(BETA_CHANNEL).is_ok());
        assert!(matches!(
            manifest.check_channel("stable"),
            Err(UpdateError::ChannelMismatch { .. })
        ));
    }

    #[test]
    fn checks_windows_minimum() {
        let manifest = sample();
        assert!(manifest
            .check_windows(&WindowsVersion::new(10, 0, 19041))
            .is_ok());
        assert!(manifest
            .check_windows(&WindowsVersion::new(10, 0, 19045))
            .is_ok());
        assert!(matches!(
            manifest.check_windows(&WindowsVersion::new(10, 0, 18363)),
            Err(UpdateError::UnsupportedWindows { .. })
        ));
    }

    #[test]
    fn rejects_insecure_urls_and_bad_hashes() {
        let mut manifest = sample();
        manifest.full.url = "http://example.invalid/full.zip".into();
        assert!(manifest.validate().is_err());

        let mut manifest = sample();
        manifest.full.url = "http://127.0.0.1:8080/full.zip".into();
        assert!(manifest.validate().is_ok());

        let mut manifest = sample();
        manifest.full.sha256 = "A".repeat(64);
        assert!(manifest.validate().is_err());

        let mut manifest = sample();
        manifest.setup.size = MAX_PACKAGE_BYTES + 1;
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn selects_only_matching_smaller_delta() {
        let manifest = sample();
        let base = Version::new(0, 1, 0);
        assert!(manifest.select_delta(&base, &"c".repeat(64)).is_some());
        assert!(manifest.select_delta(&base, &"e".repeat(64)).is_none());
        assert!(manifest
            .select_delta(&Version::new(0, 0, 9), &"c".repeat(64))
            .is_none());

        let mut large = sample();
        large.deltas[0].size = large.full.size;
        assert!(large.select_delta(&base, &"c".repeat(64)).is_none());
    }
}
