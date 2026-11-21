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
