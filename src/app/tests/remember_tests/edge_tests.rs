//! The login card edge cases: a stale record must not hide a working one, a
//! Cyrillic account matches regardless of case, the checkbox state at the end
//! of a request decides, and the retry link stays inside the error row.

use super::*;
use crate::test_support::Gate;
use gpui::{px, size, TestAppContext, VisualTestContext};

fn set_credentials(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    user: &str,
    password: &str,
) {
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.username.update(cx, |input, cx| {
                input.set_value(user.to_string(), window, cx)
            });
            v.password.update(cx, |input, cx| {
                input.set_value(password.to_string(), window, cx)
            });
        });
    });
}

fn submit_and_toggle(visual: &mut VisualTestContext, view: &Entity<Voxhold>, remember: bool) {
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.submit_login(window, cx);
            // Exactly what the painted checkbox does when clicked while busy.
            v.auto_attempt = v.auto_attempt.wrapping_add(1);
            v.auth.auto_login = false;
            v.auth.auto_retry = false;
            v.auto_record = None;
            v.auth.remember_login = remember;
        });
    });
}

#[gpui::test]
async fn an_expired_newest_record_does_not_hide_a_valid_older_one(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("expired-newest-app");
    let store = store(&dir);
    let mut valid = record(&server.base, "neva", 7, "token-valid", 3_600);
    valid.saved_at = now_unix() - 100;
    let mut expired = record(&server.base, "bob", 9, "token-expired", -10);
    expired.saved_at = now_unix();
    store.save_now(&valid).unwrap();
    store.save_now(&expired).unwrap();

    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    confirm(&mut visual, &view, &server.base);
    wait_for(&mut visual, &view, |v| v.session.is_some());
    assert_eq!(
        view.read_with(&*visual, |v, _| v
            .session
            .as_ref()
            .map(|s| s.token().to_owned())),
        Some("token-valid".into())
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_cyrillic_account_matches_in_another_case(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &profile_body(7, "Нева")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("cyrillic");
    let store = store(&dir);
    store
        .save_now(&record(&server.base, "нева", 7, "token-cyr", 3_600))
        .unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    confirm(&mut visual, &view, &server.base);
    wait_for(&mut visual, &view, |v| v.session.is_some());
    assert_eq!(
        view.read_with(&*visual, |v, _| v
            .session
            .as_ref()
            .map(|s| s.username().to_owned())),
        Some("Нева".into())
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn unchecking_during_a_manual_login_does_not_save(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &login_body(7, "neva", "token-manual")).with_gate(gate.clone()),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("manual-toggle-off");
    let store = store(&dir);
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    toggle(&mut visual, "auth-remember-login");
    set_credentials(&mut visual, &view, "neva", "password123");
    gate.arm();
    submit_and_toggle(&mut visual, &view, false);
    // The gated reply is released only after the box was cleared, so the
    // completion handler must not save the record.
    gate.release();
    wait_for(&mut visual, &view, |v| v.session.is_some());
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap().is_empty()
    });
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn checking_during_a_manual_login_saves_the_record(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, &login_body(7, "neva", "token-manual")).with_gate(gate.clone()),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("manual-toggle-on");
    let store = store(&dir);
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    set_credentials(&mut visual, &view, "neva", "password123");
    gate.arm();
    submit_and_toggle(&mut visual, &view, true);
    // The gated reply is released only after the box was set, so the completion
    // handler must save the record.
    gate.release();
    wait_for(&mut visual, &view, |v| v.session.is_some());
    wait_store(&mut visual, &store, |store| {
        store
            .load_now(&server.base)
            .unwrap()
            .is_some_and(|record| record.token == "token-manual")
    });
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn the_retry_link_stays_inside_the_error_row(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("retry-bounds");
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.auth.screen = auth::AuthScreen::Login;
            v.auth.origin = "http://127.0.0.1:1".into();
            v.auth.status = "Нет связи.".into();
            v.auth.auto_retry = true;
            v.auto_record = Some(record("http://127.0.0.1:1", "neva", 7, "token", 3_600));
            cx.notify();
        });
    });

    for (width, height) in [
        (320.0, 600.0),
        (360.0, 640.0),
        (390.0, 760.0),
        (720.0, 900.0),
        (1280.0, 900.0),
    ] {
        visual.simulate_resize(size(px(width), px(height)));
        draw(&mut visual);
        let (vw, vh) = view.read_with(&*visual, |v, _| v.viewport);
        let layout = auth::layout_auth(vw, vh, false, true);
        let bounds = visual
            .debug_bounds("auth-retry-login")
            .unwrap_or_else(|| panic!("{width}x{height}: the retry link is painted"));
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        assert!(
            x >= layout.error.x - 1.0
                && y >= layout.error.y - 1.0
                && x + w <= layout.error.right() + 1.0
                && y + h <= layout.error.bottom() + 1.0,
            "{width}x{height}: link ({x},{y},{w},{h}) leaves the error row {:?}",
            layout.error
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
