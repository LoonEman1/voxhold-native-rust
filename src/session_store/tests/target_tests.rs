//! Target selection, the new-format document, the legacy migration and the
//! cleanup generation. These tests share the parent module's `scratch`,
//! `record`, `save_now` and `write_protected` helpers.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Wait for a phase of the claim/logout handshake. The two run on different
/// threads, so the wait is bounded by time instead of by a spin count.
fn wait_phase(phase: &AtomicU64, value: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while phase.load(Ordering::SeqCst) != value {
        assert!(
            std::time::Instant::now() < deadline,
            "the other side never reached phase {value}"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn a_saved_login_persists_one_target_and_the_new_format() {
    let (dir, path) = scratch("format-v2");
    let store = SessionStore::new(Some(path.clone()));
    let saved = record("https://chat.example.org:8443", "neva", "token-1");
    save_now(&store, &saved).unwrap();

    let state = store.state().unwrap();
    assert!(!state.legacy, "a save writes the versioned document");
    assert_eq!(state.records, vec![saved.clone()]);
    let target = state.target.expect("the one target");
    assert_eq!(target.origin, saved.origin);
    assert_eq!(target.user_id, saved.user_id);
    assert_eq!(target.account, saved.account);
    assert!(!state.restore_blocked);

    // A second login replaces the target instead of adding a second one.
    let second = record("https://chat.example.org:9443", "bob", "token-2");
    save_now(&store, &second).unwrap();
    let target = store.state().unwrap().target.expect("target");
    assert_eq!(target.origin, second.origin);
    assert_eq!(target.account, "bob");

    // The protected file is still not readable and keeps the magic.
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..MAGIC.len()], MAGIC);
    assert!(!bytes.windows(6).any(|window| window == b"token-"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_clear_writes_an_empty_new_format_with_a_new_generation() {
    let (dir, path) = scratch("clear-format");
    let store = SessionStore::new(Some(path.clone()));
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();
    let before = store.epoch();
    let after = store.clear_all(before).unwrap();
    assert_eq!(after, before.saturating_add(1));
    assert_eq!(store.epoch(), after);

    let state = store.state().unwrap();
    assert!(
        !state.legacy,
        "an empty store must not look like the old format"
    );
    assert!(state.records.is_empty());
    assert!(state.target.is_none());
    assert_eq!(state.generation, after);
    assert!(path.exists(), "the cleared generation stays on disk");

    // A late delete of the old generation cannot touch the cleared file.
    assert_eq!(
        store.delete("https://chat.example.org", "neva", "token-1", before),
        Err(StoreError::Stale)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_new_format_without_a_target_never_searches_old_records() {
    let (dir, path) = scratch("no-target");
    let saved = record("https://chat.example.org", "neva", "token-1");
    write_state_file_for_test(
        &path,
        &StoreState {
            records: vec![saved.clone()],
            target: None,
            legacy: false,
            generation: 3,
            records_generation: 3,
            restore_blocked: false,
        },
    );
    let store = SessionStore::new(Some(path));
    let now = crate::auth_session::now_unix();
    assert_eq!(
        store.plan_auto_login(now, None, true).unwrap(),
        AutoLogin::Manual
    );
    assert_eq!(
        store
            .plan_auto_login(now, Some("https://chat.example.org"), true)
            .unwrap(),
        AutoLogin::Manual
    );
    // The record itself is still available for a later explicit choice.
    assert_eq!(store.load_all().unwrap(), vec![saved]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_legacy_file_migrates_only_an_exact_or_single_choice() {
    let (dir, path) = scratch("legacy-migrate");
    let first = record("https://chat.example.org:8443", "neva", "token-1");
    let second = record("https://chat.example.org:9443", "neva", "token-2");
    write_legacy_file(&path, &[first.clone(), second.clone()]);
    let store = SessionStore::new(Some(path.clone()));
    let now = crate::auth_session::now_unix();

    // The confirmed address names exactly one record.
    assert_eq!(
        store
            .plan_auto_login(now, Some("https://chat.example.org:8443"), true)
            .unwrap(),
        AutoLogin::Ready {
            record: first.clone(),
            legacy: true
        }
    );
    // Without an address the profile has two suitable records.
    assert_eq!(
        store.plan_auto_login(now, None, true).unwrap(),
        AutoLogin::Ambiguous
    );
    // With the target persisted the same choice no longer depends on the old
    // record search and is reported as the new format.
    store.set_target(&first, store.epoch()).unwrap();
    assert_eq!(
        store
            .plan_auto_login(now, Some("https://chat.example.org:8443"), false)
            .unwrap(),
        AutoLogin::Ready {
            record: first.clone(),
            legacy: false
        }
    );
    let state = store.state().unwrap();
    assert!(!state.legacy);
    assert_eq!(
        state.target.as_ref().map(|target| target.origin.clone()),
        Some(first.origin.clone())
    );
    // The other record is kept for a manual choice.
    assert_eq!(state.records.len(), 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_legacy_file_with_one_record_uses_the_profile_wide_fallback_only_when_allowed() {
    let (dir, path) = scratch("legacy-single");
    let saved = record("https://chat.example.org", "neva", "token-1");
    write_legacy_file(&path, &[saved.clone()]);
    let store = SessionStore::new(Some(path));
    let now = crate::auth_session::now_unix();
    assert_eq!(
        store.plan_auto_login(now, None, true).unwrap(),
        AutoLogin::Ready {
            record: saved.clone(),
            legacy: true
        }
    );
    // An explicit `-server` target is the only allowed origin; the fallback is
    // disabled and a different origin is manual.
    assert_eq!(
        store.plan_auto_login(now, None, false).unwrap(),
        AutoLogin::Manual
    );
    assert_eq!(
        store
            .plan_auto_login(now, Some("https://other.example.org"), false)
            .unwrap(),
        AutoLogin::Manual
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_expired_legacy_record_is_not_migrated() {
    let (dir, path) = scratch("legacy-expired");
    let mut saved = record("https://chat.example.org", "neva", "token-1");
    saved.expires_at = 100;
    write_legacy_file(&path, &[saved]);
    let store = SessionStore::new(Some(path));
    assert_eq!(
        store.plan_auto_login(5_000, None, true).unwrap(),
        AutoLogin::Manual
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_explicit_origin_must_match_the_target_exactly() {
    let (dir, path) = scratch("target-origin");
    let store = SessionStore::new(Some(path));
    save_now(
        &store,
        &record("https://chat.example.org:8443", "neva", "token-1"),
    )
    .unwrap();
    let now = crate::auth_session::now_unix();
    assert!(matches!(
        store
            .plan_auto_login(now, Some("https://chat.example.org:8443"), false)
            .unwrap(),
        AutoLogin::Ready { .. }
    ));
    for origin in [
        "https://chat.example.org:9443",
        "http://chat.example.org:8443",
        "https://chat.example.org",
    ] {
        assert_eq!(
            store.plan_auto_login(now, Some(origin), true).unwrap(),
            AutoLogin::Manual,
            "{origin}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_target_must_match_the_user_and_account_of_the_record() {
    let (dir, path) = scratch("target-identity");
    let saved = record("https://chat.example.org", "neva", "token-1");
    write_state_file_for_test(
        &path,
        &StoreState {
            records: vec![saved.clone()],
            target: Some(AutoLoginTarget {
                origin: saved.origin.clone(),
                user_id: saved.user_id + 1,
                account: saved.account.clone(),
            }),
            legacy: false,
            generation: 1,
            records_generation: 1,
            restore_blocked: false,
        },
    );
    let store = SessionStore::new(Some(path.clone()));
    assert_eq!(
        store
            .plan_auto_login(crate::auth_session::now_unix(), None, true)
            .unwrap(),
        AutoLogin::Manual
    );

    // A case difference in the account is the same identity.
    write_state_file_for_test(
        &path,
        &StoreState {
            records: vec![saved.clone()],
            target: Some(AutoLoginTarget {
                origin: saved.origin.clone(),
                user_id: saved.user_id,
                account: "NEVA".into(),
            }),
            legacy: false,
            generation: 1,
            records_generation: 1,
            restore_blocked: false,
        },
    );
    assert!(matches!(
        store
            .plan_auto_login(crate::auth_session::now_unix(), None, true)
            .unwrap(),
        AutoLogin::Ready { .. }
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_second_store_cannot_revive_or_delete_after_a_cleanup() {
    let (dir, path) = scratch("second-process");
    let first = record("https://chat.example.org", "neva", "token-1");
    let second = record("https://chat.example.org", "bob", "token-2");

    let writer = SessionStore::new(Some(path.clone()));
    save_now(&writer, &first).unwrap();
    let old_epoch = writer.epoch();

    // A second process sees the same file and logs out.
    let other = SessionStore::new(Some(path.clone()));
    let new_epoch = other.clear_all(other.epoch()).unwrap();
    assert!(new_epoch > old_epoch);

    // The queued save, rotation and delete of the old generation are refused
    // and the new empty generation stays empty.
    assert_eq!(writer.save(&first, old_epoch), Err(StoreError::Stale));
    assert_eq!(
        writer.delete(&first.origin, &first.account, &first.token, old_epoch),
        Err(StoreError::Stale)
    );
    assert_eq!(
        writer.update_after_refresh(
            &first.origin,
            &first.account,
            &["token-1"],
            "token-late",
            9_000,
            old_epoch,
        ),
        Err(StoreError::Stale)
    );
    assert!(other.load_all().unwrap().is_empty());

    // A new login in the second process creates the new generation; the old
    // writer still cannot erase it.
    other.save(&second, other.begin_login()).unwrap();
    assert_eq!(
        writer.delete(&second.origin, &second.account, &second.token, old_epoch),
        Err(StoreError::Stale)
    );
    assert_eq!(other.load_all().unwrap(), vec![second]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_restore_block_marker_survives_until_a_login_or_cleanup() {
    let (dir, path) = scratch("blocked");
    let store = SessionStore::new(Some(path));
    let saved = record("https://chat.example.org", "neva", "token-1");
    save_now(&store, &saved).unwrap();
    store.block_restore(store.epoch()).unwrap();
    assert_eq!(
        store
            .plan_auto_login(crate::auth_session::now_unix(), None, true)
            .unwrap(),
        AutoLogin::Manual,
        "the marker suppresses the automatic login"
    );
    assert_eq!(store.load_all().unwrap(), vec![saved.clone()]);

    // A new manual login lifts the marker.
    store.save(&saved, store.epoch()).unwrap();
    assert!(!store.state().unwrap().restore_blocked);
    assert!(matches!(
        store
            .plan_auto_login(crate::auth_session::now_unix(), None, true)
            .unwrap(),
        AutoLogin::Ready { .. }
    ));

    store.block_restore(store.epoch()).unwrap();
    store.clear_all(store.epoch()).unwrap();
    assert!(!store.state().unwrap().restore_blocked);
    assert_eq!(
        store
            .plan_auto_login(crate::auth_session::now_unix(), None, true)
            .unwrap(),
        AutoLogin::Manual
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_login_starts_above_the_generation_on_disk() {
    let (dir, path) = scratch("begin-login");
    let first = SessionStore::new(Some(path.clone()));
    save_now(
        &first,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();

    let second = SessionStore::new(Some(path));
    let before = second.state().unwrap().generation;
    let generation = second.begin_login();
    assert!(
        generation > before,
        "the claim is above the file it started from"
    );
    assert_eq!(
        second.state().unwrap().generation,
        generation,
        "the claim is persisted under the lock"
    );
    assert!(generation > 0);
    assert_eq!(second.epoch(), generation);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Two processes that start a login from one file must not receive the same
/// generation, and the delete of the older session must keep the record the
/// newer one saved — neither through the generation nor through the token.
#[test]
fn two_processes_claim_distinct_generations_and_keep_the_newer_login() {
    let (dir, path) = scratch("two-claims");
    let first = SessionStore::new(Some(path.clone()));
    let old = record("https://chat.example.org", "neva", "token-old");
    save_now(&first, &old).unwrap();

    // Both processes open the same file and start a login before either writes.
    let a = SessionStore::new(Some(path.clone()));
    let b = SessionStore::new(Some(path.clone()));
    let claim_a = a.begin_login();
    let claim_b = b.begin_login();
    assert!(
        claim_b > claim_a,
        "the second claim is strictly newer: {claim_a} vs {claim_b}"
    );
    assert_eq!(
        b.state().unwrap().generation,
        claim_b,
        "the claim is on disk before the login writes"
    );

    // B signs in; then the session of A ends and tries to delete its record.
    let fresh = record("https://chat.example.org", "neva", "token-fresh");
    b.save(&fresh, claim_b).unwrap();
    assert_eq!(
        a.delete(&old.origin, &old.account, &old.token, claim_a),
        Err(StoreError::Stale),
        "a delete of the old generation never reaches the newer record"
    );
    assert_eq!(
        a.clear_all(claim_a),
        Err(StoreError::Stale),
        "a cleanup of the old generation cannot erase the newer login"
    );

    // Even at the current generation a delete is scoped to the session token.
    let current = a.state().unwrap().generation;
    a.delete(&fresh.origin, &fresh.account, &old.token, current)
        .unwrap();
    assert_eq!(
        a.load_all().unwrap(),
        vec![fresh.clone()],
        "the record of the newer login stays"
    );
    assert_eq!(
        a.state().unwrap().target,
        Some(AutoLoginTarget::from_record(&fresh))
    );

    // A clear is bound to the generation its caller captured: a login saved
    // above that value afterwards survives, exactly like the logout cleanup
    // that never claims a generation of its own.
    let cleanup = b.begin_login();
    let later = record("https://chat.example.org", "bob", "token-later");
    let later_claim = a.begin_login();
    a.save(&later, later_claim).unwrap();
    assert_eq!(
        b.clear_all(cleanup),
        Err(StoreError::Stale),
        "the cleanup cannot erase a login saved after its claim"
    );
    assert_eq!(a.load_all().unwrap(), vec![fresh.clone(), later.clone()]);

    // The ended session still removes only its own record; the target now
    // names the other account and stays.
    let current = a.state().unwrap().generation;
    a.delete(&fresh.origin, &fresh.account, &fresh.token, current)
        .unwrap();
    assert_eq!(a.load_all().unwrap(), vec![later.clone()]);
    assert_eq!(
        a.state().unwrap().target,
        Some(AutoLoginTarget::from_record(&later))
    );

    // The record of the session that ended is removed when its token matches.
    let current = a.state().unwrap().generation;
    a.delete(&later.origin, &later.account, &later.token, current)
        .unwrap();
    assert!(a.load_all().unwrap().is_empty());
    assert!(a.state().unwrap().target.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

/// The reverse order of the race above, and the one the logout cleanup hit:
/// a login is saved first, then the delayed cleanup of the older session runs.
/// The clear must use the generation captured at logout and be refused, instead
/// of claiming a generation above the newer login and erasing it.
#[test]
fn a_delayed_cleanup_keeps_the_login_saved_after_the_logout() {
    let (dir, path) = scratch("delayed-cleanup");
    let window = SessionStore::new(Some(path.clone()));
    let old = record("https://chat.example.org", "neva", "token-old");
    save_now(&window, &old).unwrap();
    // The logout raises the in-memory barrier first and then captures the
    // generation the file is known to have. The barrier is above the file, so
    // this pins `known_disk_generation` to the file value: returning the barrier
    // (or anything else above the file) would let the cleanup erase a login
    // that another window saves below it.
    let barrier = window.bump_epoch();
    let captured = window.known_disk_generation();
    assert_eq!(captured, window.state().unwrap().generation);
    assert!(
        captured < barrier,
        "the capture is the file generation, below the logout barrier: {captured} vs {barrier}"
    );

    // Another window signs in before the cleanup runs and claims its own
    // generation above the captured one.
    let other = SessionStore::new(Some(path.clone()));
    let claim = other.begin_login();
    let fresh = record("https://chat.example.org", "bob", "token-fresh");
    other.save(&fresh, claim).unwrap();

    assert_eq!(
        window.clear_captured(captured),
        Err(StoreError::Stale),
        "a cleanup above the logout generation cannot erase a newer login"
    );
    // The barrier-checked form of the other tests refuses it for the same
    // reason: the file is above the captured generation.
    assert_eq!(window.clear_all(captured), Err(StoreError::Stale));
    // Nothing was cleared: the old record and the fresh one are still there,
    // and the fresh one owns the target.
    assert_eq!(window.load_all().unwrap(), vec![old, fresh.clone()]);
    assert_eq!(
        window.state().unwrap().target,
        Some(AutoLoginTarget::from_record(&fresh))
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A save that was queued before the logout must fail its claim instead of
/// taking the next generation: `begin_login_checked` refuses both an
/// in-process barrier and a file that another process moved on.
#[test]
fn a_queued_login_claim_is_refused_after_the_generation_moved() {
    let (dir, path) = scratch("checked-claim");
    let store = SessionStore::new(Some(path.clone()));
    let saved = record("https://chat.example.org", "neva", "token-1");
    save_now(&store, &saved).unwrap();
    let captured = store.epoch();

    // The logout barrier of this process.
    let bumped = store.bump_epoch();
    assert!(bumped > captured);
    assert_eq!(
        store.begin_login_checked(captured),
        Err(StoreError::Stale),
        "a queued save cannot pass the logout barrier"
    );
    assert_eq!(
        store.state().unwrap().generation,
        captured,
        "a refused claim writes nothing"
    );

    // The same file moved on by another process while this one only knows the
    // older generation: the check must read the disk, not just the memory.
    let before = store.state().unwrap().generation;
    let reader = SessionStore::new(Some(path.clone()));
    assert_eq!(reader.state().unwrap().generation, before);
    let other = SessionStore::new(Some(path.clone()));
    let claim = other.begin_login();
    assert!(claim > before);
    assert_eq!(
        reader.epoch(),
        before,
        "an uninformed store still holds the old generation in memory"
    );
    assert_eq!(
        reader.begin_login_checked(before),
        Err(StoreError::Stale),
        "a queued save cannot pass a newer generation of the file"
    );
    assert_eq!(reader.load_all().unwrap(), vec![saved]);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The claim reads, decrypts and rewrites the file under both locks, but the
/// logout barrier is an atomic that the UI thread raises without waiting for
/// them. A logout that lands in that window must not let the queued save pass:
/// the claim is refused after its write, it leaves no record, and the cleanup
/// captured before the write still clears the records that existed then.
#[test]
fn a_logout_during_the_claim_leaves_no_record_for_the_cleanup() {
    let (dir, path) = scratch("claim-logout");
    let store = SessionStore::new(Some(path));
    let old = record("https://chat.example.org", "neva", "token-old");
    save_now(&store, &old).unwrap();
    let expected = store.epoch();
    let disk = store.state().unwrap().generation;

    // The UI thread logs out after the last barrier check of the claim and
    // before the claim reaches the file: the window a stress run hits by
    // timing, fixed here by the hook.
    let logout = store.clone();
    let captured = std::sync::Arc::new(AtomicU64::new(u64::MAX));
    let capture = captured.clone();
    let fired = std::sync::Arc::new(AtomicBool::new(false));
    store.set_write_hook(move || {
        // Only the claim is raced here; the cleanup write that follows must run
        // on the generation the logout captured.
        if fired.swap(true, Ordering::SeqCst) {
            return;
        }
        logout.bump_epoch();
        capture.store(logout.known_disk_generation(), Ordering::SeqCst);
    });

    assert_eq!(
        store.begin_login_checked(expected),
        Err(StoreError::Stale),
        "the claim of a login queued before the logout is refused"
    );
    let captured = captured.load(Ordering::SeqCst);
    assert_eq!(captured, disk, "the logout captured the file generation");

    // The claim did reach the file. It wrote no record, so the file still holds
    // exactly the records and the record generation of the capture: that is
    // what keeps the logout cleanup allowed to run.
    let moved = store.state().unwrap();
    assert!(moved.generation > captured, "the claim moved the file on");
    assert_eq!(
        moved.records,
        vec![old.clone()],
        "the claim wrote no record"
    );
    assert_eq!(moved.records_generation, captured);
    assert!(moved.target.is_some());

    // The save that the old claim value accepted would have revived the
    // session; no caller runs it because the claim failed. The cleanup of the
    // logout now sees a moved file and still owns the records it captured.
    store.clear_captured(captured).unwrap();
    let state = store.state().unwrap();
    assert!(state.records.is_empty(), "the logout leaves no record");
    assert!(state.target.is_none());
    assert!(!state.restore_blocked);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The same order, repeated on a fresh file and generation: a claim runs on a
/// plain thread, the logout barrier is raised while the claim waits at the
/// write, and the save that follows is what the claim would have allowed. Every
/// round must refuse the claim and leave nothing for the next start.
#[test]
fn a_logout_during_the_claim_is_refused_on_every_generation() {
    for round in 0..200 {
        let (dir, path) = scratch(&format!("claim-logout-loop-{round}"));
        let store = SessionStore::new(Some(path));
        let old = record(
            "https://chat.example.org",
            "neva",
            &format!("token-old-{round}"),
        );
        save_now(&store, &old).unwrap();
        let expected = store.epoch();
        let disk = store.state().unwrap().generation;

        // 0: idle, 1: the claim is at its write, 2: the logout captured and the
        // claim may finish.
        let phase = std::sync::Arc::new(AtomicU64::new(0));
        let hook_phase = phase.clone();
        store.set_write_hook(move || {
            // The first write of this round is the claim; the cleanup write
            // after it must not wait for another logout.
            if hook_phase
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            wait_phase(&hook_phase, 2);
        });
        let queued = record(
            "https://chat.example.org",
            "bob",
            &format!("token-queued-{round}"),
        );
        let writer = store.clone();
        let claim_thread = std::thread::spawn(move || {
            let claim = writer.begin_login_checked(expected);
            if let Ok(claim) = claim {
                writer.save(&queued, claim).unwrap();
            }
            claim
        });
        wait_phase(&phase, 1);
        store.bump_epoch();
        let captured = store.known_disk_generation();
        assert_eq!(captured, disk, "round {round}: the capture is the file");
        phase.store(2, Ordering::SeqCst);

        assert_eq!(
            claim_thread.join().unwrap(),
            Err(StoreError::Stale),
            "round {round}: the queued save cannot pass the logout"
        );
        store.clear_captured(captured).unwrap();
        let state = store.state().unwrap();
        assert!(
            state.records.is_empty(),
            "round {round}: the logout leaves no record"
        );
        assert!(state.target.is_none(), "round {round}: no target");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Every write has the same window as the claim: the file is read, decrypted
/// and replaced under both locks, while the logout raises its barrier without
/// taking them. A write that lands after the capture must not leave content that
/// the cleanup treats as a newer login; it is marked with the generation the
/// file had before it, so the cleanup still removes the token of the session
/// that ended.
#[test]
fn a_write_that_straddles_the_logout_is_cleared_by_its_cleanup() {
    let (dir, path) = scratch("write-logout");
    let store = SessionStore::new(Some(path));
    let old = record("https://chat.example.org", "neva", "token-old");
    save_now(&store, &old).unwrap();
    // This session already knows a generation above the file, e.g. from the
    // claim of the login it is still writing.
    let expected = store.bump_epoch();

    let logout = store.clone();
    let captured = std::sync::Arc::new(AtomicU64::new(u64::MAX));
    let capture = captured.clone();
    let fired = std::sync::Arc::new(AtomicBool::new(false));
    store.set_write_hook(move || {
        if fired.swap(true, Ordering::SeqCst) {
            return;
        }
        logout.bump_epoch();
        capture.store(logout.known_disk_generation(), Ordering::SeqCst);
    });

    let rotated = store.update_after_refresh(
        "https://chat.example.org",
        "neva",
        &["token-old"],
        "token-new",
        crate::auth_session::now_unix() + 7_200,
        expected,
    );
    assert_eq!(
        rotated,
        Err(StoreError::Stale),
        "the rotation loses to the logout that was raised during its write"
    );
    let captured = captured.load(Ordering::SeqCst);
    assert_eq!(captured, 0, "the logout captured the file generation");
    let state = store.state().unwrap();
    assert_eq!(
        state.records_generation, captured,
        "the record of the ended session is marked at the capture, not above it"
    );
    assert!(!state.records.is_empty(), "the write did reach the file");
    store.clear_captured(captured).unwrap();
    let state = store.state().unwrap();
    assert!(
        state.records.is_empty(),
        "the cleanup of the logout removes the token the rotation wrote"
    );
    assert!(state.target.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_clear_on_a_read_only_file_reports_io_and_keeps_the_records() {
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

    let (dir, path) = scratch("clear-readonly");
    let store = SessionStore::new(Some(path.clone()));
    save_now(
        &store,
        &record("https://chat.example.org", "neva", "token-1"),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    let guard = ReadWrite(path);
    assert_eq!(
        store.clear_all(store.epoch()),
        Err(StoreError::Io),
        "a failed cleanup must be visible"
    );
    assert_eq!(store.load_all().unwrap().len(), 1);
    // The persistent marker also cannot be written on this disk.
    assert_eq!(store.block_restore(store.epoch()), Err(StoreError::Io));
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}
