//! A09 races between the cold start, a second process and the logout cleanup.
//!
//! A gpui test drives the background executor on its own thread, so a gated
//! HTTP answer blocks that thread until it is released. The second writer
//! therefore runs on a plain thread and is released by the server-side gate,
//! not by a sleep: the clear is strictly inside the request flight.

use super::*;
use crate::test_support::Gate;

/// A09: a logout in another process between the request and its answer makes
/// the answer stale; the message form opens and the newer generation stays.
#[gpui::test]
async fn a09_a_logout_in_another_process_skips_the_late_answer(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")).with_gate(gate.clone()),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("a09-race");
    let session_store = store(&dir);
    save(
        &session_store,
        &record(&server.base, "neva", 7, "token-old", 3_600),
    );
    let path = dir.join("sessions.bin");
    let other = std::thread::spawn(move || {
        assert!(
            gate.wait_arrived(Duration::from_secs(5)),
            "the profile request is in flight"
        );
        let second = SessionStore::new(Some(path));
        let generation = second.clear_all(second.epoch()).unwrap();
        gate.release();
        generation
    });
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    wait_for(&mut visual, &view, |v| {
        !v.auth.auto_login && !v.startup_restoring
    });
    let generation = other.join().unwrap();
    assert!(generation > 0);
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none(), "the late answer is not applied");
        assert_eq!(v.auth.screen, auth::AuthScreen::Instance);
    });
    assert!(session_store.state().unwrap().records.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

/// P1: the store read of a delayed answer must not drop the answer. A clear
/// from another process in the middle of the profile flight raises the
/// in-process generation; the attempt still settles on the manual form inside
/// the budget instead of leaving «Восстанавливаем вход…» on screen forever.
#[gpui::test]
async fn a09_a_clear_during_the_delayed_profile_opens_the_manual_form(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")).with_gate(gate.clone()),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("a09-clear-midflight");
    save(
        &store(&dir),
        &record(&server.base, "neva", 7, "token-old", 3_600),
    );
    let path = dir.join("sessions.bin");
    let other = std::thread::spawn(move || {
        // The background task has already captured the generation it started
        // under; the store read of the answer has not run yet, so only the
        // generation check can settle this attempt.
        assert!(
            gate.wait_arrived(Duration::from_secs(5)),
            "the profile request is in flight"
        );
        let second = SessionStore::new(Some(path));
        let generation = second.clear_all(second.epoch()).unwrap();
        gate.release();
        generation
    });
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let started = std::time::Instant::now();
    wait_for(&mut visual, &view, |v| {
        !v.auth.auto_login && !v.startup_restoring
    });
    // The answer is released at once and the budget is 8 s; a dropped answer
    // would never settle at all.
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "the startup surface settled in {:?}",
        started.elapsed()
    );
    let generation = other.join().unwrap();
    assert!(generation > 0);
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none(), "the late answer is not applied");
        assert_eq!(v.auth.screen, auth::AuthScreen::Instance);
    });
    draw(&mut visual);
    assert!(visual.debug_bounds("auth-instance-field").is_some());
    assert!(store(&dir).state().unwrap().records.is_empty());
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);
    std::fs::remove_dir_all(dir).unwrap();
}

/// P2: a session that ends must not delete the record a second instance saved
/// for the same account after this session started.
#[gpui::test]
async fn a09_an_expiry_keeps_the_newer_login_of_another_process(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("a09-two-instances");
    let app_store = store(&dir);
    save(
        &app_store,
        &record(&server.base, "neva", 7, "token-old", 3_600),
    );
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    wait_for(&mut visual, &view, |v| v.session.is_some());
    // The captured generation of the older session is already in memory; the
    // second instance claims a newer one on disk and signs in again.
    let other = SessionStore::new(Some(dir.join("sessions.bin")));
    let claim = other.begin_login();
    let fresh = record(&server.base, "neva", 7, "token-fresh", 3_600);
    other.save(&fresh, claim).unwrap();

    // The first session expires. Its local delete names only its own token.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let current = v.session.as_ref().expect("session");
            let expired = Session::new(
                current.origin().to_owned(),
                current.user_id(),
                current.username().to_owned(),
                current.token().to_owned(),
                now_unix() - 1,
            )
            .expect("expired session");
            v.session = Some(expired);
            assert!(
                !v.session_tick(window, cx),
                "the expiry stops the session clock"
            );
        });
    });
    assert!(view.read_with(&*visual, |v, _| v.session.is_none()));
    // The delete runs in the background; the newer record must never go away.
    for _ in 0..60 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(25));
    }
    let state = app_store.state().unwrap();
    assert_eq!(
        state.records,
        vec![fresh.clone()],
        "the record of the newer login stays"
    );
    assert_eq!(
        state.target,
        Some(crate::session_store::AutoLoginTarget::from_record(&fresh))
    );
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);
    std::fs::remove_dir_all(dir).unwrap();
}
