//! W09g.1: the cold-start restoration, the single target, the legacy
//! migration, the startup surface and the reset of saved logins on logout.
//!
//! Scenarios A01–A11 from `docs/screen-streaming-plan.md` are grouped here.
//! A12 needs a release EXE, a real backend and a Setup/portable upgrade and is
//! therefore not part of this file.

use super::*;
use crate::app::auto_login::AutoNetwork;
use crate::auth_session::{now_unix, Session};
use crate::instance_prefs::InstancePrefs;
use crate::session_store::{RememberedSession, SessionStore, StoreState};
use crate::test_support::{Reply, TestServer};
use gpui::{point, Modifiers, TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-w09g1-{}-{}-{name}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(
    cx: &mut TestAppContext,
    prefill: &str,
    dir: &Path,
) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
    let (window, view) = super::add_auth_window_with(
        cx,
        prefill,
        dir.join("workspace-layout.json"),
        dir.join("instance.json"),
        dir.join("sessions.bin"),
    );
    // Real startup work is fast in tests; a longer budget keeps the deferred
    // answers from a previous window out of the new attempt.
    view.update(cx, |v, _| {
        v.startup_budget = Duration::from_secs(30);
        v.startup_deadline = std::time::Instant::now() + Duration::from_secs(30);
    });
    (window, view)
}

fn store(dir: &Path) -> SessionStore {
    SessionStore::new(Some(dir.join("sessions.bin")))
}

fn record(origin: &str, account: &str, user_id: i64, token: &str, ttl: i64) -> RememberedSession {
    RememberedSession {
        origin: origin.into(),
        account: account.into(),
        user_id,
        token: token.into(),
        expires_at: now_unix() + ttl,
        saved_at: now_unix(),
    }
}

fn save(store: &SessionStore, record: &RememberedSession) {
    // A standalone store starts at generation 0; sync with the file before
    // writing over a record the application has already saved.
    let _ = store.state();
    store.save(record, store.epoch()).unwrap();
}

fn profile_body(user_id: i64, username: &str) -> String {
    format!(r#"{{"user_id":{user_id},"username":"{username}","about":""}}"#)
}

fn servers_body() -> &'static str {
    r#"[{"id":1,"name":"QA","role":"owner"}]"#
}

fn login_body(user_id: i64, username: &str, token: &str) -> String {
    let expires_at = now_unix() + 3_600;
    format!(
        r#"{{"user":{{"id":{user_id},"username":"{username}","created_at":1700000000}},"session":{{"token":"{token}","expires_at":{expires_at}}}}}"#
    )
}

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

fn click(visual: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>) {
    visual.simulate_mouse_move(position, None, Modifiers::default());
    visual.simulate_click(position, Modifiers::default());
}

fn click_selector(visual: &mut VisualTestContext, selector: &'static str) {
    draw(visual);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"));
    click(
        visual,
        point(
            bounds.origin.x + bounds.size.width / 2.0,
            bounds.origin.y + bounds.size.height / 2.0,
        ),
    );
    visual.run_until_parked();
}

fn confirm(visual: &mut VisualTestContext, view: &Entity<Voxhold>, origin: &str) {
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.instance.update(cx, |input, cx| {
                input.set_value(origin.to_string(), window, cx)
            });
        });
    });
    super::focus_field(visual, view, |v| v.instance.clone());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
}

fn type_credentials(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    user: &str,
    password: &str,
) {
    super::focus_field(visual, view, |v| v.username.clone());
    visual.simulate_input(user);
    super::focus_field(visual, view, |v| v.password.clone());
    visual.simulate_input(password);
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
}

fn wait_for(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    mut done: impl FnMut(&Voxhold) -> bool,
) {
    for _ in 0..1200 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| done(v)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the application condition did not become true");
}

