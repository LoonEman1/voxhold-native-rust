//! The optional «Запомнить вход» record: a DPAPI-protected list of sessions
//! and the one target that the next cold start restores.
//!
//! The file is `%APPDATA%\Voxhold\sessions.bin` and starts with the magic
//! `VXSS1` followed by one `CryptProtectData` blob. Only the current Windows
//! user can decrypt it; the JSON inside never leaves memory unredacted and the
//! decrypted buffer is zeroed on drop. A password is never part of a record.
//!
//! The document now has two shapes. The old shape is a bare JSON array of
//! records without a pointer; it is still readable and is reported as
//! [`StoreState::legacy`]. The new shape is a versioned object that stores the
//! records, exactly one [`AutoLoginTarget`] (`null` means «no target», which
//! must never fall back to a record) and a generation counter. A file after an
//! explicit logout is a new-shape document with no records and no target, so an
//! empty store cannot be mistaken for the old format.
//!
//! Every read-modify-write (save, delete, rotation, cleanup) runs under one
//! in-process mutex and one named mutex, and re-reads the file before changing
//! it, so two windows or two processes cannot drop each other's records. The
//! generation is stored on disk and checked under the lock: a write that
//! captures an older generation is refused with [`StoreError::Stale`] instead of
//! reviving or deleting the session that replaced it. A new login also claims
//! its generation on disk under the same locks, so two processes starting from
//! one file never share a number. A logout cleanup never claims a newer
//! generation: it clears only while the file is still at the generation the
//! logout knew to be on disk, so a login saved after that point survives. A
//! moved file that carries no newer record is only a claim whose save was
//! refused, so the cleanup still clears the records it captured. An automatic
//! delete is scoped to the token of the session that ended. Loading is bounded
//! (256 KiB, 16 records) and every record and target is validated before it is
//! returned.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// File prefix before the DPAPI blob.
pub const MAGIC: &[u8; 5] = b"VXSS1";
/// The version of the object document. A bare array is the old format.
const FORMAT_VERSION: u32 = 2;
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

/// The one automatic login the next cold start may perform. It is bound to the
/// exact origin and the account that actually signed in, so a record of the
/// same account on another port is never substituted.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoLoginTarget {
    pub origin: String,
    pub user_id: i64,
    pub account: String,
}

impl std::fmt::Debug for AutoLoginTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // There is no credential here, but the account and origin stay out of
        // paths that a future diagnostic might forward.
        f.debug_struct("AutoLoginTarget")
            .field("origin", &self.origin)
            .field("user_id", &self.user_id)
            .field("account", &self.account)
            .finish()
    }
}

impl AutoLoginTarget {
    pub fn from_record(record: &RememberedSession) -> Self {
        Self {
            origin: record.origin.clone(),
            user_id: record.user_id,
            account: record.account.clone(),
        }
    }

    /// The record this target names, with the account compared like the store.
    pub fn matches(&self, record: &RememberedSession) -> bool {
        self.origin == record.origin
            && self.user_id == record.user_id
            && accounts_match(&self.account, &record.account)
    }
}

/// Everything one read returns. `legacy` is not persisted: it tells the caller
/// that the file was the old bare array and therefore has no pointer.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct StoreState {
    pub records: Vec<RememberedSession>,
    pub target: Option<AutoLoginTarget>,
    pub legacy: bool,
    pub generation: u64,
    /// The generation of the last write that changed records or the target; a
    /// claim moves only [`generation`](Self::generation), so a logout cleanup
    /// can tell a refused claim from a newer login.
    pub(crate) records_generation: u64,
    pub restore_blocked: bool,
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
    /// The session that scheduled this write has already ended: no record of it
    /// is kept. A claim may have moved the generation or a write may have
    /// reached the file, but the logout cleanup still owns that content.
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

/// What the cold start may do with the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoLogin {
    /// One record was named by the stored target or by an unambiguous legacy
    /// migration. `legacy` asks the caller to persist the chosen target after
    /// the session is accepted.
    Ready {
        record: RememberedSession,
        legacy: bool,
    },
    /// There is nothing to restore: no target, no matching record, an expired
    /// multi-record legacy file or a cleanup marker. The address form opens.
    Manual,
    /// The old format has several suitable records and no pointer to choose
    /// one. The address form opens without any network attempt.
    Ambiguous,
}

