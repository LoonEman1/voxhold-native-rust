//! The store on its own: DPAPI round trip, damage, exact origin, rotation,
//! concurrency, the generation barrier that a logout uses, and the limits that
//! keep a damaged or hostile file from loading.

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

fn save_now(store: &SessionStore, record: &RememberedSession) -> Result<(), StoreError> {
    store.save(record, store.epoch())
}

fn delete_now(store: &SessionStore, origin: &str, account: &str) -> Result<(), StoreError> {
    store.delete(origin, account, store.epoch())
}

fn load(store: &SessionStore, origin: &str) -> Result<Option<RememberedSession>, StoreError> {
    store.load_for_origin(origin, crate::auth_session::now_unix())
}

fn write_protected(path: &Path, json: &[u8]) {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&protect(json).unwrap());
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_record_round_trips_through_dpapi_without_plaintext() {
    let (dir, path) = scratch("roundtrip");
    let store = SessionStore::new(Some(path.clone()));
    let saved = record("https://chat.example.org:8443", "neva", "token-secret");
    save_now(&store, &saved).unwrap();

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
        load(&store, "https://chat.example.org:8443").unwrap(),
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
    save_now(&store, &saved).unwrap();
    assert_eq!(store.load_all().unwrap(), vec![saved]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_valid_blob_with_broken_json_is_corrupt_without_a_panic() {
    let (dir, path) = scratch("json");
    write_protected(&path, b"not json");
    let store = SessionStore::new(Some(path));
    assert_eq!(store.load_all(), Err(StoreError::Corrupt));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_oversized_file_is_refused_before_it_is_decrypted() {
    let (dir, path) = scratch("oversized");
    let mut bytes = MAGIC.to_vec();
    bytes.resize(MAGIC.len() + MAX_FILE_BYTES as usize + 1, 0);
    std::fs::write(&path, bytes).unwrap();
    let store = SessionStore::new(Some(path.clone()));
    assert_eq!(store.load_all(), Err(StoreError::Corrupt));
    // The repair path still works: a checked save replaces the huge file.
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();
    assert_eq!(store.load_all().unwrap().len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_records_are_dropped_instead_of_loaded() {
    let (dir, path) = scratch("invalid");
    let valid = record("https://chat.example.org", "neva", "token-ok");
    let json = serde_json::json!([
        valid,
        {
            "origin": "https://chat.example.org",
            "account": "neva",
            "user_id": 7,
            "token": "",
            "expires_at": valid.expires_at,
            "saved_at": valid.saved_at
        },
        {
            "origin": "https://chat.example.org",
            "account": "neva",
            "user_id": -1,
            "token": "token-bad-id",
            "expires_at": valid.expires_at,
            "saved_at": valid.saved_at
        },
        {
            "origin": "https://chat.example.org",
            "account": "  ",
            "user_id": 7,
            "token": "token-bad-account",
            "expires_at": valid.expires_at,
            "saved_at": valid.saved_at
        },
        {
            "origin": "https://chat.example.org",
            "account": "neva",
            "user_id": 7,
            "token": "token-bad-expiry",
            "expires_at": 0,
            "saved_at": valid.saved_at
        },
        {
            "origin": "https://chat.example.org/",
            "account": "neva",
            "user_id": 7,
            "token": "token-bad-origin",
            "expires_at": valid.expires_at,
            "saved_at": valid.saved_at
        }
    ]);
    write_protected(&path, &serde_json::to_vec(&json).unwrap());

    let store = SessionStore::new(Some(path));
    let loaded = store.load_all().unwrap();
    assert_eq!(loaded.len(), 1, "{loaded:?}");
    assert_eq!(loaded[0].token, "token-ok");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_list_keeps_the_sixteen_newest_records() {
    let (dir, path) = scratch("limit");
    let mut records = Vec::new();
    for index in 0..20 {
        let mut saved = record(
            "https://chat.example.org",
            &format!("user-{index}"),
            &format!("token-{index}"),
        );
        saved.saved_at = 1_000 + index;
        records.push(saved);
    }
    write_protected(&path, &serde_json::to_vec(&records).unwrap());

    let store = SessionStore::new(Some(path));
    let loaded = store.load_all().unwrap();
    assert_eq!(loaded.len(), MAX_RECORDS);
    assert!(loaded.iter().all(|record| record.saved_at >= 1_004));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_origin_including_the_port_must_match_exactly() {
    let (dir, path) = scratch("origin");
    let store = SessionStore::new(Some(path));
    let first = record("https://chat.example.org:8443", "neva", "token-a");
    let second = record("https://chat.example.org:9443", "neva", "token-b");
    save_now(&store, &first).unwrap();
    save_now(&store, &second).unwrap();

    assert_eq!(
        load(&store, "https://chat.example.org:8443").unwrap(),
        Some(first)
    );
    assert_eq!(
        load(&store, "https://chat.example.org:9443").unwrap(),
        Some(second)
    );
    assert!(load(&store, "https://chat.example.org").unwrap().is_none());
    assert!(load(&store, "http://chat.example.org:8443")
        .unwrap()
        .is_none());
    assert_eq!(store.load_all().unwrap().len(), 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_newest_expired_record_does_not_hide_a_valid_one() {
    let (dir, path) = scratch("expired-newest");
    let store = SessionStore::new(Some(path));
    let mut valid = record("https://chat.example.org", "neva", "token-valid");
    valid.expires_at = 1_800;
    valid.saved_at = 1_000;
    let mut expired = record("https://chat.example.org", "bob", "token-expired");
    expired.expires_at = 1_500;
    expired.saved_at = 2_000;
    save_now(&store, &valid).unwrap();
    save_now(&store, &expired).unwrap();

    // `now` is past the newer record but before the older one expires.
    let loaded = store
        .load_for_origin("https://chat.example.org", 1_600)
        .unwrap()
        .expect("the valid record");
    assert_eq!(loaded.token, "token-valid");
    // Once every record is expired, the newest stale one comes back so the
    // caller can delete it.
    let stale = store
        .load_for_origin("https://chat.example.org", 5_000)
        .unwrap()
        .expect("the newest stale record");
    assert_eq!(stale.token, "token-expired");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_account_ignores_case_and_save_replaces_the_same_key() {
    let (dir, path) = scratch("account");
    let store = SessionStore::new(Some(path));
    save_now(
        &store,
        &record("https://chat.example.org", "Neva", "token-1"),
    )
    .unwrap();
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-2"),
    )
    .unwrap();
    let all = store.load_all().unwrap();
    assert_eq!(all.len(), 1, "one record per origin and account");
    assert_eq!(all[0].token, "token-2");

    delete_now(&store, "https://chat.example.org", "NEVA").unwrap();
    assert!(store.load_all().unwrap().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unicode_accounts_compare_case_insensitively() {
    assert!(accounts_match("Нева", "нева"));
    assert!(accounts_match("  НЕВА  ", "нева"));
    assert!(!accounts_match("Нева", "нево"));

    let (dir, path) = scratch("unicode");
    let store = SessionStore::new(Some(path));
    save_now(
        &store,
        &record("https://chat.example.org", "Нева", "token-1"),
    )
    .unwrap();
    save_now(
        &store,
        &record("https://chat.example.org", "нева", "token-2"),
    )
    .unwrap();
    let all = store.load_all().unwrap();
    assert_eq!(all.len(), 1, "one record per account regardless of case");
    assert_eq!(all[0].token, "token-2");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn expiry_is_visible_so_the_caller_can_delete_the_record() {
    let (dir, path) = scratch("expired");
    let store = SessionStore::new(Some(path));
    let mut saved = record("https://chat.example.org", "neva", "token-1");
    saved.expires_at = 100;
    save_now(&store, &saved).unwrap();
    let loaded = store.load_all().unwrap();
    assert!(loaded[0].is_expired(101));
    assert!(!loaded[0].is_expired(99));
    delete_now(&store, &saved.origin, &saved.account).unwrap();
    assert!(store.load_all().unwrap().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rotation_only_replaces_an_accepted_token_and_the_previous_one_can_catch_up() {
    let (dir, path) = scratch("rotation");
    let store = SessionStore::new(Some(path));
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();

    assert!(!store
        .update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-9"],
            "token-2",
            5_000,
            store.epoch(),
        )
        .unwrap());
    assert_eq!(store.load_all().unwrap()[0].token, "token-1");

    assert!(store
        .update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-1"],
            "token-2",
            5_000,
            store.epoch(),
        )
        .unwrap());
    let saved = store.load_all().unwrap();
    assert_eq!(saved[0].token, "token-2");
    assert_eq!(saved[0].expires_at, 5_000);

    // The first update was lost: the next rotation still knows the token
    // that was actually stored and catches up with it.
    assert!(store
        .update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-3", "token-2"],
            "token-4",
            6_000,
            store.epoch(),
        )
        .unwrap());
    assert_eq!(store.load_all().unwrap()[0].token, "token-4");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rotation_never_touches_another_origin_or_account() {
    let (dir, path) = scratch("rotation-other");
    let store = SessionStore::new(Some(path));
    save_now(
        &store,
        &record("https://chat.example.org:8443", "neva", "token-1"),
    )
    .unwrap();
    assert!(!store
        .update_after_refresh(
            "https://chat.example.org:9443",
            "neva",
            &["token-1"],
            "token-2",
            5_000,
            store.epoch(),
        )
        .unwrap());
    assert!(!store
        .update_after_refresh(
            "https://chat.example.org:8443",
            "bob",
            &["token-1"],
            "token-2",
            5_000,
            store.epoch(),
        )
        .unwrap());
    assert_eq!(store.load_all().unwrap()[0].token, "token-1");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_late_save_after_a_logout_does_not_resurrect_the_record() {
    let (dir, path) = scratch("barrier-save");
    let store = SessionStore::new(Some(path));
    let saved = record("https://chat.example.org", "neva", "token-1");
    save_now(&store, &saved).unwrap();
    // The login scheduled this write; then the user logged out before it ran.
    let old_epoch = store.epoch();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let late = store.clone();
        let late_record = saved.clone();
        let start = &barrier;
        scope.spawn(move || {
            start.wait();
            let _ = late.save(&late_record, old_epoch);
        });
        let out = store.clone();
        scope.spawn(move || {
            start.wait();
            let epoch = out.bump_epoch();
            out.delete(&saved.origin, &saved.account, epoch).unwrap();
        });
    });
    assert!(
        store.load_all().unwrap().is_empty(),
        "a write from before the logout must not revive the session"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_late_delete_does_not_remove_a_new_record() {
    let (dir, path) = scratch("barrier-delete");
    let store = SessionStore::new(Some(path));
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();
    let old_epoch = store.epoch();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let late = store.clone();
        let start = &barrier;
        scope.spawn(move || {
            start.wait();
            let _ = late.delete("https://chat.example.org", "neva", old_epoch);
        });
        let fresh = store.clone();
        scope.spawn(move || {
            start.wait();
            let epoch = fresh.bump_epoch();
            fresh
                .save(
                    &record("https://chat.example.org", "neva", "token-2"),
                    epoch,
                )
                .unwrap();
        });
    });
    let all = store.load_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].token, "token-2", "the new login survives");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_late_save_does_not_overwrite_a_new_record() {
    let (dir, path) = scratch("barrier-overwrite");
    let store = SessionStore::new(Some(path));
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();
    let old_epoch = store.epoch();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let late = store.clone();
        let start = &barrier;
        scope.spawn(move || {
            start.wait();
            let _ = late.save(
                &record("https://chat.example.org", "neva", "token-old"),
                old_epoch,
            );
        });
        let fresh = store.clone();
        scope.spawn(move || {
            start.wait();
            let epoch = fresh.bump_epoch();
            fresh
                .save(
                    &record("https://chat.example.org", "neva", "token-new"),
                    epoch,
                )
                .unwrap();
        });
    });
    assert_eq!(store.load_all().unwrap()[0].token, "token-new");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_stale_generation_is_an_explicit_error() {
    let (dir, path) = scratch("stale");
    let store = SessionStore::new(Some(path));
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();
    let old_epoch = store.epoch();
    store.bump_epoch();

    assert_eq!(
        store.save(
            &record("https://chat.example.org", "neva", "token-2"),
            old_epoch
        ),
        Err(StoreError::Stale)
    );
    assert_eq!(
        store.delete("https://chat.example.org", "neva", old_epoch),
        Err(StoreError::Stale)
    );
    assert_eq!(
        store.update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-1"],
            "token-2",
            5_000,
            old_epoch,
        ),
        Err(StoreError::Stale)
    );
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
                // Sixteen iterations keep eight records per worker, exactly the
                // file cap, so a lost cross-thread record would show as eight.
                for index in 0..16 {
                    let account = format!("user-{worker}-{index}");
                    store
                        .save(
                            &record(
                                "https://chat.example.org",
                                &account,
                                &format!("token-{worker}-{index}"),
                            ),
                            store.epoch(),
                        )
                        .unwrap();
                    if index % 2 == 0 {
                        store
                            .delete("https://chat.example.org", &account, store.epoch())
                            .unwrap();
                    }
                }
            });
        }
    });
    // Each worker leaves eight records; the other thread's records survive.
    assert_eq!(store.load_all().unwrap().len(), 16);
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
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    let guard = ReadWrite(path.clone());

    let result = save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-2"),
    );

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
    assert!(save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1")
    )
    .is_err());
    assert_eq!(store.load_all(), Err(StoreError::Io));
    std::fs::remove_dir_all(dir).unwrap();
    let _ = path;
}

#[test]
fn a_disabled_store_is_a_quiet_no_op() {
    let store = SessionStore::new(None);
    assert!(store.load_all().unwrap().is_empty());
    assert!(save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1")
    )
    .is_ok());
    assert!(!store
        .update_after_refresh(
            "https://chat.example.org",
            "neva",
            &["token-1"],
            "token-2",
            5_000,
            store.epoch(),
        )
        .unwrap());
}

