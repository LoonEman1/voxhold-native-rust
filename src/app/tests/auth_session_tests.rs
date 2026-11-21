//! Transport contracts for the session: login, refresh, logout and the `401`
//! report that the application reads through the shared client slot.

use super::*;
use crate::auth_session::{now_unix, Plan, Session};
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use std::time::Duration;
use voxhold_api::{ChannelSubscription, Client, Error, SessionUpdate};

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
    let (visual, view, _voice) = super::super::voice_tests::fixture(cx);

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

/// A real root window with a client pointed at the test server. The session is
/// installed separately so each test controls its expiry and generation.
fn app_fixture(
    cx: &mut TestAppContext,
    base: &str,
    token: &str,
) -> (&'static mut VisualTestContext, Entity<Voxhold>) {
    cx.update(|cx| gpui_component::init(cx));
    let mut client = Client::new(base).expect("origin");
    client.set_token(token.into());
    let (window, view) = super::super::settings_tests::make_app(cx, client);
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    (visual, view)
}

fn install_session(view: &mut Voxhold, base: &str, expires_at: i64) {
    view.session = Some(
        Session::new(base.into(), 1, "alice".into(), "token-1".into(), expires_at)
            .expect("session"),
    );
    view.session_epoch = view.session_epoch.wrapping_add(1);
}

/// Queue a real `401` through the shared client exactly like an ordinary
/// request would, then return the refused token.
fn queue_401(client: &Client) -> String {
    assert!(matches!(
        runtime().block_on(client.servers()),
        Err(Error::Http(401))
    ));
    client.peek_unauthorized().expect("refused token")
}

#[gpui::test]
async fn a_current_token_401_waits_for_its_rotation(cx: &mut TestAppContext) {
    let server = TestServer::spawn(vec![Reply::new(401, "{}")]);
    let base = server.base.clone();
    let (visual, view) = app_fixture(cx, &base, "token-1");
    let probe = view.read_with(&*visual, |v, _| v.client.clone().expect("client"));
    assert_eq!(queue_401(&probe), "token-1");

    let (epoch, generation) = visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &base, now_unix() + 60);
            let epoch = v.session_epoch;
            let now = now_unix();
            let plan = v
                .session_gate
                .plan(v.session.as_ref().expect("session"), now);
            assert!(matches!(plan, Plan::Refresh { .. }), "the rotation starts");
            let generation = v.session.as_ref().expect("session").generation();

            // The timer runs `check_unauthorized` before the refresh answer
            // arrives. The event must be kept, not applied and not lost.
            assert!(
                v.check_unauthorized(window, cx),
                "the timer keeps running while the flight is open"
            );
            let session = v.session.as_ref().expect("session");
            assert!(session.is_active());
            assert_eq!(session.token(), "token-1");
            assert_eq!(
                v.client.as_ref().and_then(|c| c.peek_unauthorized()),
                Some("token-1".into()),
                "the deferred 401 stays queued"
            );
            (epoch, generation)
        })
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.finish_refresh(
                epoch,
                generation,
                Ok(SessionUpdate {
                    token: "token-2".into(),
                    expires_at: now_unix() + 3600,
                }),
                window,
                cx,
            );
            let session = v.session.as_ref().expect("session");
            assert!(session.is_active());
            assert_eq!(session.token(), "token-2");
            assert_eq!(session.generation(), 2);
            assert_eq!(v.client.as_ref().expect("client").token(), "token-2");

            // The deferred refusal names the old token now; the next drain
            // drops it instead of ending the rotated session.
            assert!(v.check_unauthorized(window, cx));
            assert!(v.session.as_ref().expect("session").is_active());
            assert_eq!(v.client.as_ref().and_then(|c| c.peek_unauthorized()), None);
        })
    });
}

#[gpui::test]
async fn a_current_token_401_without_a_flight_ends_the_session(cx: &mut TestAppContext) {
    let server = TestServer::spawn(vec![Reply::new(401, "{}")]);
    let base = server.base.clone();
    let (visual, view) = app_fixture(cx, &base, "token-1");
    let probe = view.read_with(&*visual, |v, _| v.client.clone().expect("client"));
    assert_eq!(queue_401(&probe), "token-1");

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &base, now_unix() + 3600);
            assert!(
                !v.check_unauthorized(window, cx),
                "the clock stops with the session"
            );
            assert!(v.session.is_none());
            assert!(v.client.is_none());
            assert_eq!(v.auth.screen, crate::auth::AuthScreen::Login);
            assert!(v.auth.status.contains("Сессия истекла"));
        })
    });
}

