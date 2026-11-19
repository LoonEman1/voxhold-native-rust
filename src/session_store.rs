//! The optional «Запомнить вход» record: a DPAPI-protected list of sessions.
//!
//! The file is `%APPDATA%\Voxhold\sessions.bin` and starts with the magic
//! `VXSS1` followed by one `CryptProtectData` blob. Only the current Windows
//! user can decrypt it; the JSON inside never leaves memory unredacted and the
//! decrypted buffer is zeroed on drop. A password is never part of a record.
//!
//! Every read-modify-write (save, delete, rotation) runs under one in-process
//! mutex and one named mutex, and re-reads the file before changing it, so two
//! windows or two processes cannot drop each other's records. A record is keyed
//! by the exact origin (scheme, host and port) and the account without regard
//! to case; the token only ever goes back to that origin.
//!
//! The store also owns a generation counter. A background write captures it and
//! passes it back; a logout or a new login bumps it, so a write that was queued
//! before the change is refused with [`StoreError::Stale`] instead of reviving
//! or deleting the session that replaced it. Loading is bounded (256 KiB, 16
//! records) and every record is validated before it is returned.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// File prefix before the DPAPI blob.
pub const MAGIC: &[u8; 5] = b"VXSS1";
/// Additional entropy: a blob from another app cannot be decrypted here.
const ENTROPY: &[u8] = b"VoxHold session v1";
/// Named mutex shared by every Voxhold process in this session.
#[cfg(windows)]
const MUTEX_NAME: &str = "Local\\VoxHold.SessionStore.v1";
/// How long a writer waits for another process before giving up.
const LOCK_TIMEOUT_MS: u32 = 5_000;
/// The largest file worth decrypting. Sixteen records are a few kilobytes; a
/// bigger file is damage, not a record list.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// How many remembered logins one file may hold. The oldest are dropped first.
const MAX_RECORDS: usize = 16;

/// One remembered login. The token is the credential, so the manual `Debug`
/// below always prints `<redacted>`; no error or log line carries it either.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedSession {
    /// Canonical origin, exactly as the HTTP/WS client uses it.
    pub origin: String,
    /// Account name as the server returned it at login.
    pub account: String,
    pub user_id: i64,
    pub token: String,
    /// Unix seconds from the login/refresh answer.
    pub expires_at: i64,
    /// Unix seconds of the last successful save.
    pub saved_at: i64,
}

impl std::fmt::Debug for RememberedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RememberedSession")
            .field("origin", &self.origin)
            .field("account", &self.account)
            .field("user_id", &self.user_id)
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("saved_at", &self.saved_at)
            .finish()
    }
}

impl RememberedSession {
    pub fn is_expired(&self, now: i64) -> bool {
        now >= self.expires_at
    }
}

/// Why a store operation failed. The texts are deliberately generic: neither a
/// path nor a token may reach a log or a visible error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The file is not ours, the blob cannot be decrypted, or the JSON is bad.
    Corrupt,
    /// Another process held the named mutex for longer than the timeout.
    Lock,
    /// Local I/O refused the read or the write.
    Io,
    /// A record that cannot be stored (empty origin, account or token).
    Invalid,
    /// The session that scheduled this write has already ended; nothing was
    /// written and nothing was deleted.
    Stale,
    /// DPAPI or the named mutex is not available on this platform.
    #[allow(dead_code)]
    Unsupported,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StoreError::Corrupt => "хранилище сессий повреждено",
            StoreError::Lock => "хранилище сессий занято",
            StoreError::Io => "хранилище сессий недоступно",
            StoreError::Invalid => "некорректная запись сессии",
            StoreError::Stale => "сессия уже завершена",
            StoreError::Unsupported => "хранилище сессий не поддерживается",
        })
    }
}

impl std::error::Error for StoreError {}

/// One owner for the file. `Clone` is cheap and every clone shares the same
/// in-process lock and generation counter; the named mutex is taken per
/// operation.
#[derive(Clone)]
pub struct SessionStore {
    path: Option<PathBuf>,
    lock: Arc<Mutex<()>>,
    epoch: Arc<AtomicU64>,
}