#[test]
fn an_unchecked_login_clears_this_account_and_the_active_target() {
    let (dir, path) = scratch("forget");
    let store = SessionStore::new(Some(path));
    let first = record("https://chat.example.org", "neva", "token-1");
    let second = record("https://chat.example.org", "bob", "token-2");
    save_now(&store, &first).unwrap();
    save_now(&store, &second).unwrap();
    assert_eq!(
        store.state().unwrap().target.map(|target| target.account),
        Some("bob".into())
    );

    // bob signs in without the box: his record and the active target go, while
    // neva's record stays for a later explicit choice.
    store
        .forget(&second.origin, &second.account, store.epoch())
        .unwrap();
    let state = store.state().unwrap();
    assert_eq!(state.records, vec![first]);
    assert!(state.target.is_none());
    assert_eq!(
        store
            .plan_auto_login(crate::auth_session::now_unix(), None, true)
            .unwrap(),
        AutoLogin::Manual,
        "the new format never searches records after an explicit forget"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn debug_output_never_prints_the_token() {
    let saved = record("https://chat.example.org", "neva", "token-secret");
    let debug = format!("{saved:?}");
    assert!(debug.contains("<redacted>"), "{debug}");
    assert!(!debug.contains("token-secret"), "{debug}");
    assert!(debug.contains("neva"), "{debug}");
}

mod target_tests;
