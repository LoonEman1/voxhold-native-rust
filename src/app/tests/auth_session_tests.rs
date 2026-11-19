//! Transport contracts for the session: login, refresh, logout and the `401`
//! report that the application reads through the shared client slot.

use crate::test_support::{Gate, Reply, TestServer};
use std::time::Duration;
use voxhold_api::{Client, Error};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

#[test]
fn login_reads_user_and_session_from_the_backend_shape() {
    let server = TestServer::spawn(vec![Reply::new(
        200,
        r#"{"user":{"id":7,"username":"neva","created_at":1700000000},
            "session":{"token":"token-1","expires_at":1700003600}}"#,
    )]);
    let mut client = Client::new(&server.base).expect("origin");
    assert_eq!(client.origin(), server.base);

    let login = runtime()
        .block_on(client.login("neva", "password123"))
        .expect("login");
    assert_eq!(login.user_id, 7);
    assert_eq!(login.username, "neva");
    assert_eq!(login.token, "token-1");
    assert_eq!(login.expires_at, 1_700_003_600);
    assert_eq!(client.token(), "token-1");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/auth/login");
}

#[test]
fn a_login_answer_without_a_session_does_not_install_a_token() {
    for body in [
        r#"{"user":{"id":7,"username":"neva"},"session":{}}"#,
        r#"{"user":{"id":7,"username":"neva"},"session":{"expires_at":1700003600}}"#,
        r#"{"user":{"id":0,"username":"neva"},"session":{"token":"t","expires_at":1700003600}}"#,
        r#"{"user":{"id":7,"username":"neva"},"session":{"token":"t","expires_at":0}}"#,
    ] {
        let server = TestServer::spawn(vec![Reply::new(200, body)]);
        let mut client = Client::new(&server.base).expect("origin");
        let result = runtime().block_on(client.login("neva", "password123"));
        assert!(matches!(result, Err(Error::InvalidResponse(_))), "{body}");
        assert_eq!(client.token(), "");
    }
}

#[test]
fn refresh_parses_the_top_level_pair_without_mutating_the_client() {
    let server = TestServer::spawn(vec![Reply::new(
        200,
        r#"{"token":"token-2","expires_at":1700007200}"#,
    )]);
    let mut client = Client::new(&server.base).expect("origin");
    client.set_token("token-1".into());

    let update = runtime().block_on(client.refresh()).expect("refresh");
    assert_eq!(update.token, "token-2");
    assert_eq!(update.expires_at, 1_700_007_200);
    // The owner applies the pair only after checking the generation.
    assert_eq!(client.token(), "token-1");

    let requests = server.requests();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/auth/refresh");
    assert_eq!(requests[0].header("authorization"), Some("Bearer token-1"));
}

#[test]
fn a_refresh_answer_without_a_token_or_expiry_is_a_format_error() {
    for body in [
        r#"{"expires_at":1700007200}"#,
        r#"{"token":"","expires_at":1700007200}"#,
        r#"{"token":"token-2"}"#,
        r#"{"token":"token-2","expires_at":0}"#,
    ] {
        let server = TestServer::spawn(vec![Reply::new(200, body)]);
        let mut client = Client::new(&server.base).expect("origin");
        client.set_token("token-1".into());
        let result = runtime().block_on(client.refresh());
        assert!(matches!(result, Err(Error::InvalidResponse(_))), "{body}");
        assert_eq!(client.token(), "token-1");
    }
}

#[test]
fn a_401_reports_the_token_that_was_actually_sent_to_every_clone() {
    let server = TestServer::spawn(vec![Reply::new(401, "{}"), Reply::new(401, "{}")]);
    let mut client = Client::new(&server.base).expect("origin");
    client.set_token("token-old".into());
    let clone = client.clone();
    let rt = runtime();

    assert!(matches!(
        rt.block_on(clone.servers()),
        Err(Error::Http(401))
    ));
    assert_eq!(clone.take_unauthorized().as_deref(), Some("token-old"));
    assert_eq!(client.take_unauthorized(), None, "the slot is shared once");

    // A request that starts after the rotation names the new token.
    client.set_token("token-new".into());
    assert!(matches!(
        rt.block_on(clone.servers()),
        Err(Error::Http(401))
    ));
    assert_eq!(client.take_unauthorized().as_deref(), Some("token-new"));
}

