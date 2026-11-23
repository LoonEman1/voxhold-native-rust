//! Logout and a new login own the store generation. A write or a delete that
//! was already queued must not change the file of the session that replaced
//! it, and a failed delete must be visible instead of silent.

use super::*;
use crate::session_store::StoreError;
use gpui::{TestAppContext, VisualTestContext};
use std::path::PathBuf;

struct ReadOnly(PathBuf);

impl Drop for ReadOnly {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::metadata(&self.0) {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            let _ = std::fs::set_permissions(&self.0, permissions);
        }
    }
}

#[gpui::test]
async fn logout_invalidates_a_write_that_was_already_queued(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("logout-epoch");
    let store = store(&dir);
    store
        .save_now(&record(&server.base, "neva", 7, "token-1", 3_600))
        .unwrap();
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    let before = view.read_with(&*visual, |v, _| v.session_store.epoch());
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &server.base, "token-1");
            v.logout(window, cx);
        });
    });
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap().is_empty()
    });

    // Exactly what a save that started before the logout would carry.
    let late = view.read_with(&*visual, |v, _| {
        v.session_store.save(
            &record(&server.base, "neva", 7, "token-late", 3_600),
            before,
        )
    });
    assert_eq!(late, Err(StoreError::Stale));
    assert!(
        store.load_all().unwrap().is_empty(),
        "the late save must not revive the deleted session"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A save queued by a manual login that completed just before the logout must
/// not claim a newer generation afterwards: the checked claim refuses it, so
/// the record does not come back even when the save task runs after the clear.
/// The close-time fallback runs the cleanup synchronously, so the result does
/// not depend on the order the executor happens to poll the two tasks in.
#[gpui::test]
async fn a_queued_remember_save_cannot_revive_the_login_after_logout(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("queued-save-logout");
    let store = store(&dir);
    store
        .save_now(&record(&server.base, "neva", 7, "token-1", 3_600))
        .unwrap();
    let (window, view) = open(cx, &server.base, &dir);
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &server.base, "token-1");
            // The password login has just answered: its remember-save is queued
            // with the generation of this session.
            v.remember_manual_login(
                true,
                server.base.clone(),
                "neva".into(),
                7,
                "token-1".into(),
                now_unix() + 3_600,
                window,
                cx,
            );
            // The user logs out before that save reaches the disk.
            v.logout(window, cx);
            v.finish_pending_cleanup();
        });
    });
    visual.run_until_parked();
    assert!(
        store.load_all().unwrap().is_empty(),
        "the queued save must not revive the session that was just ended"
    );
    assert!(store.state().unwrap().target.is_none());
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none());
        assert!(v.last_stored_token.is_none());
    });
    std::fs::remove_dir_all(dir).unwrap();
}

/// A logout while the previous cleanup is still running is not a no-op: the
/// login saved between the two logouts claims a newer generation, the running
/// clear is refused for it as `Stale`, and only a second pass bound to the
/// generation of the second logout removes it. The test executor does not poll
/// the queued cleanup before the second logout, so the order is fixed.
#[gpui::test]
async fn a_second_logout_during_the_cleanup_also_clears_its_login(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("second-logout");
    let store = store(&dir);
    store
        .save_now(&record(&server.base, "neva", 7, "token-a", 3_600))
        .unwrap();
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &server.base, "token-a");
            // The first cleanup is queued but the executor has not polled it.
            v.logout(window, cx);
            // A new login is remembered in the meantime: its claim is above the
            // generation the first logout captured.
            let claim = v.session_store.begin_login();
            v.session_store
                .save(&record(&server.base, "bob", 9, "token-b", 3_600), claim)
                .unwrap();
            // The user logs out again while that first clear still runs. The
            // session exists only so the logout has a revocation to post; the
            // cleanup itself is profile-wide.
            install_session(v, &server.base, "token-b");
            v.logout(window, cx);
        });
    });
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap().is_empty()
    });
    wait_for(&mut visual, &view, |v| {
        !v.cleanup_pending && !v.cleanup_retry
    });
    let state = store.state().unwrap();
    assert!(
        state.records.is_empty(),
        "the second logout leaves no record"
    );
    assert!(state.target.is_none(), "no target survives the two logouts");
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none());
        assert!(!v.auto_blocked);
    });
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_failed_logout_delete_is_visible_and_keeps_the_record(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("logout-readonly");
    let store = store(&dir);
    store
        .save_now(&record(&server.base, "neva", 7, "token-1", 3_600))
        .unwrap();
    let path = dir.join("sessions.bin");
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    let guard = ReadOnly(path);

    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &server.base, "token-1");
            v.logout(window, cx);
        });
    });
    // One retry, then the address screen names the failure.
    wait_for(&mut visual, &view, |v| {
        v.auth
            .status
            .starts_with(crate::app::auto_login::CLEAR_ERROR)
    });
    assert!(
        !store.load_all().unwrap().is_empty(),
        "the read-only file still holds the record"
    );
    draw(&mut visual);
    assert!(
        visual.debug_bounds("auth-retry-login").is_none(),
        "the delete error is not a retry of the login"
    );
    assert!(
        visual.debug_bounds("auth-cleanup-retry").is_some(),
        "the failed cleanup offers a local retry"
    );
    assert_eq!(
        view.read_with(&*visual, |v, _| v.auth.screen),
        crate::auth::AuthScreen::Instance
    );

    // The notice itself must stay readable rather than be swallowed.
    assert!(view
        .read_with(&*visual, |v, _| v.auth.status.clone())
        .starts_with(crate::app::auto_login::CLEAR_ERROR));
    drop(guard);
    // The retry runs on the now writable disk and finishes the cleanup.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.retry_cleanup(window, cx));
    });
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap().is_empty()
    });
    wait_for(&mut visual, &view, |v| !v.cleanup_retry);
    view.read_with(&*visual, |v, _| {
        assert!(v.auth.status.is_empty());
        assert!(!v.auto_blocked);
    });
    std::fs::remove_dir_all(dir).unwrap();
}
