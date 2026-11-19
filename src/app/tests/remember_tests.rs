//! «Запомнить вход» on the real auth window and the real workspace: the two
//! independent checkboxes, automatic sign-in, transient retry, 401 and wrong
//! user deletion, a toggled box during the flight, rotation persistence,
//! logout, window close, a damaged file and two origins that must never share
//! a Bearer token. Every path uses a fresh temporary directory.

use super::*;
use crate::auth_session::{now_unix, Session};
use crate::session_store::{RememberedSession, SessionStore};
use crate::test_support::{Reply, TestServer};
use gpui::{point, px, size, Modifiers, TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};
use std::time::Duration;
use voxhold_api::{Client, SessionUpdate};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-remember-{}-{}-{name}",
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
    super::add_auth_window_with(
        cx,
        prefill,
        dir.join("workspace-layout.json"),
        dir.join("instance.json"),
        dir.join("sessions.bin"),
    )
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

fn toggle(visual: &mut VisualTestContext, selector: &'static str) {
    draw(visual);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"));
    click(
        visual,
        point(
            bounds.origin.x + px(9.),
            bounds.origin.y + bounds.size.height / 2.0,
        ),
    );
    visual.run_until_parked();
}

fn set_origin(visual: &mut VisualTestContext, view: &Entity<Voxhold>, origin: &str) {
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.instance.update(cx, |input, cx| {
                input.set_value(origin.to_string(), window, cx)
            });
        });
    });
}