#[gpui::test]
async fn a_current_token_401_with_a_refused_refresh_ends_the_session(cx: &mut TestAppContext) {
    let server = TestServer::spawn(vec![Reply::new(401, "{}")]);
    let base = server.base.clone();
    let (visual, view) = app_fixture(cx, &base, "token-1");
    let probe = view.read_with(&*visual, |v, _| v.client.clone().expect("client"));
    assert_eq!(queue_401(&probe), "token-1");

    let (epoch, generation) = visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &base, now_unix() + 60);
            let epoch = v.session_epoch;
            let now = now_unix();
            assert!(matches!(
                v.session_gate
                    .plan(v.session.as_ref().expect("session"), now),
                Plan::Refresh { .. }
            ));
            assert!(v.check_unauthorized(window, cx));
            (epoch, v.session.as_ref().expect("session").generation())
        })
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.finish_refresh(epoch, generation, Err(Error::Http(401)), window, cx);
            assert!(v.session.is_none(), "a refused refresh ends the session");
            assert!(v.client.is_none());
            assert!(v.auth.status.contains("Сессия истекла"));
            // The deferred 401 can no longer touch a future login.
            assert!(v.check_unauthorized(window, cx));
            assert!(v.session.is_none());
        })
    });
}

#[gpui::test]
async fn a_rotation_restarts_the_account_socket_and_the_channel_subscription(
    cx: &mut TestAppContext,
) {
    // The account socket is allowed to fail against the test server; the test
    // observes the restart and the subscription value, not a real stream.
    let server = TestServer::spawn(Vec::new());
    let base = server.base.clone();
    let (visual, view) = app_fixture(cx, &base, "token-1");

    let (epoch, generation, realtime_before, access_generation) = visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, &base, now_unix() + 60);
            v.channels = vec![voxhold_api::Channel {
                id: 9,
                name: "general".into(),
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            }];
            v.channel = 9;
            v.conversation = None;
            v.start_realtime(v.client.clone().expect("client"), window, cx);
            (
                v.session_epoch,
                v.session.as_ref().expect("session").generation(),
                v.realtime_generation,
                v.access_generation,
            )
        })
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.finish_refresh(
                epoch,
                generation,
                Ok(SessionUpdate {
                    token: "token-2".into(),
                    expires_at: now_unix() + 3600,
                }),
                window,
                cx,
            );
            assert_eq!(
                v.realtime_generation,
                realtime_before.wrapping_add(1),
                "the account socket is recreated with the new credential"
            );
            assert!(v.event_task.is_some());
            assert!(v.events.is_some());
            let subscription = *v
                .event_subscription
                .as_ref()
                .expect("subscription channel")
                .borrow();
            assert_eq!(
                subscription,
                Some(ChannelSubscription {
                    server_id: 1,
                    channel_id: 9,
                    generation: access_generation,
                }),
                "the open channel subscription is restored"
            );
        })
    });
}

#[gpui::test]
async fn a_late_refresh_after_logout_or_a_new_login_is_ignored(cx: &mut TestAppContext) {
    let base = "http://127.0.0.1:1";
    let (visual, view) = app_fixture(cx, base, "token-1");
    let (old_epoch, generation) = visual.update(|window, app| {
        view.update(app, |v, cx| {
            install_session(v, base, now_unix() + 3600);
            let _ = window;
            let _ = cx;
            (
                v.session_epoch,
                v.session.as_ref().expect("session").generation(),
            )
        })
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| v.logout(window, cx));
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.finish_refresh(
                old_epoch,
                generation,
                Ok(SessionUpdate {
                    token: "token-2".into(),
                    expires_at: now_unix() + 7200,
                }),
                window,
                cx,
            );
            assert!(v.session.is_none(), "a logged-out session stays closed");
            assert!(v.client.is_none());
        })
    });

    // A new login owns the state: the same late answer cannot replace it.
    let new_epoch = visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new(base).expect("origin"));
            v.session = Some(
                Session::new(
                    base.into(),
                    2,
                    "bob".into(),
                    "token-new".into(),
                    now_unix() + 3600,
                )
                .expect("session"),
            );
            v.session_epoch = v.session_epoch.wrapping_add(1);
            let _ = cx;
            v.session_epoch
        })
    });
    assert_ne!(new_epoch, old_epoch);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.finish_refresh(
                old_epoch,
                generation,
                Ok(SessionUpdate {
                    token: "token-stale".into(),
                    expires_at: now_unix() + 7200,
                }),
                window,
                cx,
            );
            let session = v.session.as_ref().expect("new session");
            assert_eq!(session.token(), "token-new");
            assert_eq!(session.user_id(), 2);
        })
    });
}

