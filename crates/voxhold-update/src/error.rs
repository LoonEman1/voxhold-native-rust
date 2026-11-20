//! Typed errors for the update layer.
//!
//! Every message is safe to log: URLs are reduced to scheme/host/path, query
//! strings and user information are never included.

use std::path::PathBuf;

use thiserror::Error;

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, UpdateError>;

/// Errors produced by manifest, package, delta, download, staging and apply.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("manifest schema {0} is not supported")]
    UnsupportedSchema(u32),
    #[error("manifest channel mismatch: expected {expected}, got {actual}")]
    ChannelMismatch { expected: String, actual: String },
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    #[error("invalid version: {0}")]
    InvalidVersion(String),
    #[error("downgrade from {current} to {candidate} is not allowed")]
    Downgrade { current: String, candidate: String },
    #[error("windows {current} is below the required minimum {minimum}")]
    UnsupportedWindows { minimum: String, current: String },
    #[error("manifest signature is invalid")]
    BadSignature,
    #[error("manifest signature is malformed")]
    BadSignatureEncoding,
    #[error("update public key is missing or malformed")]
    BadPublicKey,
    #[error("hash mismatch for {path}")]
    HashMismatch { path: PathBuf },
    #[error("{path} is {actual} bytes, above the limit of {limit} bytes")]
    TooLarge {
        path: PathBuf,
        limit: u64,
        actual: u64,
    },
    #[error("{path} is {actual} bytes, expected {expected}")]
    SizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("delta base package hash does not match")]
    DeltaBaseMismatch,
    #[error("delta target package hash does not match")]
    DeltaTargetMismatch,
    #[error("invalid delta: {0}")]
    InvalidDelta(String),
    #[error("invalid package: {0}")]
    InvalidPackage(String),
    #[error("insecure URL rejected: {url}")]
    InsecureUrl { url: String },
    #[error("download failed with HTTP status {status} for {url}")]
    HttpStatus { url: String, status: u16 },
    #[error("download was cancelled")]
    Cancelled,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("network error: {0}")]
    Network(String),
    #[error("no staged update at {0}")]
    NotStaged(PathBuf),
    #[error("update apply failed: {0}")]
    Apply(String),
    #[error("invalid update state: {0}")]
    InvalidState(String),
    #[error("new version did not confirm startup within {0} seconds")]
    LaunchTimeout(u64),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

impl UpdateError {
    /// Attach a path to an I/O error.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Short stable code suitable for logs and helper exit messages.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedSchema(_) => "manifest-schema",
            Self::ChannelMismatch { .. } => "manifest-channel",
            Self::InvalidManifest(_) => "manifest-invalid",
            Self::InvalidVersion(_) => "version-invalid",
            Self::Downgrade { .. } => "version-downgrade",
            Self::UnsupportedWindows { .. } => "windows-unsupported",
            Self::BadSignature => "signature-invalid",
            Self::BadSignatureEncoding => "signature-encoding",
            Self::BadPublicKey => "public-key-invalid",
            Self::HashMismatch { .. } => "hash-mismatch",
            Self::TooLarge { .. } => "too-large",
            Self::SizeMismatch { .. } => "size-mismatch",
            Self::DeltaBaseMismatch => "delta-base-mismatch",
            Self::DeltaTargetMismatch => "delta-target-mismatch",
            Self::InvalidDelta(_) => "delta-invalid",
            Self::InvalidPackage(_) => "package-invalid",
            Self::InsecureUrl { .. } => "url-insecure",
            Self::HttpStatus { .. } => "http-status",
            Self::Cancelled => "cancelled",
            Self::Io { .. } => "io",
            Self::Network(_) => "network",
            Self::NotStaged(_) => "not-staged",
            Self::Apply(_) => "apply-failed",
            Self::InvalidState(_) => "state-invalid",
            Self::LaunchTimeout(_) => "launch-timeout",
            Self::InvalidArgument(_) => "argument-invalid",
        }
    }
}

/// Remove query, fragment and credentials from a URL before it reaches a log.
pub fn redact_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => "<invalid-url>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_query_and_credentials() {
        let text = redact_url("https://user:secret@example.invalid/app?token=abc#frag");
        assert_eq!(text, "https://example.invalid/app");
        assert!(!text.contains("abc"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn redacts_invalid_urls() {
        assert_eq!(redact_url("not a url"), "<invalid-url>");
    }
}
