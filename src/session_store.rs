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

use std::path::{Path, PathBuf};
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
            StoreError::Unsupported => "хранилище сессий не поддерживается",
        })
    }
}

impl std::error::Error for StoreError {}

/// One owner for the file. `Clone` is cheap and every clone shares the same
/// in-process lock; the named mutex is taken per operation.
#[derive(Clone)]
pub struct SessionStore {
    path: Option<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl SessionStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
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

    /// The newest record for this exact origin. A different scheme, host or
    /// port never matches, so a token cannot travel to another instance.
    pub fn load_for_origin(&self, origin: &str) -> Result<Option<RememberedSession>, StoreError> {
        let records = self.load_all()?;
        Ok(records
            .into_iter()
            .filter(|record| record.origin == origin)
            .max_by_key(|record| record.saved_at))
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
    pub fn save(&self, record: &RememberedSession) -> Result<(), StoreError> {
        if record.origin.trim().is_empty()
            || record.account.trim().is_empty()
            || record.token.is_empty()
            || record.user_id <= 0
            || record.expires_at <= 0
        {
            return Err(StoreError::Invalid);
        }
        self.write_records(|records| {
            records.retain(|current| !same_key(current, &record.origin, &record.account));
            records.push(record.clone());
            true
        })
        .map(|_| ())
    }

    /// Drop the record for `(origin, account)`. Account comparison ignores
    /// case; the origin is exact.
    pub fn delete(&self, origin: &str, account: &str) -> Result<(), StoreError> {
        self.write_records(|records| {
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
    /// stored token.
    pub fn update_after_refresh(
        &self,
        origin: &str,
        account: &str,
        accepted_tokens: &[&str],
        new_token: &str,
        expires_at: i64,
    ) -> bool {
        if new_token.is_empty() || expires_at <= 0 {
            return false;
        }
        let accepted: Vec<&str> = accepted_tokens
            .iter()
            .copied()
            .filter(|token| !token.is_empty())
            .collect();
        if accepted.is_empty() {
            return false;
        }
        self.write_records(|records| {
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
        .unwrap_or(false)
    }

    /// Serialize one read-modify-write under both locks. The closure returns
    /// whether it changed anything; the file is rewritten only when it did.
    fn write_records(
        &self,
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
    record.origin == origin && account_eq(&record.account, account)
}

fn account_eq(left: &str, right: &str) -> bool {
    left.trim() == right.trim() || left.trim().to_lowercase() == right.trim().to_lowercase()
}

fn read_records(path: &Path) -> Result<Vec<RememberedSession>, StoreError> {
    // A file used as a directory is an unavailable store, not a missing one.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && parent.exists() && !parent.is_dir() {
            return Err(StoreError::Io);
        }
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
    serde_json::from_slice::<Vec<RememberedSession>>(&plain).map_err(|_| StoreError::Corrupt)
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
mod tests {
    use super::*;

    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "voxhold-session-store-{}-{}-{name}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("sessions.bin"))
    }

    fn record(origin: &str, account: &str, token: &str) -> RememberedSession {
        RememberedSession {
            origin: origin.into(),
            account: account.into(),
            user_id: 7,
            token: token.into(),
            expires_at: crate::auth_session::now_unix() + 3_600,
            saved_at: crate::auth_session::now_unix(),
        }
    }

    #[test]
    fn a_record_round_trips_through_dpapi_without_plaintext() {
        let (dir, path) = scratch("roundtrip");
        let store = SessionStore::new(Some(path.clone()));
        let saved = record("https://chat.example.org:8443", "neva", "token-secret");
        store.save(&saved).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..MAGIC.len()], MAGIC);
        assert!(
            !bytes
                .windows(saved.token.len())
                .any(|w| w == saved.token.as_bytes()),
            "the file must not contain the token"
        );
        assert_eq!(store.load_all().unwrap(), vec![saved.clone()]);
        assert_eq!(
            store
                .load_for_origin("https://chat.example.org:8443")
                .unwrap(),
            Some(saved)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_empty_and_a_bad_magic_is_corrupt() {
        let (dir, path) = scratch("magic");
        let store = SessionStore::new(Some(path.clone()));
        assert!(store.load_all().unwrap().is_empty());

        std::fs::write(&path, b"not our file").unwrap();
        assert_eq!(store.load_all(), Err(StoreError::Corrupt));
        std::fs::write(&path, MAGIC).unwrap();
        assert_eq!(store.load_all(), Err(StoreError::Corrupt));

        let mut foreign = MAGIC.to_vec();
        foreign.extend_from_slice(b"garbage blob");
        std::fs::write(&path, &foreign).unwrap();
        assert_eq!(store.load_all(), Err(StoreError::Corrupt));
        // A save repairs a damaged file instead of refusing the login.
        let saved = record("https://chat.example.org", "neva", "token-1");
        store.save(&saved).unwrap();
        assert_eq!(store.load_all().unwrap(), vec![saved]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_valid_blob_with_broken_json_is_corrupt_without_a_panic() {
        let (dir, path) = scratch("json");
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&protect(b"not json").unwrap());
        std::fs::write(&path, bytes).unwrap();
        let store = SessionStore::new(Some(path));
        assert_eq!(store.load_all(), Err(StoreError::Corrupt));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_origin_including_the_port_must_match_exactly() {
        let (dir, path) = scratch("origin");
        let store = SessionStore::new(Some(path));
        let first = record("https://chat.example.org:8443", "neva", "token-a");
        let second = record("https://chat.example.org:9443", "neva", "token-b");
        store.save(&first).unwrap();
        store.save(&second).unwrap();

        assert_eq!(
            store
                .load_for_origin("https://chat.example.org:8443")
                .unwrap(),
            Some(first)
        );
        assert_eq!(
            store
                .load_for_origin("https://chat.example.org:9443")
                .unwrap(),
            Some(second)
        );
        assert!(store
            .load_for_origin("https://chat.example.org")
            .unwrap()
            .is_none());
        assert!(store
            .load_for_origin("http://chat.example.org:8443")
            .unwrap()
            .is_none());
        assert_eq!(store.load_all().unwrap().len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_account_ignores_case_and_save_replaces_the_same_key() {
        let (dir, path) = scratch("account");
        let store = SessionStore::new(Some(path));
        store
            .save(&record("https://chat.example.org", "Neva", "token-1"))
            .unwrap();
        store
            .save(&record("https://chat.example.org", "neva", "token-2"))
            .unwrap();
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1, "one record per origin and account");
        assert_eq!(all[0].token, "token-2");

        store.delete("https://chat.example.org", "NEVA").unwrap();
        assert!(store.load_all().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn expiry_is_visible_so_the_caller_can_delete_the_record() {
        let (dir, path) = scratch("expired");
        let store = SessionStore::new(Some(path));
        let mut saved = record("https://chat.example.org", "neva", "token-1");
        saved.expires_at = 100;
        store.save(&saved).unwrap();
        let loaded = store.load_all().unwrap();
        assert!(loaded[0].is_expired(101));
        assert!(!loaded[0].is_expired(99));
        store.delete(&saved.origin, &saved.account).unwrap();
        assert!(store.load_all().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotation_only_replaces_an_accepted_token_and_the_previous_one_can_catch_up() {
        let (dir, path) = scratch("rotation");
        let store = SessionStore::new(Some(path));
        store
            .save(&record("https://chat.example.org", "neva", "token-1"))
            .unwrap();

        assert!(!store.update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-9"],
            "token-2",
            5_000
        ));
        assert_eq!(store.load_all().unwrap()[0].token, "token-1");

        assert!(store.update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-1"],
            "token-2",
            5_000
        ));
        let saved = store.load_all().unwrap();
        assert_eq!(saved[0].token, "token-2");
        assert_eq!(saved[0].expires_at, 5_000);

        // The first update was lost: the next rotation still knows the token
        // that was actually stored and catches up with it.
        assert!(store.update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-3", "token-2"],
            "token-4",
            6_000
        ));
        assert_eq!(store.load_all().unwrap()[0].token, "token-4");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotation_never_touches_another_origin_or_account() {
        let (dir, path) = scratch("rotation-other");
        let store = SessionStore::new(Some(path));
        store
            .save(&record("https://chat.example.org:8443", "neva", "token-1"))
            .unwrap();
        assert!(!store.update_after_refresh(
            "https://chat.example.org:9443",
            "neva",
            &["token-1"],
            "token-2",
            5_000
        ));
        assert!(!store.update_after_refresh(
            "https://chat.example.org:8443",
            "bob",
            &["token-1"],
            "token-2",
            5_000
        ));
        assert_eq!(store.load_all().unwrap()[0].token, "token-1");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn two_threads_saving_and_deleting_keep_every_other_record() {
        let (dir, path) = scratch("threads");
        let store = SessionStore::new(Some(path));
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for worker in 0..2 {
                let store = store.clone();
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    for index in 0..20 {
                        let account = format!("user-{worker}-{index}");
                        store
                            .save(&record(
                                "https://chat.example.org",
                                &account,
                                &format!("token-{worker}-{index}"),
                            ))
                            .unwrap();
                        if index % 2 == 0 {
                            store.delete("https://chat.example.org", &account).unwrap();
                        }
                    }
                });
            }
        });
        // Each worker leaves ten records; the other thread's records survive.
        assert_eq!(store.load_all().unwrap().len(), 20);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_only_file_reports_an_error_and_stays_intact() {
        struct ReadWrite(PathBuf);

        impl Drop for ReadWrite {
            fn drop(&mut self) {
                if let Ok(metadata) = std::fs::metadata(&self.0) {
                    let mut permissions = metadata.permissions();
                    permissions.set_readonly(false);
                    let _ = std::fs::set_permissions(&self.0, permissions);
                }
            }
        }

        let (dir, path) = scratch("readonly");
        let store = SessionStore::new(Some(path.clone()));
        store
            .save(&record("https://chat.example.org", "neva", "token-1"))
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        let guard = ReadWrite(path.clone());

        let result = store.save(&record("https://chat.example.org", "neva", "token-2"));

        drop(guard);
        assert_eq!(result, Err(StoreError::Io));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["sessions.bin".to_string()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unavailable_directory_reports_an_error() {
        let (dir, path) = scratch("unavailable");
        // The parent is a file, so neither the directory nor the temp file can
        // be created; the call must fail rather than pretend success.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let store = SessionStore::new(Some(blocker.join("sessions.bin")));
        assert!(store
            .save(&record("https://chat.example.org", "neva", "token-1"))
            .is_err());
        assert_eq!(store.load_all(), Err(StoreError::Io));
        std::fs::remove_dir_all(dir).unwrap();
        let _ = path;
    }

    #[test]
    fn a_disabled_store_is_a_quiet_no_op() {
        let store = SessionStore::new(None);
        assert!(store.load_all().unwrap().is_empty());
        assert!(store
            .save(&record("https://chat.example.org", "neva", "token-1"))
            .is_ok());
        assert!(!store.update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-1"],
            "token-2",
            5_000
        ));
    }

    #[test]
    fn debug_output_never_prints_the_token() {
        let saved = record("https://chat.example.org", "neva", "token-secret");
        let debug = format!("{saved:?}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(!debug.contains("token-secret"), "{debug}");
        assert!(debug.contains("neva"), "{debug}");
    }
}