#[gpui::test]
async fn an_invalid_refresh_answer_ends_the_session(cx: &mut TestAppContext) {
    let base = "http://127.0.0.1:1";
    let (visual, view) = app_fixture(cx, base, "token-1");
    let (epoch, generation) = visual.update(|_window, app| {
        view.update(app, |v, cx| {
            install_session(v, base, now_unix() + 60);
            let now = now_unix();
            assert!(matches!(
                v.session_gate
                    .plan(v.session.as_ref().expect("session"), now),
                Plan::Refresh { .. }
            ));
            let _ = cx;
            (
                v.session_epoch,
                v.session.as_ref().expect("session").generation(),
            )
        })
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.finish_refresh(
                epoch,
                generation,
                Err(Error::InvalidResponse("bad")),
                window,
                cx,
            );
            assert!(
                v.session.is_none(),
                "a malformed refresh answer is terminal, not a retry"
            );
            assert!(v.auth.status.contains("Сессия истекла"));
        })
    });
}

#[gpui::test]
async fn a_transient_refresh_failure_keeps_the_session_for_a_retry(cx: &mut TestAppContext) {
    let base = "http://127.0.0.1:1";
    let (visual, view) = app_fixture(cx, base, "token-1");
    let (epoch, generation, now) = visual.update(|_window, app| {
        view.update(app, |v, cx| {
            install_session(v, base, now_unix() + 60);
            let now = now_unix();
            assert!(matches!(
                v.session_gate
                    .plan(v.session.as_ref().expect("session"), now),
                Plan::Refresh { .. }
            ));
            let _ = cx;
            (
                v.session_epoch,
                v.session.as_ref().expect("session").generation(),
                now,
            )
        })
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.finish_refresh(epoch, generation, Err(Error::Network), window, cx);
            let session = v.session.as_ref().expect("session survives");
            assert!(session.is_active());
            assert_eq!(session.token(), "token-1");
            assert_eq!(
                v.session_gate.plan(session, now),
                Plan::Waiting,
                "the bounded retry is scheduled"
            );
        })
    });
}

#[cfg(feature = "voxhold-media")]
#[gpui::test]
async fn a_rotated_token_keeps_the_voice_handle_and_does_not_rejoin(cx: &mut TestAppContext) {
    use crate::auth_session::Session;
    use voxhold_media::publisher::{Phase, Sharing};

    let (visual, view, voice) = super::super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                ..Default::default()
            });
            // `start_refresh` announces the rotation before the request runs.
            panel.begin_token_rotation();
        });
    });
    let attempt = voice.read_with(&*visual, |panel, _| {
        panel
            .test_handle_attempt()
            .expect("the fixture attached one handle")
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            // No client object is needed to prove the session path: the panel
            // still receives the rotated-token notification, and no real
            // WebSocket is opened by this test.
            v.session = Some(
                Session::new(
                    "http://127.0.0.1:1".into(),
                    1,
                    "alice".into(),
                    "token-1".into(),
                    now_unix() + 3600,
                )
                .expect("session"),
            );
            v.session_epoch = v.session_epoch.wrapping_add(1);
            v.client = None;
            v.apply_rotated_token(window, cx);
        });
    });

    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_handle_attempt(),
            Some(attempt),
            "the rotation must keep the one media worker"
        );
        assert_eq!(
            panel.call_channel(),
            7,
            "the call is not left and re-joined"
        );
        assert!(
            panel.test_rotation_active(),
            "the worker now owns the one deferred authorization"
        );
        assert_eq!(
            panel.test_screen_phase(),
            Phase::Live,
            "the confirmed broadcast is kept for the worker's recovery"
        );
    });
}

#[cfg(feature = "voxhold-media")]
#[gpui::test]
async fn a_call_joined_during_a_refresh_defers_its_first_authorization(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            // The refresh starts while the user is not in a call, so no handle
            // carried the gate.
            panel.leave_call(window, cx);
            panel.begin_token_rotation();
            // The user joins before the answer arrives. The new handle must
            // inherit the panel-level latch.
            panel.test_join_detached(window, cx);
            assert_eq!(panel.call_channel(), 7);
            assert!(panel.test_handle_attempt().is_some());
            assert!(
                panel.test_rotation_active(),
                "the new handle inherits the panel-level latch"
            );
            assert!(
                !panel.test_rotation_authorizes(),
                "the first authorization waits for the refresh answer"
            );
        });
    });

    visual.update(|_window, app| {
        voice.update(app, |panel, _| panel.token_rotated());
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_rotation_authorizes(),
            "the answered refresh releases the deferred authorization"
        );
    });

    // A call joined after the answer has no reason to wait.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.leave_call(window, cx);
            panel.test_join_detached(window, cx);
        });
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_rotation_authorizes(),
            "a later join authorizes immediately"
        );
        assert!(
            !panel.test_rotation_active(),
            "the completed rotation does not own the new join"
        );
    });
}
