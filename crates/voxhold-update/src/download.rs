//! Background download with cancellation, a size limit and `Range` resume.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;

use crate::error::{redact_url, Result, UpdateError};
use crate::manifest::MAX_PACKAGE_BYTES;
use crate::urls::{is_loopback_host, validate_update_url};

/// Extra bytes allowed above the manifest size.
pub const SIZE_MARGIN: u64 = 1024 * 1024;
/// Timeout for one body chunk.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);

/// Cooperative cancellation shared with the UI.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Safe to call from any thread and more than once.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub(crate) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

/// Progress callback: `(downloaded_total_bytes, expected_total_bytes)`.
pub type ProgressFn = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

/// One download request.
pub struct DownloadRequest<'a> {
    pub url: &'a str,
    pub dest: &'a Path,
    pub expected_size: u64,
    pub expected_sha256: &'a str,
    pub max_bytes: u64,
    pub cancel: CancelToken,
    pub progress: Option<ProgressFn>,
}

/// Successful download.
#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub bytes: u64,
    pub sha256: String,
    pub resumed_from: u64,
}

/// Path of the partial file next to `dest`.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// Download `url` into `dest`, resuming a `.part` file when possible.
pub async fn download(request: DownloadRequest<'_>) -> Result<DownloadOutcome> {
    let url = validate_update_url(request.url)?;
    if request.expected_size == 0 || request.expected_size > MAX_PACKAGE_BYTES {
        return Err(UpdateError::InvalidArgument(
            "expected size is outside the supported range".into(),
        ));
    }
    if request.max_bytes < request.expected_size {
        return Err(UpdateError::InvalidArgument(
            "size limit is below the expected size".into(),
        ));
    }
    let part = part_path(request.dest);
    if let Some(parent) = part.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| UpdateError::io(parent, error))?;
    }

    let mut offset = tokio::fs::metadata(&part)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if offset > request.expected_size {
        remove_if_exists(&part).await?;
        offset = 0;
    }
    if offset == request.expected_size {
        if hash_file(&part).await? == request.expected_sha256 {
            replace_file(&part, request.dest).await?;
            notify(&request.progress, offset, Some(request.expected_size));
            return Ok(DownloadOutcome {
                bytes: offset,
                sha256: request.expected_sha256.to_string(),
                resumed_from: offset,
            });
        }
        remove_if_exists(&part).await?;
        offset = 0;
    }

    if request.cancel.is_cancelled() {
        remove_if_exists(&part).await?;
        return Err(UpdateError::Cancelled);
    }

    let client = build_client()?;
    let mut builder = client.get(url.clone());
    if offset > 0 {
        builder = builder.header(RANGE, format!("bytes={offset}-"));
    }
    let response = builder.send().await.map_err(network_error)?;
    let status = response.status();
    let resumed_from = offset;
    let mut start_offset = offset;
    match status {
        StatusCode::OK => {
            start_offset = 0;
        }
        StatusCode::PARTIAL_CONTENT => {
            let range = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_content_range)
                .ok_or_else(|| UpdateError::Network("missing Content-Range".into()))?;
            if range.start != offset {
                remove_if_exists(&part).await?;
                return Err(UpdateError::Network(
                    "server resumed at a different offset".into(),
                ));
            }
            if let Some(total) = range.total {
                if total != request.expected_size {
                    remove_if_exists(&part).await?;
                    return Err(UpdateError::SizeMismatch {
                        path: part,
                        expected: request.expected_size,
                        actual: total,
                    });
                }
            }
        }
        StatusCode::RANGE_NOT_SATISFIABLE => {
            remove_if_exists(&part).await?;
            return Err(UpdateError::HttpStatus {
                url: redact_url(request.url),
                status: status.as_u16(),
            });
        }
        other => {
            remove_if_exists(&part).await?;
            return Err(UpdateError::HttpStatus {
                url: redact_url(request.url),
                status: other.as_u16(),
            });
        }
    }

    if let Some(length) = response.content_length() {
        let total = length.saturating_add(start_offset);
        if total > request.max_bytes {
            remove_if_exists(&part).await?;
            return Err(UpdateError::TooLarge {
                path: part,
                limit: request.max_bytes,
                actual: total,
            });
        }
    }

    let mut file = if start_offset == 0 {
        tokio::fs::File::create(&part)
            .await
            .map_err(|error| UpdateError::io(&part, error))?
    } else {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .await
            .map_err(|error| UpdateError::io(&part, error))?
    };

    let mut written = start_offset;
    notify(&request.progress, written, Some(request.expected_size));
    let mut response = response;
    loop {
        let chunk = tokio::select! {
            _ = request.cancel.cancelled() => {
                drop(file);
                remove_if_exists(&part).await?;
                return Err(UpdateError::Cancelled);
            }
            chunk = tokio::time::timeout(CHUNK_TIMEOUT, response.chunk()) => {
                match chunk {
                    Err(_) => {
                        let _ = file.flush().await;
                        drop(file);
                        return Err(UpdateError::Network("body read timed out".into()));
                    }
                    Ok(Err(error)) => {
                        let _ = file.flush().await;
                        drop(file);
                        return Err(network_error(error));
                    }
                    Ok(Ok(None)) => break,
                    Ok(Ok(Some(bytes))) => bytes,
                }
            }
        };
        written = written.saturating_add(chunk.len() as u64);
        if written > request.max_bytes {
            drop(file);
            remove_if_exists(&part).await?;
            return Err(UpdateError::TooLarge {
                path: part,
                limit: request.max_bytes,
                actual: written,
            });
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| UpdateError::io(&part, error))?;
        notify(&request.progress, written, Some(request.expected_size));
    }
    file.flush()
        .await
        .map_err(|error| UpdateError::io(&part, error))?;
    drop(file);

    let actual = tokio::fs::metadata(&part)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if actual != request.expected_size {
        remove_if_exists(&part).await?;
        return Err(UpdateError::SizeMismatch {
            path: part,
            expected: request.expected_size,
            actual,
        });
    }
    let digest = hash_file(&part).await?;
    if digest != request.expected_sha256 {
        remove_if_exists(&part).await?;
        return Err(UpdateError::HashMismatch { path: part });
    }
    replace_file(&part, request.dest).await?;
    Ok(DownloadOutcome {
        bytes: actual,
        sha256: digest,
        resumed_from,
    })
}

