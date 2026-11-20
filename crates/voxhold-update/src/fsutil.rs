//! Small retrying filesystem helpers shared by apply and recovery.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::{Result, UpdateError};

/// Rename, retrying while a handle is released.
pub(crate) fn rename_retry(from: &Path, to: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
                let _ = error;
            }
            Err(error) => return Err(UpdateError::io(to, error)),
        }
    }
}

/// Remove a directory tree, retrying until `timeout` expires.
pub(crate) fn remove_dir_with_timeout(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
                let _ = error;
            }
            Err(error) => return Err(UpdateError::io(path, error)),
        }
    }
}

/// Remove a directory tree with the default retry budget.
pub(crate) fn remove_dir_retry(path: &Path) -> Result<()> {
    remove_dir_with_timeout(path, Duration::from_secs(10))
}