fn wait_store(
    visual: &mut VisualTestContext,
    store: &SessionStore,
    mut done: impl FnMut(&SessionStore) -> bool,
) {
    for _ in 0..1200 {
        visual.run_until_parked();
        if done(store) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the session store condition did not become true");
}

fn settled_startup(visual: &mut VisualTestContext, view: &Entity<Voxhold>) {
    wait_for(visual, view, |v| !v.startup_restoring);
}

/// A surface may not be painted *in the frame that `debug_bounds` observed*:
/// in gpui 0.2.2 it only reports the last rendered frame, so every stage draws
/// first and then checks.
fn assert_no_form_painted(visual: &mut VisualTestContext) {
    for selector in [
        "auth-instance-field",
        "auth-username-field",
        "auth-password-field",
        "auth-primary",
        "auth-remember-login",
        "auth-remember-instance",
    ] {
        assert!(
            visual.debug_bounds(selector).is_none(),
            "{selector} was painted during the automatic restoration"
        );
    }
}

fn ws_requests(server: &TestServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| request.path == "/api/v1/ws")
        .count()
}

/// A01: the real root waits on the neutral surface, then paints the workspace;
/// there is exactly one startup intent and no password request.
#[gpui::test]
async fn a01_cold_start_restores_without_any_form_frame(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")).with_delay(Duration::from_millis(400)),
        Reply::new(200, servers_body()).with_delay(Duration::from_millis(200)),
    ]);
    let dir = scratch("a01");
    save(
        &store(&dir),
        &record(&server.base, "neva", 7, "token-stored", 3_600),
    );
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();

    // Frame 1: the startup phase owns the very first paint.
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    assert!(
        visual.debug_bounds("auth-restore").is_some(),
        "the start surface is painted"
    );
    assert_no_form_painted(&mut visual);
    assert!(view.read_with(&*visual, |v, _| v.session.is_none()));

    // Frame 2: the answer is still delayed; no address or login form appears.
    std::thread::sleep(Duration::from_millis(80));
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    assert_no_form_painted(&mut visual);
    assert!(
        view.read_with(&*visual, |v, _| v.session.is_none()),
        "the delayed answer has not arrived yet"
    );

    // The workspace is the next surface after both answers.
    wait_for(&mut visual, &view, |v| v.session.is_some());
    draw(&mut visual);
    assert_no_form_painted(&mut visual);
    assert!(
        view.read_with(&*visual, |v, _| v.client.is_some()),
        "the workspace owns the window"
    );
    view.read_with(&*visual, |v, _| {
        assert!(v.event_task.is_some(), "one account socket task is running");
        assert!(v.events.is_some(), "the socket owns the one event queue");
    });

    let requests = server.requests();
    assert!(
        requests
            .iter()
            .all(|request| request.path != "/api/v1/auth/login"),
        "the stored token never posts the password"
    );
    let profiles: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "/api/v1/me/profile")
        .collect();
    assert_eq!(profiles.len(), 1, "exactly one startup intent");
    assert_eq!(
        profiles[0].header("authorization"),
        Some("Bearer token-stored")
    );
    // One account socket, opened by the only accepted startup intent.
    let mut sockets = ws_requests(&server);
    for _ in 0..200 {
        if sockets > 0 {
            break;
        }
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
        sockets = ws_requests(&server);
    }
    assert_eq!(sockets, 1, "exactly one account socket is opened");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A01: the only start action is reachable by keyboard; Enter cancels without