fn confirm(visual: &mut VisualTestContext, view: &Entity<Voxhold>, origin: &str) {
    set_origin(visual, view, origin);
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
    for _ in 0..1000 {
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
    for _ in 0..1000 {
        visual.run_until_parked();
        if done(store) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the session store condition did not become true");
}

fn settled_auto(visual: &mut VisualTestContext, view: &Entity<Voxhold>) {
    wait_for(visual, view, |v| !v.auth.auto_login);
}

#[gpui::test]
async fn a_stored_session_enters_without_a_password(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("auto-success");
    let store = store(&dir);
    // The account case differs from the profile answer; it must still match.
    store
        .save(&record(&server.base, "NEVA", 7, "token-stored", 3_600))
        .unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    wait_for(&mut visual, &view, |v| v.session.is_some());

    let (origin, user_id, token) = view.read_with(&*visual, |v, _| {
        let session = v.session.as_ref().expect("session");
        (
            session.origin().to_owned(),
            session.user_id(),
            session.token().to_owned(),
        )
    });
    assert_eq!(origin, server.base);
    assert_eq!(user_id, 7);
    assert_eq!(token, "token-stored");
    assert_eq!(
        view.read_with(&*visual, |v, _| v.last_stored_token.clone()),
        Some("token-stored".into())
    );

    let requests = server.requests();
    assert!(
        requests.iter().all(|r| r.path != "/api/v1/auth/login"),
        "a stored session must not post the password"
    );
    let profile = requests
        .iter()
        .find(|r| r.path == "/api/v1/me/profile")
        .expect("profile request");
    assert_eq!(profile.header("authorization"), Some("Bearer token-stored"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_401_deletes_the_record_and_asks_for_the_password(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(401, "{}")]);
    let dir = scratch("auto-401");
    let store = store(&dir);
    store
        .save(&record(&server.base, "neva", 7, "token-dead", 3_600))
        .unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    wait_for(&mut visual, &view, |v| {
        !v.auth.auto_login && v.auth.status.contains("Сессия истекла")
    });

    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none());
        assert!(v.client.is_none());
        assert!(!v.auth.remember_login);
    });
    assert!(store.load_for_origin(&server.base).unwrap().is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_network_failure_keeps_the_record_and_retry_signs_in(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(503, "{}"),
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("auto-retry");
    let store = store(&dir);
    store
        .save(&record(&server.base, "neva", 7, "token-kept", 3_600))
        .unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    wait_for(&mut visual, &view, |v| {
        v.auth.auto_retry && v.auth.status == "Нет связи с сервером."
    });
    let kept = store
        .load_for_origin(&server.base)
        .unwrap()
        .expect("the record survives a network failure");
    assert_eq!(kept.token, "token-kept");
    assert_eq!(kept.account, "neva");

    draw(&mut visual);
    let bounds = visual
        .debug_bounds("auth-retry-login")
        .expect("the retry link is painted");
    click(
        &mut visual,
        point(
            bounds.origin.x + bounds.size.width / 2.0,
            bounds.origin.y + bounds.size.height / 2.0,
        ),
    );
    wait_for(&mut visual, &view, |v| v.session.is_some());
    assert_eq!(
        view.read_with(&*visual, |v, _| v
            .session
            .as_ref()
            .map(|s| s.token().to_owned())),
        Some("token-kept".into())
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_record_for_another_user_is_deleted(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &profile_body(99, "bob")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("wrong-user");
    let store = store(&dir);
    store
        .save(&record(&server.base, "neva", 7, "token-other", 3_600))
        .unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    wait_for(&mut visual, &view, |v| {
        !v.auth.auto_login && v.auth.status.contains("другого аккаунта")
    });
    assert!(store.load_for_origin(&server.base).unwrap().is_none());
    view.read_with(&*visual, |v, _| assert!(v.session.is_none()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn unchecking_during_the_flight_blocks_the_late_success(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("uncheck-flight");
    let store = store(&dir);
    store
        .save(&record(&server.base, "neva", 7, "token-flight", 3_600))
        .unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            // The state while the profile answer is in flight.
            v.auth.screen = auth::AuthScreen::Login;
            v.auth.origin = server.base.clone();
            v.auth.remember_login = true;
            v.auth.auto_login = true;
            v.auto_attempt = 7;
            v.auto_record = store.load_for_origin(&server.base).unwrap();

            // Exactly what the painted box does: the attempt id moves and the
            // preference turns off.
            v.auto_attempt = v.auto_attempt.wrapping_add(1);
            v.auth.auto_login = false;
            v.auth.remember_login = false;
            v.auto_record = None;

            // The late success for the old attempt arrives now.
            let client = Client::new(&server.base).expect("origin");
            let session = Session::new(
                server.base.clone(),
                7,
                "neva".into(),
                "token-flight".into(),
                now_unix() + 3_600,
            )
            .expect("session");
            v.finish_auto_network(
                7,
                crate::app::auto_login::AutoNetwork::SignedIn {
                    client,
                    servers: Vec::new(),
                    session,
                },
                window,
                cx,
            );
            assert!(v.session.is_none(), "the late success must not sign in");
            assert_eq!(v.auth.screen, auth::AuthScreen::Login);
            assert!(!v.auth.remember_login);
        });
    });
    assert!(
        store.load_for_origin(&server.base).unwrap().is_some(),
        "unchecking cancels the attempt but does not delete the record"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn an_expired_record_is_deleted_without_a_request(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("expired");
    let store = store(&dir);
    store
        .save(&record(&server.base, "neva", 7, "token-old", -10))
        .unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    settled_auto(&mut visual, &view);
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap().is_empty()
    });
    assert!(
        server.requests().is_empty(),
        "an expired record is not sent"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn the_address_and_login_boxes_are_independent_in_all_four_combinations(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    for (address, login) in [(false, false), (false, true), (true, false), (true, true)] {
        let server = TestServer::spawn(vec![
            Reply::new(200, &login_body(7, "neva", "token-manual")),
            Reply::new(200, servers_body()),
        ]);
        let dir = scratch("combinations");
        let store = store(&dir);
        let (window, view) = open(cx, &server.base, &dir);
        let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        visual.run_until_parked();

        if address {
            toggle(&mut visual, "auth-remember-instance");
        }
        confirm(&mut visual, &view, &server.base);
        if login {
            toggle(&mut visual, "auth-remember-login");
        }
        type_credentials(&mut visual, &view, "neva", "password123");
        wait_for(&mut visual, &view, |v| v.session.is_some());
        wait_store(&mut visual, &store, |store| {
            if login {
                store.load_for_origin(&server.base).unwrap().is_some()
            } else {
                store.load_all().unwrap().is_empty()
            }
        });

        let instance = std::fs::read_to_string(dir.join("instance.json")).unwrap();
        if address {
            assert!(
                instance.contains("\"remember_instance\":true"),
                "{instance}"
            );
            assert!(instance.contains(&server.base), "{instance}");
        } else {
            assert_eq!(instance, "{\"remember_instance\":false}");
        }
        if login {
            let saved = store.load_for_origin(&server.base).unwrap().unwrap();
            assert_eq!(saved.account, "neva");
            assert_eq!(saved.user_id, 7);
            assert_eq!(saved.token, "token-manual");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[gpui::test]
async fn an_unchecked_manual_login_removes_a_matching_record(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(503, "{}"),
        Reply::new(200, &login_body(7, "neva", "token-new")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("manual-unchecked");
    let store = store(&dir);
    store
        .save(&record(&server.base, "neva", 7, "token-old", 3_600))
        .unwrap();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    // The automatic attempt hit a transient failure and kept the record.
    wait_for(&mut visual, &view, |v| v.auth.auto_retry);
    // The box is checked for the kept record; uncheck it before signing in.
    toggle(&mut visual, "auth-remember-login");
    type_credentials(&mut visual, &view, "neva", "password123");
    wait_for(&mut visual, &view, |v| v.session.is_some());
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap().is_empty()
    });
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_failed_save_is_visible_in_the_workspace(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &login_body(7, "neva", "token-manual")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("save-error");
    // The session path is a directory, so the atomic rename cannot replace it.
    let (window, view) = super::add_auth_window_with(
        cx,
        &server.base,
        dir.join("workspace-layout.json"),
        dir.join("instance.json"),
        dir.clone(),
    );
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    toggle(&mut visual, "auth-remember-login");
    type_credentials(&mut visual, &view, "neva", "password123");
    wait_for(&mut visual, &view, |v| v.session.is_some());
    wait_for(&mut visual, &view, |v| !v.remember_error.is_empty());
    draw(&mut visual);
    assert!(
        visual.debug_bounds("remember-error").is_some(),
        "the failed save is a visible notice"
    );
    view.read_with(&*visual, |v, _| {
        assert!(v.client.is_some(), "the session itself is unaffected");
    });
    std::fs::remove_dir_all(dir).unwrap();
}

fn install_session(view: &mut Voxhold, base: &str, token: &str) {
    view.client = Some(Client::new(base).expect("origin"));
    view.session = Some(
        Session::new(
            base.into(),
            7,
            "neva".into(),
            token.into(),
            now_unix() + 3_600,
        )
        .expect("session"),
    );
    view.session_epoch = view.session_epoch.wrapping_add(1);
    view.last_stored_token = Some(token.into());
}

#[gpui::test]
async fn rotation_updates_the_record_and_the_next_one_catches_up(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("rotation");
    let store = store(&dir);
    store
        .save(&record(&server.base, "neva", 7, "token-1", 3_600))
        .unwrap();
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &server.base, "token-1");
            let epoch = v.session_epoch;
            let generation = v.session.as_ref().expect("session").generation();
            v.finish_refresh(
                epoch,
                generation,
                Ok(SessionUpdate {
                    token: "token-2".into(),
                    expires_at: now_unix() + 7_200,
                }),
                window,
                cx,
            );
        });
    });
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap()[0].token == "token-2"
    });

    // A second rotation keeps the file current.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let epoch = v.session_epoch;
            let generation = v.session.as_ref().expect("session").generation();
            v.finish_refresh(
                epoch,
                generation,
                Ok(SessionUpdate {
                    token: "token-3".into(),
                    expires_at: now_unix() + 7_200,
                }),
                window,
                cx,
            );
        });
    });
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap()[0].token == "token-3"
    });
    assert_eq!(
        view.read_with(&*visual, |v, _| v.last_stored_token.clone()),
        Some("token-3".into())
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn logout_deletes_the_record_even_without_the_server(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(Vec::new());
    let dir = scratch("logout");
    let store = store(&dir);
    store
        .save(&record(&server.base, "neva", 7, "token-1", 3_600))
        .unwrap();
    let (window, view) = open(cx, &server.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &server.base, "token-1");
            v.logout(window, cx);
        });
    });
    wait_store(&mut visual, &store, |store| {
        store.load_all().unwrap().is_empty()
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none());
        assert_eq!(v.auth.screen, auth::AuthScreen::Login);
        assert!(!v.auth.remember_login);
    });
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn closing_the_window_keeps_the_remembered_login(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("drop");
    let store = store(&dir);
    let saved = record("http://127.0.0.1:1", "neva", 7, "token-1", 3_600);
    store.save(&saved).unwrap();
    let (window, _view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    assert_eq!(store.load_all().unwrap(), vec![saved]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_damaged_blob_falls_back_to_the_password_and_can_be_repaired(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, &login_body(7, "neva", "token-manual")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("damaged");
    let store = store(&dir);
    std::fs::write(dir.join("sessions.bin"), b"VXSS1 damaged blob").unwrap();
    assert!(store.load_all().is_err());
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, &server.base);
    settled_auto(&mut visual, &view);
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none());
        assert!(!v.auth.remember_login);
    });

    toggle(&mut visual, "auth-remember-login");
    type_credentials(&mut visual, &view, "neva", "password123");
    wait_for(&mut visual, &view, |v| v.session.is_some());
    wait_store(&mut visual, &store, |store| {
        store
            .load_for_origin(&server.base)
            .unwrap()
            .is_some_and(|record| record.token == "token-manual")
    });
    std::fs::remove_dir_all(dir).unwrap();
}