/// The persisted object. `records` and `target` are written together, so a
/// manual login with «Запомнить вход» either stores both or neither.
#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    #[serde(default)]
    generation: u64,
    /// Absent in older files: read as `generation`, which keeps the old refusal.
    #[serde(default)]
    records_generation: Option<u64>,
    #[serde(default)]
    target: Option<AutoLoginTarget>,
    #[serde(default)]
    restore_blocked: bool,
    #[serde(default)]
    records: Vec<RememberedSession>,
}

/// One owner for the file. `Clone` is cheap and every clone shares the same
/// in-process lock and generation counter; the named mutex is taken per
/// operation.
#[derive(Clone)]
pub struct SessionStore {
    path: Option<PathBuf>,
    lock: Arc<Mutex<()>>,
    epoch: Arc<AtomicU64>,
    disk: Arc<AtomicU64>,
    /// Test hook: runs inside a write where the UI thread can raise the logout
    /// barrier in production.
    #[cfg(test)]
    write_hook: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}

/// What a write does with a file that is already above its expected generation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Moved {
    /// Another writer with a record of its own moved the file: refuse.
    Refuse,
    /// A logout cleanup may still run when that generation changed no record:
    /// only a claim whose save was refused, so the records are the captured ones.
    ClaimOnly,
}