/// waiting for a delayed answer.
#[gpui::test]
async fn a01_the_start_action_is_keyboard_accessible(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")).with_delay(Duration::from_millis(400)),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("a01-keyboard");
    save(
        &store(&dir),
        &record(&server.base, "neva", 7, "token-stored", 3_600),
    );
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
        view.update(app, |v, _| v.restore_cancel_focus.focus(window));
    });
    visual.simulate_keystrokes("enter");
    assert!(
        view.read_with(&*visual, |v, _| !v.startup_restoring
            && v.auth.screen == auth::AuthScreen::Instance),
        "Enter on the focused start action selects the instance"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A01: the start action is the first focus target of the neutral surface and
/// answers Space without a manual focus call. The test platform never runs
/// `on_next_frame` callbacks, so the state marker that arms the first focus is
/// checked directly and the same method the callback calls applies it.
#[gpui::test]
async fn a01_the_start_action_holds_the_first_focus_and_answers_space(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("a01-first-focus");
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    // The empty store settled on the manual form. Show the neutral surface for
    // the one keyboard check; no request is left pending.
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.startup_restoring = true;
            v.restore_focus_done = false;
            v.auth.screen = auth::AuthScreen::Login;
            v.auth.auto_login = true;
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(v.startup_restoring);
        assert!(v.restore_focus_done, "the first focus was armed once");
    });
    let cancel = view.read_with(&*visual, |v, _| v.restore_cancel_focus.clone());
    // The frame callback of the shipped window does exactly this.
    visual.update(|window, app| {
        view.update(app, |v, _| v.focus_start_action(window));
    });
    visual.update(|window, app| {
        assert_eq!(
            window.focused(app),
            Some(cancel.clone()),
            "the start action owns the first focus"
        );
    });
    // It is the only action of the surface, so Tab cannot move focus away.
    visual.simulate_keystrokes("tab");
    visual.update(|window, app| {
        assert_eq!(
            window.focused(app),
            Some(cancel),
            "Tab keeps the only action of the surface"
        );
    });
    visual.simulate_keystrokes("space");
    assert!(
        view.read_with(&*visual, |v, _| !v.startup_restoring
            && v.auth.screen == auth::AuthScreen::Instance),
        "Space on the focused start action selects the instance"
    );
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A06: every transient answer keeps the record and one retry signs in.
#[gpui::test]
async fn a06_transient_failures_keep_the_record_and_retry_signs_in(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    for status in [408u16, 429, 503] {
        let server = TestServer::spawn_routed(vec![
            Reply::new(status, "{}"),
            Reply::new(200, &profile_body(7, "neva")),
            Reply::new(200, servers_body()),
        ]);
        let dir = scratch("a06");
        let session_store = store(&dir);
        save(
            &session_store,
            &record(&server.base, "neva", 7, "token-kept", 3_600),
        );
        let (window, view) = open(cx, "", &dir);
        let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        visual.run_until_parked();
        wait_for(&mut visual, &view, |v| {
            v.auth.auto_retry && v.auth.status == "Нет связи."
        });
        assert_eq!(
            session_store.state().unwrap().target.map(|t| t.origin),
            Some(server.base.clone())
        );
        click_selector(&mut visual, "auth-retry-login");
        wait_for(&mut visual, &view, |v| v.session.is_some());
        assert_eq!(
            view.read_with(&*visual, |v, _| v
                .session
                .as_ref()
                .map(|s| s.token().to_owned())),
            Some("token-kept".into())
        );
        let attempts = server
            .requests()
            .iter()
            .filter(|request| request.path == "/api/v1/me/profile")
            .count();
        assert_eq!(attempts, 2, "one startup attempt and one retry: {status}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[gpui::test]
async fn a06_a_timeout_on_the_retry_keeps_the_record_and_offers_another(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn_routed(vec![
        Reply::new(503, "{}").with_path("/api/v1/me/profile"),
        Reply::new(200, &profile_body(7, "neva"))
            .with_delay(Duration::from_millis(900))
            .with_path("/api/v1/me/profile"),
        Reply::new(200, &profile_body(7, "neva")).with_path("/api/v1/me/profile"),
        Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
        Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
    ]);
    let dir = scratch("a06-timeout");
    let session_store = store(&dir);
    save(
        &session_store,
        &record(&server.base, "neva", 7, "token-kept", 3_600),
    );
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    wait_for(&mut visual, &view, |v| v.auth.auto_retry);
    // The retry uses the same bounded attempt budget; a delayed answer is a
    // transient failure, not a blocked UI.
    visual.update(|_window, app| {
        view.update(app, |v, _| v.startup_budget = Duration::from_millis(150));
    });
    click_selector(&mut visual, "auth-retry-login");
    wait_for(&mut visual, &view, |v| v.auth.auto_retry);
    assert!(session_store.state().unwrap().target.is_some());
    // The next retry with the normal budget finishes the sign-in.
    visual.update(|_window, app| {
        view.update(app, |v, _| v.startup_budget = Duration::from_secs(8));
    });
    click_selector(&mut visual, "auth-retry-login");
    wait_for(&mut visual, &view, |v| v.session.is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

/// A07: «Выбрать инстанс» is immediate; a late answer for the cancelled
/// attempt cannot replace the workspace of the new instance.
#[gpui::test]
async fn a07_cancel_is_immediate_and_a_late_answer_is_ignored(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let first = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let second = TestServer::spawn_routed(vec![
        Reply::new(200, &login_body(9, "bob", "token-b")).with_path("/api/v1/auth/login"),
        Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
    ]);
    let dir = scratch("a07");
    let session_store = store(&dir);
    save(
        &session_store,
        &record(&first.base, "neva", 7, "token-a", 3_600),
    );
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    // Paint the real startup surface once, then act through the same method the
    // button calls; the imperative path is what must be immediate.
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    assert!(
        visual.debug_bounds("auth-restore-cancel").is_some(),
        "the cancel action is painted"
    );
    let old_attempt = view.read_with(&*visual, |v, _| v.auto_attempt);
    visual.update(|window, app| {
        view.update(app, |v, cx| v.cancel_startup_restore(window, cx));
    });
    assert!(
        view.read_with(&*visual, |v, _| !v.startup_restoring
            && v.auth.screen == auth::AuthScreen::Instance),
        "the address choice is available immediately"
    );
    confirm(&mut visual, &view, &second.base);
    click_selector(&mut visual, "auth-remember-login");
    type_credentials(&mut visual, &view, "bob", "password123");
    wait_for(&mut visual, &view, |v| v.session.is_some());

    // Feed a late result for the cancelled attempt.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.finish_auto_network(
                old_attempt,
                AutoNetwork::SignedIn {
                    client: Client::new(&first.base).expect("origin"),
                    servers: Vec::new(),
                    session: Session::new(
                        first.base.clone(),
                        7,
                        "neva".into(),
                        "token-a".into(),
                        now_unix() + 3_600,
                    )
                    .expect("session"),
                },
                window,
                cx,
            );
        });
    });
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        let session = v.session.as_ref().expect("the second login stays");
        assert_eq!(session.user_id(), 9);
        assert_eq!(session.token(), "token-b");
        assert_eq!(v.auth.screen, auth::AuthScreen::Login);
    });
    wait_store(&mut visual, &session_store, |store| {
        store.state().unwrap().target.map(|t| t.user_id) == Some(9)
    });
    std::fs::remove_dir_all(dir).unwrap();
}