pub(crate) fn build_client() -> Result<reqwest::Client> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if is_allowed(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("redirect to an insecure URL")
        }
    });
    reqwest::Client::builder()
        .redirect(policy)
        .connect_timeout(Duration::from_secs(15))
        .user_agent("VoxHold-Updater/1")
        .build()
        .map_err(network_error)
}

fn is_allowed(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => url.host_str().map(is_loopback_host).unwrap_or(false),
        _ => false,
    }
}

fn network_error(error: reqwest::Error) -> UpdateError {
    UpdateError::Network(error.without_url().to_string())
}

fn notify(progress: &Option<ProgressFn>, written: u64, total: Option<u64>) {
    if let Some(callback) = progress {
        callback(written, total);
    }
}

async fn remove_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(UpdateError::io(path, error)),
    }
}

async fn replace_file(from: &Path, to: &Path) -> Result<()> {
    remove_if_exists(to).await?;
    tokio::fs::rename(from, to)
        .await
        .map_err(|error| UpdateError::io(to, error))
}

async fn hash_file(path: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| UpdateError::io(path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| UpdateError::io(path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Parsed `Content-Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

/// Parse `bytes <start>-<end>/<total|*>`.
pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let value = value.trim();
    let rest = value.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    if end < start {
        return None;
    }
    let total = if total.trim() == "*" {
        None
    } else {
        Some(total.trim().parse::<u64>().ok()?)
    };
    if let Some(total) = total {
        if end >= total {
            return None;
        }
    }
    Some(ContentRange { start, end, total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(
            parse_content_range("bytes 10-19/100"),
            Some(ContentRange {
                start: 10,
                end: 19,
                total: Some(100)
            })
        );
        assert_eq!(
            parse_content_range("bytes 0-0/*"),
            Some(ContentRange {
                start: 0,
                end: 0,
                total: None
            })
        );
        assert_eq!(parse_content_range("bytes 10-9/100"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
        assert_eq!(parse_content_range("bytes 0-100/100"), None);
    }

    #[test]
    fn cancellation_is_observable() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }
}