impl SessionStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
            epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The current generation of the signed-in session. A background write
    /// captures this value and passes it back, so a logout that happens while
    /// the write is queued invalidates it instead of resurrecting the record.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// End the current generation. Every write that captured an older value is
    /// refused with [`StoreError::Stale`]; returns the new generation.
    pub fn bump_epoch(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// `%APPDATA%\Voxhold\sessions.bin`, with the same fallbacks as the other
    /// local files. `None` disables the store (tests and unsupported setups).
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("Voxhold").join("sessions.bin"))
    }

    /// The newest usable record for this exact origin. A different scheme, host
    /// or port never matches, so a token cannot travel to another instance.
    ///
    /// A record that has already expired is only returned when every record for
    /// the origin is expired: the newest stale one is then handed back so the
    /// caller can delete it. This keeps a dead newer record from hiding an
    /// older token that still works.
    pub fn load_for_origin(
        &self,
        origin: &str,
        now: i64,
    ) -> Result<Option<RememberedSession>, StoreError> {
        let records = self.load_all()?;
        let matching: Vec<RememberedSession> = records
            .into_iter()
            .filter(|record| record.origin == origin)
            .collect();
        if let Some(valid) = matching
            .iter()
            .filter(|record| !record.is_expired(now))
            .max_by_key(|record| record.saved_at)
        {
            return Ok(Some(valid.clone()));
        }
        Ok(matching.into_iter().max_by_key(|record| record.saved_at))
    }

    pub fn load_all(&self) -> Result<Vec<RememberedSession>, StoreError> {
        let Some(path) = &self.path else {
            return Ok(Vec::new());
        };
        let _local = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        read_records(path)
    }

    /// Insert or replace the record for `(origin, account)`. A corrupt file is
    /// treated as empty: a manual login with the box checked may repair it.
    /// `expected_epoch` must still be the current generation.
    pub fn save(&self, record: &RememberedSession, expected_epoch: u64) -> Result<(), StoreError> {
        if !valid_record(record) {
            return Err(StoreError::Invalid);
        }
        self.write_records(expected_epoch, |records| {
            records.retain(|current| !same_key(current, &record.origin, &record.account));
            records.push(record.clone());
            trim_records(records);
            true
        })
        .map(|_| ())
    }

    /// Drop the record for `(origin, account)`. Account comparison ignores
    /// case; the origin is exact. `expected_epoch` must still be current.
    pub fn delete(
        &self,
        origin: &str,
        account: &str,
        expected_epoch: u64,
    ) -> Result<(), StoreError> {
        self.write_records(expected_epoch, |records| {
            let before = records.len();
            records.retain(|current| !same_key(current, origin, account));
            records.len() != before
        })
        .map(|_| ())
    }

    /// Replace the stored token after a successful rotation, but only when the
    /// record still belongs to this account and carries one of the accepted
    /// tokens. Returns whether the file changed. A failed attempt leaves the
    /// record untouched so the next rotation can catch up with the previous
    /// stored token. A stale generation changes nothing.
    pub fn update_after_refresh(
        &self,
        origin: &str,
        account: &str,
        accepted_tokens: &[&str],
        new_token: &str,
        expires_at: i64,
        expected_epoch: u64,
    ) -> Result<bool, StoreError> {
        if new_token.is_empty() || expires_at <= 0 {
            return Ok(false);
        }
        let accepted: Vec<&str> = accepted_tokens
            .iter()
            .copied()
            .filter(|token| !token.is_empty())
            .collect();
        if accepted.is_empty() {
            return Ok(false);
        }
        self.write_records(expected_epoch, |records| {
            let Some(record) = records
                .iter_mut()
                .find(|record| same_key(record, origin, account))
            else {
                return false;
            };
            if !accepted.iter().any(|token| *token == record.token) {
                return false;
            }
            record.token = new_token.to_owned();
            record.expires_at = expires_at;
            true
        })
    }

    /// Serialize one read-modify-write under both locks. The generation is
    /// checked only after the locks are held, so a logout that bumped it while
    /// this write waited for the file cannot be overwritten by a late save.
    /// The closure returns whether it changed anything; the file is rewritten
    /// only when it did.
    fn write_records(
        &self,
        expected_epoch: u64,
        change: impl FnOnce(&mut Vec<RememberedSession>) -> bool,
    ) -> Result<bool, StoreError> {
        let Some(path) = &self.path else {
            return Ok(false);
        };
        let _local = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _named = NamedLock::acquire()?;
        if self.epoch() != expected_epoch {
            return Err(StoreError::Stale);
        }
        let mut records = match read_records(path) {
            Ok(records) => records,
            // A manual save or a logout may replace a damaged file. A failed
            // read must never turn into a panic or a secret in a message.
            Err(StoreError::Corrupt) => Vec::new(),
            Err(error) => return Err(error),
        };
        if !change(&mut records) {
            return Ok(false);
        }
        write_records_file(path, &records)?;
        Ok(true)
    }
}

fn same_key(record: &RememberedSession, origin: &str, account: &str) -> bool {
    record.origin == origin && accounts_match(&record.account, account)
}