impl SessionStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
            epoch: Arc::new(AtomicU64::new(0)),
            disk: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            write_hook: Arc::new(Mutex::new(None)),
        }
    }

    /// The current generation of the signed-in session. A background write
    /// captures this value and passes it back, so a logout that happens while
    /// the write is queued invalidates it instead of resurrecting the record.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// The highest generation this process knows to be on the file. It is at or
    /// below the real one: it only moves when a read, a claim or a write
    /// observed that value, never when [`bump_epoch`](Self::bump_epoch) raises
    /// the in-process login barrier.
    ///
    /// A logout captures this value for its cleanup. A login that is saved
    /// after the logout claims a generation above it on the file, so the clear
    /// is refused instead of erasing that login; the in-memory barrier alone
    /// would coincide with the claim of the other window and lose that race.
    pub fn known_disk_generation(&self) -> u64 {
        self.disk.load(Ordering::SeqCst)
    }

    /// End the current generation in this process. Every write that captured an
    /// older value is refused with [`StoreError::Stale`]; returns the new value.
    pub fn bump_epoch(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Start a new login: claim the next generation above every one this
    /// process or the file has seen, under both locks, and write the claim to
    /// disk. Two processes that open the same file therefore never receive the
    /// same number, so a delete or a cleanup from one of them cannot pass the
    /// generation check of a record the other one saved afterwards.
    ///
    /// A value this process already raised in memory (the login barrier of
    /// [`bump_epoch`](Self::bump_epoch)) is kept, so the save that follows the
    /// claim with the returned value is accepted. The claim reads, decrypts and
    /// rewrites the file; it must run on a background executor, never on the
    /// UI thread.
    ///
    /// The application always claims through
    /// [`begin_login_checked`](Self::begin_login_checked), because the answer
    /// that starts a save may already be stale. This unconditional form exists
    /// for the tests that need a fresh generation (a second window signing in).
    #[cfg(test)]
    pub(crate) fn begin_login(&self) -> u64 {
        match self.claim_generation(None) {
            Ok(claim) => claim,
            // The file is held by another process or is unreadable. A strictly
            // newer in-process generation still invalidates every queued write.
            Err(_) => self.bump_epoch(),
        }
    }

    /// The login barrier of a write that was queued under `expected_epoch`,
    /// for a save that asked for a generation without waiting for the answer.
    ///
    /// The claim is refused with [`StoreError::Stale`] as soon as this process
    /// or the file has moved past `expected_epoch`: a logout that happened
    /// while the save waited owns the file now, and a record written with a
    /// freshly claimed generation would revive the session the user ended. The
    /// barrier is checked under both locks before the claim reads the file and
    /// again after that read, immediately before the new generation is written,
    /// so a logout that raised it during the read is still refused. It is
    /// checked once more after the write, because the barrier is raised without
    /// a lock while the file is replaced; the caller must then not save. The
    /// claim wrote no record, so the logout cleanup still clears the file.
    pub fn begin_login_checked(&self, expected_epoch: u64) -> Result<u64, StoreError> {
        self.claim_generation(Some(expected_epoch))
    }

    /// Claim the next login generation, optionally only when the store has not
    /// moved past `checked`. A stale caller cannot claim a new generation.
    /// I/O and lock failures are returned so a failed save can be reported.
    fn claim_generation(&self, checked: Option<u64>) -> Result<u64, StoreError> {
        let Some(path) = &self.path else {
            if checked.is_some_and(|expected| self.epoch() != expected) {
                return Err(StoreError::Stale);
            }
            return Ok(self.bump_epoch());
        };
        let _local = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if checked.is_some_and(|expected| self.epoch() != expected) {
            return Err(StoreError::Stale);
        }
        let _named = NamedLock::acquire()?;
        let mut state = match read_state(path) {
            Ok(state) => state,
            // A claim may replace a damaged file, exactly like a manual save.
            Err(StoreError::Corrupt) => StoreState::default(),
            Err(error) => return Err(error),
        };
        // Reading and decrypting is the long part of the claim. A logout that
        // raised the barrier during it owns the session now, so the claim is
        // refused before it reserves a generation above that logout.
        if checked.is_some_and(|expected| self.epoch() != expected) {
            return Err(StoreError::Stale);
        }
        let disk = state.generation;
        if checked.is_some_and(|expected| disk > expected) {
            // Another process or window logged in or out while this save was
            // queued. The next write of this store starts from that generation.
            self.sync_epoch(disk);
            return Err(StoreError::Stale);
        }
        let claim = self.epoch().max(disk);
        // Strictly above the file, and never below the in-process barrier.
        let claim = if claim <= disk {
            disk.saturating_add(1)
        } else {
            claim
        };
        // The last point before the claim reaches the file; a logout raises the
        // barrier without taking either lock, so it can land here.
        #[cfg(test)]
        self.run_write_hook();
        state.generation = claim;
        write_state_file(path, &state)?;
        // Read before `sync_epoch`, which moves the in-process generation to
        // this very claim and would otherwise look like a logout.
        let moved = checked.is_some_and(|expected| self.epoch() > expected);
        self.sync_epoch(claim);
        if moved {
            // The logout won while this claim was written. The file moved to a
            // generation without a record, so the cleanup of that logout still
            // owns every record; the save must not run, or it would revive the
            // session the user ended. The newer value must not reach it either.
            return Err(StoreError::Stale);
        }
        // A concurrent barrier of this process may already be above the claim;
        // the caller must use that newer value for its own write.
        Ok(self.epoch())
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

    /// One snapshot: records, the pointer, the disk generation and the format.
    /// The in-process generation is raised to the disk one, so the caller can
    /// use [`epoch`](Self::epoch) for the next write.
    pub fn state(&self) -> Result<StoreState, StoreError> {
        self.read_state_synced()
    }

    /// The newest usable record for this exact origin. A different scheme, host
    /// or port never matches, so a token cannot travel to another instance.
    ///
    /// A record that has already expired is only returned when every record for
    /// the origin is expired: the newest stale one is then handed back so the
    /// caller can delete it. This keeps a dead newer record from hiding an
    /// older token that still works.
    ///
    /// Only the tests read a record directly; the application goes through
    /// [`plan_auto_login`](Self::plan_auto_login).
    #[cfg(test)]
    pub(crate) fn load_for_origin(
        &self,
        origin: &str,
        now: i64,
    ) -> Result<Option<RememberedSession>, StoreError> {
        let state = self.read_state_synced()?;
        let matching: Vec<RememberedSession> = state
            .records
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

    /// Every stored record. It exists for the tests: the application reads the
    /// store only through [`plan_auto_login`](Self::plan_auto_login) and the
    /// single-target snapshot from [`state`](Self::state).
    #[cfg(test)]
    pub(crate) fn load_all(&self) -> Result<Vec<RememberedSession>, StoreError> {
        Ok(self.read_state_synced()?.records)
    }

    /// Decide whether the cold start may restore a session.
    ///
    /// * A new-format target is used only when `explicit_origin` is absent or
    ///   exactly equal to the target origin. A mismatch opens the form for the
    ///   explicit address and sends no token.
    /// * A legacy file has no pointer. With an explicit origin only the single
    ///   usable record for that exact origin is chosen. Without one, a single
    ///   usable record in the whole profile is chosen when the caller allows the
    ///   profile-wide fallback. Everything else is manual or ambiguous.
    pub fn plan_auto_login(
        &self,
        now: i64,
        explicit_origin: Option<&str>,
        allow_profile_single: bool,
    ) -> Result<AutoLogin, StoreError> {
        let state = self.read_state_synced()?;
        if state.restore_blocked {
            return Ok(AutoLogin::Manual);
        }
        if let Some(target) = state.target {
            if explicit_origin.is_some_and(|origin| origin != target.origin) {
                return Ok(AutoLogin::Manual);
            }
            let Some(record) = state.records.iter().find(|record| target.matches(record)) else {
                return Ok(AutoLogin::Manual);
            };
            return Ok(AutoLogin::Ready {
                record: record.clone(),
                legacy: false,
            });
        }
        if !state.legacy {
            // «No target» in the new format is authoritative: the records of an
            // earlier login must not be searched again.
            return Ok(AutoLogin::Manual);
        }
        let usable: Vec<&RememberedSession> = state
            .records
            .iter()
            .filter(|record| !record.is_expired(now))
            .collect();
        if let Some(origin) = explicit_origin {
            let matching: Vec<&RememberedSession> = usable
                .iter()
                .copied()
                .filter(|record| record.origin == origin)
                .collect();
            return match matching.as_slice() {
                [record] => Ok(AutoLogin::Ready {
                    record: (**record).clone(),
                    legacy: true,
                }),
                [] => Ok(AutoLogin::Manual),
                _ => Ok(AutoLogin::Ambiguous),
            };
        }
        if !allow_profile_single {
            return Ok(AutoLogin::Manual);
        }
        match usable.as_slice() {
            [record] => Ok(AutoLogin::Ready {
                record: (**record).clone(),
                legacy: true,
            }),
            [] => Ok(AutoLogin::Manual),
            _ => Ok(AutoLogin::Ambiguous),
        }
    }

    /// Store a manual login and make it the one target of the next start. A
    /// successful save also lifts a cleanup marker: this login supersedes it.
    pub fn save(&self, record: &RememberedSession, expected_epoch: u64) -> Result<(), StoreError> {
        if !valid_record(record) {
            return Err(StoreError::Invalid);
        }
        self.mutate(expected_epoch, false, true, Moved::Refuse, |state| {
            state
                .records
                .retain(|current| !same_key(current, &record.origin, &record.account));
            state.records.push(record.clone());
            trim_records(&mut state.records);
            state.target = Some(AutoLoginTarget::from_record(record));
            state.restore_blocked = false;
            true
        })
        .map(|_| ())
    }

    /// Persist the target chosen by a successful legacy migration. The record
    /// itself is not rewritten, so a rotation that landed meanwhile is kept.
    pub fn set_target(
        &self,
        record: &RememberedSession,
        expected_epoch: u64,
    ) -> Result<(), StoreError> {
        if !valid_record(record) {
            return Err(StoreError::Invalid);
        }
        self.mutate(expected_epoch, false, true, Moved::Refuse, |state| {
            let Some(current) = state
                .records
                .iter()
                .find(|current| same_key(current, &record.origin, &record.account))
            else {
                return false;
            };
            if current.user_id != record.user_id {
                return false;
            }
            let target = AutoLoginTarget::from_record(record);
            if state.target.as_ref() == Some(&target) {
                return false;
            }
            state.target = Some(target);
            state.restore_blocked = false;
            true
        })
        .map(|_| ())
    }

    /// Drop the record for `(origin, account)` only when it still carries
    /// `token`, and the target when it names the same identity. Account
    /// comparison ignores case; the origin is exact.
    ///
    /// The token check keeps a session that just ended from deleting a newer
    /// login of the same account: another window or process that saved after
    /// this session started stored a different token, so the record and its
    /// target stay. A record that is already gone is not touched either.
    pub fn delete(
        &self,
        origin: &str,
        account: &str,
        token: &str,
        expected_epoch: u64,
    ) -> Result<(), StoreError> {
        self.mutate(expected_epoch, false, true, Moved::Refuse, |state| {
            let mut removed_user = None;
            state.records.retain(|current| {
                if same_key(current, origin, account) && current.token == token {
                    removed_user = Some(current.user_id);
                    false
                } else {
                    true
                }
            });
            // Only the target that names the removed record goes away; a newer
            // login of the same account may own the pointer now.
            let target_matches = removed_user.is_some_and(|user_id| {
                state.target.as_ref().is_some_and(|target| {
                    target.origin == origin
                        && target.user_id == user_id
                        && accounts_match(&target.account, account)
                })
            });
            if target_matches {
                state.target = None;
            }
            removed_user.is_some()
        })
        .map(|_| ())
    }

    /// A manual login without «Запомнить вход»: remove this account's record
    /// and the active target whatever account it named. The next start must not
    /// restore the previously saved user.
    pub fn forget(
        &self,
        origin: &str,
        account: &str,
        expected_epoch: u64,
    ) -> Result<(), StoreError> {
        self.mutate(expected_epoch, false, true, Moved::Refuse, |state| {
            let before = state.records.len();
            state
                .records
                .retain(|current| !same_key(current, origin, account));
            let removed = state.records.len() != before;
            let had_target = state.target.take().is_some();
            removed || had_target
        })
        .map(|_| ())
    }

    /// Remove every record and the target in one atomic write, and move the
    /// generation on disk. The new-shape empty document is the evidence that
    /// this profile was explicitly logged out; an old empty array is not.
    /// Returns the new generation.
    ///
    /// The generation is checked against the in-process barrier and the file,
    /// exactly like the other writes. A logout uses
    /// [`clear_captured`](Self::clear_captured) instead, because its own
    /// barrier is above the generation the file has. This form models the
    /// logout of a second instance in the tests.
    #[cfg(test)]
    pub(crate) fn clear_all(&self, expected_epoch: u64) -> Result<u64, StoreError> {
        self.clear(expected_epoch, true)
    }

    /// The logout cleanup: clear every record only while the file is still at
    /// `captured_epoch`, the value [`known_disk_generation`] reported at the
    /// logout.
    ///
    /// The in-memory barrier that the logout itself raised is above that value
    /// and is deliberately not part of the check: the barrier is what a queued
    /// save must fail, while this clear must be allowed to run at the
    /// generation the file really has. The file check is what keeps a login
    /// that another window saved in between.
    ///
    /// A file moved past `captured_epoch` without a newer record is only the
    /// claim of a login that was then refused, so the records are the ones this
    /// cleanup captured. They are cleared at the generation the claim reached,
    /// which also lets a save still waiting for that claim land afterwards.
    pub fn clear_captured(&self, captured_epoch: u64) -> Result<u64, StoreError> {
        self.clear(captured_epoch, false)
    }

    fn clear(&self, expected_epoch: u64, check_barrier: bool) -> Result<u64, StoreError> {
        self.mutate(
            expected_epoch,
            true,
            check_barrier,
            Moved::ClaimOnly,
            |state| {
                state.records.clear();
                state.target = None;
                state.restore_blocked = false;
                true
            },
        )?;
        Ok(self.epoch())
    }

    /// Store a persistent «do not restore» marker while the records themselves
    /// could not be removed. Only the tests use the barrier-checked form; the
    /// application always binds the marker to the captured logout generation.
    #[cfg(test)]
    pub(crate) fn block_restore(&self, expected_epoch: u64) -> Result<(), StoreError> {
        self.block(expected_epoch, true)
    }

    /// The best-effort second step of a failed logout cleanup, bound to the
    /// same captured generation as [`clear_captured`].
    pub fn block_restore_captured(&self, captured_epoch: u64) -> Result<(), StoreError> {
        self.block(captured_epoch, false)
    }

    fn block(&self, expected_epoch: u64, check_barrier: bool) -> Result<(), StoreError> {
        self.mutate(
            expected_epoch,
            false,
            check_barrier,
            Moved::Refuse,
            |state| {
                if state.restore_blocked {
                    return false;
                }
                state.restore_blocked = true;
                true
            },
        )
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
        self.mutate(expected_epoch, false, true, Moved::Refuse, |state| {
            let Some(record) = state
                .records
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

    fn read_state_synced(&self) -> Result<StoreState, StoreError> {
        let Some(path) = &self.path else {
            return Ok(StoreState::default());
        };
        let _local = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = read_state(path)?;
        self.sync_epoch(state.generation);
        Ok(state)
    }

    /// Serialize one read-modify-write under both locks. The generation is
    /// checked only after the locks are held, so a logout that bumped it while
    /// this write waited for the file cannot be overwritten by a late save.
    /// The closure returns whether it changed anything; the file is rewritten
    /// only when it did. `new_generation` moves the on-disk generation on even
    /// when the caller did not change the local one (logout cleanup).
    /// `check_barrier` is false only for the logout cleanup, which runs at the
    /// generation the file had at the logout and therefore below the barrier
    /// that logout raised; the file check still protects a newer login.
    /// `moved` decides what a file above `expected_epoch` means; see [`Moved`].
    fn mutate(
        &self,
        expected_epoch: u64,
        new_generation: bool,
        check_barrier: bool,
        moved: Moved,
        change: impl FnOnce(&mut StoreState) -> bool,
    ) -> Result<bool, StoreError> {
        let Some(path) = &self.path else {
            return Ok(false);
        };
        let _local = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _named = NamedLock::acquire()?;
        if check_barrier && self.epoch() > expected_epoch {
            return Err(StoreError::Stale);
        }
        let mut state = match read_state(path) {
            Ok(state) => state,
            // A manual save or a logout may replace a damaged file. A failed
            // read must never turn into a panic or a secret in a message.
            Err(StoreError::Corrupt) => StoreState::default(),
            Err(error) => return Err(error),
        };
        let disk = state.generation;
        let moved_on = expected_epoch < disk;
        if moved_on && !(moved == Moved::ClaimOnly && state.records_generation <= expected_epoch) {
            // Another process or window has moved the store on with a record of
            // its own. The next write must start from its generation.
            self.sync_epoch(disk);
            return Err(StoreError::Stale);
        }
        if !change(&mut state) {
            // Nothing was written, so only the observed disk generation is
            // recorded. `expected_epoch` may be the in-memory barrier, which is
            // not on the file and must not be captured by a later cleanup.
            self.sync_epoch(disk);
            return Ok(false);
        }
        let next = if moved_on {
            // Only a claim that saved nothing moved the file here. Writing at
            // its generation lets a save still waiting for that claim land.
            disk
        } else {
            disk.max(expected_epoch)
                .saturating_add(u64::from(new_generation))
        };
        #[cfg(test)]
        self.run_write_hook();
        state.generation = next;
        state.records_generation = next;
        write_state_file(path, &state)?;
        // Read before `sync_epoch`: this write may itself move the in-process
        // generation above `expected_epoch` without any logout.
        let moved = check_barrier && self.epoch() > expected_epoch;
        self.sync_epoch(next);
        if moved {
            // The logout won while the file was replaced. This content belongs
            // to the session that ended, so it is marked with the generation the
            // file had before: the cleanup of that logout captured at least that
            // value and clears these records instead of keeping the session.
            state.records_generation = disk;
            let _ = write_state_file(path, &state);
            return Err(StoreError::Stale);
        }
        Ok(true)
    }

    /// Record a generation that a read, a claim or a write saw on the file.
    /// Every caller passes a value that exists on disk; the in-memory barrier
    /// goes through `bump_epoch` instead.
    fn sync_epoch(&self, generation: u64) {
        self.epoch.fetch_max(generation, Ordering::SeqCst);
        self.disk.fetch_max(generation, Ordering::SeqCst);
    }

    /// Test helper: whether another thread currently holds this store's
    /// in-process mutex, i.e. an operation of this store is waiting for the
    /// file and a second one would wait behind it.
    #[cfg(test)]
    pub(crate) fn local_lock_is_held(&self) -> bool {
        self.lock.try_lock().is_err()
    }

    /// Test helper: run `hook` inside every write after the last barrier check
    /// and before the file is replaced. That is where the UI thread of the same
    /// process raises the logout barrier, so a test can fix that order instead
    /// of racing for it.
    #[cfg(test)]
    pub(crate) fn set_write_hook(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self
            .write_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(hook));
    }

    #[cfg(test)]
    fn run_write_hook(&self) {
        let hook = self
            .write_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook();
        }
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

fn valid_target(target: &AutoLoginTarget) -> bool {
    target.user_id > 0
        && !target.account.trim().is_empty()
        && canonical_origin(&target.origin).is_some_and(|origin| origin == target.origin)
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

fn read_state(path: &Path) -> Result<StoreState, StoreError> {
    // A file used as a directory is an unavailable store, not a missing one.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && parent.exists() && !parent.is_dir() {
            return Err(StoreError::Io);
        }
    }
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_FILE_BYTES => return Err(StoreError::Corrupt),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StoreState::default())
        }
        Err(_) => return Err(StoreError::Io),
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StoreState::default())
        }
        Err(_) => return Err(StoreError::Io),
    };
    if bytes.len() <= MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Err(StoreError::Corrupt);
    }
    let plain = unprotect(&bytes[MAGIC.len()..])?;
    let value: serde_json::Value =
        serde_json::from_slice(&plain).map_err(|_| StoreError::Corrupt)?;
    let mut state = if value.is_array() {
        // The old format: a bare list and no pointer.
        StoreState {
            records: serde_json::from_value(value).map_err(|_| StoreError::Corrupt)?,
            legacy: true,
            ..StoreState::default()
        }
    } else if value.is_object() {
        let document: Document = serde_json::from_value(value).map_err(|_| StoreError::Corrupt)?;
        if document.version != FORMAT_VERSION {
            return Err(StoreError::Corrupt);
        }
        StoreState {
            records: document.records,
            target: document.target,
            legacy: false,
            generation: document.generation,
            records_generation: document
                .records_generation
                .unwrap_or(document.generation)
                .min(document.generation),
            restore_blocked: document.restore_blocked,
        }
    } else {
        return Err(StoreError::Corrupt);
    };
    // A single damaged record must not take the whole list down with it.
    state.records.retain(valid_record);
    trim_records(&mut state.records);
    if state
        .target
        .as_ref()
        .is_some_and(|target| !valid_target(target))
    {
        state.target = None;
    }
    Ok(state)
}