#[test]
fn both_refusals_are_queued_in_arrival_order() {
    let server = TestServer::spawn(vec![Reply::new(401, "{}"), Reply::new(401, "{}")]);
    let mut client = Client::new(&server.base).expect("origin");
    let rt = runtime();
    client.set_token("token-old".into());
    assert!(matches!(
        rt.block_on(client.servers()),
        Err(Error::Http(401))
    ));
    // The rotation happens between the two refusals, exactly like a request
    // that was already on the wire when the new token arrived.
    client.set_token("token-new".into());
    assert!(matches!(
        rt.block_on(client.servers()),
        Err(Error::Http(401))
    ));
    assert_eq!(client.take_unauthorized().as_deref(), Some("token-old"));
    assert_eq!(client.take_unauthorized().as_deref(), Some("token-new"));
    assert_eq!(client.take_unauthorized(), None);
}

#[test]
fn a_late_401_from_the_old_token_keeps_the_new_one() {
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn(vec![Reply::new(401, "{}").with_gate(gate.clone())]);
    let mut client = Client::new(&server.base).expect("origin");
    client.set_token("token-old".into());

    let request_client = client.clone();
    let worker = std::thread::spawn(move || runtime().block_on(request_client.servers()));
    assert!(
        gate.wait_arrived(Duration::from_secs(5)),
        "request reached the server"
    );
    // The rotation lands while the old request is still on the wire.
    client.set_token("token-new".into());
    gate.release();
    assert!(matches!(worker.join().unwrap(), Err(Error::Http(401))));

    // The answer names the old token; the application must not clear the new
    // session because the token no longer matches.
    assert_eq!(client.take_unauthorized().as_deref(), Some("token-old"));
    assert_eq!(client.token(), "token-new");
}

#[test]
fn logout_posts_the_bearer_and_accepts_no_content() {
    let server = TestServer::spawn(vec![Reply::new(204, "")]);
    let mut client = Client::new(&server.base).expect("origin");
    client.set_token("token-1".into());

    runtime().block_on(client.logout()).expect("logout");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/auth/logout");
    assert_eq!(requests[0].header("authorization"), Some("Bearer token-1"));
}

#[cfg(feature = "voxhold-media")]
#[gpui::test]
async fn logout_with_an_active_call_clears_the_session_and_posts_once(
    cx: &mut gpui::TestAppContext,
) {
    use crate::auth::AuthScreen;
    use crate::auth_session::{now_unix, Session};

    let server = TestServer::spawn(vec![Reply::new(204, "")]);
    let base = server.base.clone();
    let (mut visual, view, _voice) = super::super::voice_tests::fixture(cx);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let mut client = Client::new(&base).expect("origin");
            client.set_token("token-1".into());
            v.client = Some(client);
            v.session = Some(
                Session::new(
                    base.clone(),
                    1,
                    "alice".into(),
                    "token-1".into(),
                    now_unix() + 3600,
                )
                .expect("session"),
            );
            v.session_epoch = v.session_epoch.wrapping_add(1);
            v.logout(window, cx);
        });
    });

    // The local end of the session is immediate.
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none());
        assert!(v.client.is_none());
        assert!(v.voice.is_none());
        assert_eq!(v.voice_channel, 0);
        assert_eq!(v.auth.screen, AuthScreen::Login);
        assert!(
            v.auth.status.is_empty(),
            "a normal logout has no failure text"
        );
    });

    // The revocation is one best-effort request in the background.
    for _ in 0..100 {
        visual.run_until_parked();
        if !server.requests().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "exactly one server logout: {requests:?}");
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/auth/logout");
    assert_eq!(requests[0].header("authorization"), Some("Bearer token-1"));

    // A second logout cannot repeat the request: the revocation was consumed.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.logout(window, cx));
    });
    for _ in 0..20 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.requests().len(), 1, "no duplicate revocation");
}