/// Account comparison shared by the store and the automatic login: trim and a
/// Unicode-aware lower case, so «Нева» and «нева» are the same account. The
/// backend applies NFKC plus lower case; the local file never needs the
/// normalization form because both sides go through this one function.
pub(crate) fn accounts_match(left: &str, right: &str) -> bool {
    let (left, right) = (left.trim(), right.trim());
    left == right || left.to_lowercase() == right.to_lowercase()
}

/// One record as it is allowed to leave the file: a canonical origin, a real
/// account and token, and a positive identity and expiry.
fn valid_record(record: &RememberedSession) -> bool {
    !record.account.trim().is_empty()
        && !record.token.is_empty()
        && record.user_id > 0
        && record.expires_at > 0
        && canonical_origin(&record.origin).is_some_and(|origin| origin == record.origin)
}

fn canonical_origin(origin: &str) -> Option<String> {
    voxhold_api::Client::new(origin)
        .ok()
        .map(|client| client.origin())
}

/// Keep the newest `MAX_RECORDS` records. Only a file over the limit is sorted,
/// so an ordinary two-record file keeps its insertion order.
fn trim_records(records: &mut Vec<RememberedSession>) {
    if records.len() <= MAX_RECORDS {
        return;
    }
    records.sort_by(|left, right| right.saved_at.cmp(&left.saved_at));
    records.truncate(MAX_RECORDS);
}

fn read_records(path: &Path) -> Result<Vec<RememberedSession>, StoreError> {
    // A file used as a directory is an unavailable store, not a missing one.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && parent.exists() && !parent.is_dir() {
            return Err(StoreError::Io);
        }
    }
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_FILE_BYTES => return Err(StoreError::Corrupt),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(StoreError::Io),
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(StoreError::Io),
    };
    if bytes.len() <= MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Err(StoreError::Corrupt);
    }
    let plain = unprotect(&bytes[MAGIC.len()..])?;
    let mut records = serde_json::from_slice::<Vec<RememberedSession>>(&plain)
        .map_err(|_| StoreError::Corrupt)?;
    // A single damaged record must not take the whole list down with it.
    records.retain(valid_record);
    trim_records(&mut records);
    Ok(records)
}

fn write_records_file(path: &Path, records: &[RememberedSession]) -> Result<(), StoreError> {
    let json = Zeroizing::new(serde_json::to_vec(records).map_err(|_| StoreError::Corrupt)?);
    let blob = protect(&json)?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + blob.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&blob);
    crate::instance_prefs::write_atomic(path, &bytes).map_err(|_| StoreError::Io)
}

/// `CryptProtectData` for the current user, with fixed entropy and no UI.
#[cfg(windows)]
fn protect(plain: &[u8]) -> Result<Vec<u8>, StoreError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr() as *mut u8,
    };
    let mut entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            std::ptr::null(),
            &mut entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(StoreError::Corrupt);
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Ok(bytes)
}

/// The decrypted buffer is wrapped in `Zeroizing`, so a failed parse still
/// wipes the token from memory.
#[cfg(windows)]
fn unprotect(blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(),
            &mut entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(StoreError::Corrupt);
    }
    let bytes = Zeroizing::new(unsafe {
        std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
    });
    unsafe {
        // The decrypted copy is already wrapped in `Zeroizing`; the DPAPI buffer
        // still holds the same plaintext until it is freed, so wipe it first.
        std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Ok(bytes)
}

#[cfg(not(windows))]
fn protect(_plain: &[u8]) -> Result<Vec<u8>, StoreError> {
    Err(StoreError::Unsupported)
}

#[cfg(not(windows))]
fn unprotect(_blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, StoreError> {
    Err(StoreError::Unsupported)
}

/// A scoped named mutex. Dropping the guard releases it even on a panic.
#[cfg(windows)]
struct NamedLock(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl NamedLock {
    fn acquire() -> Result<Self, StoreError> {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

        let name: Vec<u16> = MUTEX_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(StoreError::Lock);
        }
        let wait = unsafe { WaitForSingleObject(handle, LOCK_TIMEOUT_MS) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Ok(Self(handle))
        } else {
            unsafe {
                CloseHandle(handle);
            }
            Err(StoreError::Lock)
        }
    }
}

#[cfg(windows)]
impl Drop for NamedLock {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

#[cfg(not(windows))]
struct NamedLock;

#[cfg(not(windows))]
impl NamedLock {
    fn acquire() -> Result<Self, StoreError> {
        Err(StoreError::Unsupported)
    }
}

#[cfg(all(test, windows))]
mod tests;