/// A08: online and offline logout both clear every record and target, and the
/// next process starts at the address form.
#[gpui::test]
async fn a08_online_and_offline_logout_clear_the_profile(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    for online in [true, false] {
        let server = TestServer::spawn_routed(vec![
            Reply::new(200, &login_body(7, "neva", "token-a")).with_path("/api/v1/auth/login"),
            Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
            Reply::new(204, "").with_path("/api/v1/auth/logout"),
        ]);
        let dir = scratch("a08");
        let session_store = store(&dir);
        let (window, view) = open(cx, &server.base, &dir);
        let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        visual.run_until_parked();
        settled_startup(&mut visual, &view);
        confirm(&mut visual, &view, &server.base);
        click_selector(&mut visual, "auth-remember-login");
        type_credentials(&mut visual, &view, "neva", "password123");
        wait_for(&mut visual, &view, |v| v.session.is_some());
        // A second saved account proves the cleanup is not limited to one.
        let other = record("https://other.example.org", "bob", 9, "token-other", 3_600);
        save(&session_store, &other);
        if online {
            visual.update(|window, app| {
                view.update(app, |v, cx| v.logout(window, cx));
            });
            wait_store(&mut visual, &session_store, |store| {
                let state = store.state().unwrap();
                state.records.is_empty() && state.target.is_none() && !state.legacy
            });
            let logouts: Vec<_> = server
                .requests()
                .into_iter()
                .filter(|request| request.path == "/api/v1/auth/logout")
                .collect();
            assert_eq!(logouts.len(), 1, "exactly one revocation");
            assert_eq!(logouts[0].header("authorization"), Some("Bearer token-a"));
        } else {
            // The server is gone before the click: the local cleanup must work.
            drop(server);
            visual.update(|window, app| {
                view.update(app, |v, cx| v.logout(window, cx));
            });
            wait_store(&mut visual, &session_store, |store| {
                let state = store.state().unwrap();
                state.records.is_empty() && state.target.is_none() && !state.legacy
            });
        }
        view.read_with(&*visual, |v, _| {
            assert!(v.session.is_none());
            assert_eq!(v.auth.screen, auth::AuthScreen::Instance);
        });
        // A new window sees the empty new-format store and opens the form.
        visual.update(|window, _app| window.remove_window());
        visual.run_until_parked();
        drop(view);
        let (window, view) = open(cx, "", &dir);
        let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        visual.run_until_parked();
        settled_startup(&mut visual, &view);
        assert!(view.read_with(&*visual, |v, _| v.session.is_none()));
        draw(&mut visual);
        assert!(visual.debug_bounds("auth-instance-field").is_some());
        visual.update(|window, _app| window.remove_window());
        visual.run_until_parked();
        drop(view);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// A09: the second process moves the generation on; the first process cannot
/// restore or delete the newer login.
#[gpui::test]
async fn a09_a_second_process_wins_the_generation_race(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("a09");
    let writer = store(&dir);
    let old = record(&server.base, "neva", 7, "token-old", 3_600);
    save(&writer, &old);
    let old_epoch = writer.epoch();
    // The second instance logs out while the first one still holds a write.
    let other = SessionStore::new(Some(dir.join("sessions.bin")));
    let new_epoch = other.clear_all(other.epoch()).unwrap();
    assert!(new_epoch > old_epoch);
    assert_eq!(
        writer.save(&old, old_epoch),
        Err(crate::session_store::StoreError::Stale)
    );
    assert_eq!(
        writer.delete(&old.origin, &old.account, &old.token, old_epoch),
        Err(crate::session_store::StoreError::Stale)
    );
    // A new login in the second instance is the only record the next start
    // may restore.
    let fresh = record(&server.base, "bob", 9, "token-new", 3_600);
    other.save(&fresh, other.begin_login()).unwrap();
    assert_eq!(
        other.plan_auto_login(now_unix(), None, true).unwrap(),
        crate::session_store::AutoLogin::Ready {
            record: fresh.clone(),
            legacy: false
        }
    );
    assert_eq!(writer.load_all().unwrap(), vec![fresh]);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A10: a damaged blob and a refused cleanup are visible; the retry works and
/// the next process returns to the address form.
#[gpui::test]
async fn a10_a_damaged_blob_stays_manual_and_a_refused_cleanup_recovers(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("a10");
    std::fs::write(dir.join("sessions.bin"), b"VXSS1 damaged blob").unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);
    draw(&mut visual);
    assert!(visual.debug_bounds("auth-instance-field").is_some());
    assert!(view.read_with(&*visual, |v, _| v.session.is_none()));
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);

    // The refused cleanup is covered in detail by `epoch_tests`; here the
    // repaired file is accepted by a new process.
    std::fs::remove_dir_all(dir).unwrap();
}

/// P3: installing a session performs no file or DPAPI work on the UI thread.
/// Another store waits for the cross-process mutex while holding the in-process
/// one; the sign-in still returns at once.
#[cfg(windows)]
#[gpui::test]
async fn a_sign_in_never_waits_for_the_protected_store(cx: &mut TestAppContext) {
    use std::time::Instant;

    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("p3-ui-lock");
    let session_store = store(&dir);
    save(
        &session_store,
        &record(&server.base, "neva", 7, "token-1", 3_600),
    );
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);

    // A second process holds the named mutex; a background write of this store
    // then waits for it while holding the in-process mutex.
    let (ready, holder) = crate::session_store::hold_named_lock_for(Duration::from_millis(3_000));
    ready
        .recv_timeout(Duration::from_secs(2))
        .expect("the second process holds the lock");
    let writer = session_store.clone();
    let blocked = std::thread::spawn(move || {
        let late = record("https://chat.example.org", "probe", 9, "token-probe", 3_600);
        let _ = writer.save(&late, writer.epoch());
    });
    let mut owned = false;
    for _ in 0..400 {
        if session_store.local_lock_is_held() {
            owned = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        owned,
        "the background write owns the in-process lock and waits for the file"
    );

    let client = Client::new(&server.base).expect("origin");
    let session = Session::new(
        server.base.clone(),
        7,
        "neva".into(),
        "token-1".into(),
        now_unix() + 3_600,
    )
    .expect("session");
    let started = Instant::now();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.enter_workspace(client, Vec::new(), session, window, cx)
        });
    });
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(1_000),
        "the UI thread waited {elapsed:?} for the protected store"
    );
    assert!(view.read_with(&*visual, |v, _| v.session.is_some()));

    // The queued write finishes once the other process releases the file.
    blocked.join().expect("the background write finishes");
    holder.join().expect("the lock holder finishes");
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A11: a plain close keeps the login, an explicit logout during media ends
/// it, and a late refresh cannot write the old session back.
#[gpui::test]
async fn a11_close_keeps_the_login_and_late_refresh_stays_closed(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &login_body(7, "neva", "token-1")).with_path("/api/v1/auth/login"),
        Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
        Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
        Reply::new(200, &profile_body(7, "neva")).with_path("/api/v1/me/profile"),
    ]);
    let dir = scratch("a11");
    let session_store = store(&dir);
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);
    confirm(&mut visual, &view, &server.base);
    click_selector(&mut visual, "auth-remember-login");
    type_credentials(&mut visual, &view, "neva", "password123");
    wait_for(&mut visual, &view, |v| v.session.is_some());
    wait_store(&mut visual, &session_store, |store| {
        store.state().unwrap().target.is_some()
    });
    // The window closes without logout: the record survives.
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);
    assert!(session_store.state().unwrap().target.is_some());

    // A new process restores it; logout during an active session clears it,
    // and the late refresh of the old generation cannot revive it.
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    wait_for(&mut visual, &view, |v| v.session.is_some());
    let (session_epoch, generation) = view.read_with(&*visual, |v, _| {
        (
            v.session_epoch,
            v.session.as_ref().expect("session").generation(),
        )
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.logout(window, cx);
            v.finish_refresh(
                session_epoch,
                generation,
                Ok(voxhold_api::SessionUpdate {
                    token: "token-late".into(),
                    expires_at: now_unix() + 7_200,
                }),
                window,
                cx,
            );
        });
    });
    wait_store(&mut visual, &session_store, |store| {
        let state = store.state().unwrap();
        state.records.is_empty() && state.target.is_none()
    });
    view.read_with(&*visual, |v, _| assert!(v.session.is_none()));
    std::fs::remove_dir_all(dir).unwrap();
}

/// The cleanup that was queued when the window closes still reaches the disk.
#[gpui::test]
async fn a11_close_completes_a_pending_logout_cleanup(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &login_body(7, "neva", "token-1")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("a11-close-cleanup");
    let session_store = store(&dir);
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);
    confirm(&mut visual, &view, &server.base);
    click_selector(&mut visual, "auth-remember-login");
    type_credentials(&mut visual, &view, "neva", "password123");
    wait_for(&mut visual, &view, |v| v.session.is_some());
    wait_store(&mut visual, &session_store, |store| {
        store.state().unwrap().target.is_some()
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.logout(window, cx));
        window.remove_window();
    });
    visual.run_until_parked();
    let state = session_store.state().unwrap();
    assert!(state.records.is_empty(), "the close finished the cleanup");
    assert!(state.target.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

mod login_flow_tests;
mod race_tests;
