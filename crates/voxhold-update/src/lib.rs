//! Self-contained update layer for Voxhold.
//!
//! The crate deliberately has no UI dependency. It covers:
//!
//! * `manifest` — strict schema-1 parsing and version/OS/channel checks;
//! * `check` — background fetch of `latest.json` + signature over HTTPS;
//! * `verify` — Ed25519 verification before JSON parsing;
//! * `package` — deterministic portable ZIPs and installed-package repacking;
//! * `delta` — content deltas with an optional block patch for large files;
//! * `download` — HTTPS-only background download with `Range` resume;
//! * `stage` — verified staging under `%LOCALAPPDATA%\VoxHold\updates`;
//! * `apply` — backup, atomic swap, handshake, rollback, helper relocation
//!   and backup cleanup.
//!
//! Logs never contain query strings or tokens; use [`error::redact_url`] when
//! a URL must appear in diagnostics.

pub mod apply;
pub mod check;
pub mod delta;
pub mod download;
pub mod error;
pub mod manifest;
pub mod package;
pub mod patch;
pub mod stage;
pub mod timefmt;
pub mod urls;
pub mod verify;

pub use error::{redact_url, Result, UpdateError};
pub use manifest::{
    DeltaEntry, FullPackage, Manifest, Offer, SetupPackage, VerifiedManifest, WindowsVersion,
    BETA_CHANNEL, MAX_PACKAGE_BYTES, SCHEMA_VERSION,
};
pub use package::{PackageFile, PackageIndex, PackedPackage};
pub use verify::UpdatePublicKey;

/// User agent used by release tooling and diagnostics.
pub const USER_AGENT: &str = "VoxHold-Updater/1";