fn write_state_file(path: &Path, state: &StoreState) -> Result<(), StoreError> {
    let document = Document {
        version: FORMAT_VERSION,
        generation: state.generation,
        records_generation: Some(state.records_generation),
        target: state.target.clone(),
        restore_blocked: state.restore_blocked,
        records: state.records.clone(),
    };
    let json = Zeroizing::new(serde_json::to_vec(&document).map_err(|_| StoreError::Corrupt)?);
    let blob = protect(&json)?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + blob.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&blob);
    crate::instance_prefs::write_atomic(path, &bytes).map_err(|_| StoreError::Io)
}

/// Test helper: write the old bare-array document so the migration tests use
/// the exact bytes a previous build produced.
#[cfg(test)]
pub(crate) fn write_legacy_file(path: &Path, records: &[RememberedSession]) {
    let json = Zeroizing::new(serde_json::to_vec(records).expect("legacy json"));
    let blob = protect(&json).expect("protect");
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&blob);
    std::fs::write(path, bytes).expect("write legacy");
}

/// Test helper: write an arbitrary new-shape document (for «target absent» and
/// cleanup-marker cases).
#[cfg(test)]
pub(crate) fn write_state_file_for_test(path: &Path, state: &StoreState) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("dir");
    write_state_file(path, state).expect("write state");
}

/// Test helper: hold the cross-process mutex on another thread for `duration`
/// and signal when it is held. A store operation of this process then waits on
/// it exactly like behind a second process.
#[cfg(all(test, windows))]
pub(crate) fn hold_named_lock_for(
    duration: std::time::Duration,
) -> (std::sync::mpsc::Receiver<()>, std::thread::JoinHandle<()>) {
    let (ready, acquired) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _lock = NamedLock::acquire().expect("named session lock");
        let _ = ready.send(());
        std::thread::sleep(duration);
    });
    (acquired, handle)
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