/// Two instances on different ports: the first Bearer never reaches the second
/// over HTTP or the WS handshake, in either direction.
#[gpui::test]
async fn two_origins_never_exchange_their_bearer(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let first = TestServer::spawn(vec![
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let second = TestServer::spawn(vec![
        Reply::new(200, &login_body(7, "neva", "token-second")),
        Reply::new(200, servers_body()),
    ]);
    assert_ne!(first.base, second.base, "two different ports");
    let dir = scratch("two-origins");
    let store = store(&dir);
    store
        .save(&record(&first.base, "neva", 7, "token-first", 3_600))
        .unwrap();
    let (window, view) = open(cx, &second.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    // The confirmed origin is the second server; it has no record.
    confirm(&mut visual, &view, &second.base);
    settled_auto(&mut visual, &view);
    type_credentials(&mut visual, &view, "neva", "password123");
    wait_for(&mut visual, &view, |v| v.session.is_some());
    assert_eq!(
        view.read_with(&*visual, |v, _| v
            .session
            .as_ref()
            .map(|s| s.token().to_owned())),
        Some("token-second".into())
    );
    assert!(
        first.requests().is_empty(),
        "the first origin was never confirmed"
    );

    // Switch to the first origin: the stored token signs in there.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.logout(window, cx));
    });
    visual.run_until_parked();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.back_to_instance(window, cx));
    });
    visual.run_until_parked();
    confirm(&mut visual, &view, &first.base);
    wait_for(&mut visual, &view, |v| v.session.is_some());
    assert_eq!(
        view.read_with(&*visual, |v, _| v
            .session
            .as_ref()
            .map(|s| s.token().to_owned())),
        Some("token-first".into())
    );

    for (server, own, other) in [
        (&first, "token-first", "token-second"),
        (&second, "token-second", "token-first"),
    ] {
        let own = format!("Bearer {own}");
        let other = format!("Bearer {other}");
        for request in server.requests() {
            let authorization = request.header("authorization");
            assert_ne!(
                authorization,
                Some(other.as_str()),
                "{} received the other origin's Bearer on {}",
                server.base,
                request.path
            );
            if request.path == "/api/v1/ws" {
                assert!(
                    authorization.is_none(),
                    "the WS handshake must not carry a Bearer"
                );
            }
            assert!(
                !request
                    .body
                    .windows(other.len())
                    .any(|window| window == other.as_bytes()),
                "{} received the other origin's Bearer in a body",
                server.base
            );
        }
        let authenticated = server
            .requests()
            .iter()
            .filter(|request| request.header("authorization") == Some(own.as_str()))
            .count();
        assert!(
            authenticated > 0,
            "{} saw its own token at least once",
            server.base
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn the_login_checkbox_is_painted_on_the_password_label_row(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("geometry");
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    confirm(&mut visual, &view, "http://127.0.0.1:1");
    settled_auto(&mut visual, &view);
    for (width, height) in [(320.0, 360.0), (390.0, 760.0), (1280.0, 900.0)] {
        visual.simulate_resize(size(px(width), px(height)));
        draw(&mut visual);
        let (vw, vh) = view.read_with(&*visual, |v, _| v.viewport);
        let layout = auth::layout_auth(vw, vh, false, false);
        let bounds = visual
            .debug_bounds("auth-remember-login")
            .unwrap_or_else(|| panic!("{width}x{height}: the box is painted"));
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        assert!(
            (x - layout.remember_login.x).abs() < 1.0
                && (y - layout.remember_login.y).abs() < 1.0
                && (w - layout.remember_login.w).abs() < 1.0
                && (h - layout.remember_login.h).abs() < 1.0,
            "{width}x{height}: painted ({x},{y},{w},{h}) vs {:?}",
            layout.remember_login
        );
        assert!(y >= layout.password_label.y - 1.0);
        assert!(y + h <= layout.password.y + 1.0);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
