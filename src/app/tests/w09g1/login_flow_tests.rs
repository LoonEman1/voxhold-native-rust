//! A02-A05: the two checkboxes, the legacy migration, the exact `-server`
//! target and the expiry during the startup answer.

use super::*;

/// Close one window the way the lifecycle does, and stop its account socket
/// explicitly. `remove_window` alone leaves the entity alive in the test
/// harness, so its reconnect loop could still open a socket seconds later; the
/// product's `Drop` calls the same method of the same entity.
fn close_window(visual: &mut VisualTestContext, view: &Entity<Voxhold>) {
    visual.update(|window, app| {
        view.update(app, |v, _| v.stop_realtime());
        window.remove_window();
    });
    visual.run_until_parked();
}

/// Let the requests of the previous window finish before a test snapshots a
/// counter: the realtime handshake can land after the window is dropped.
fn settle_requests(visual: &mut VisualTestContext, servers: &[&TestServer]) {
    let mut last: Vec<usize> = servers.iter().map(|s| s.requests().len()).collect();
    let mut stable = 0;
    for _ in 0..80 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(25));
        let now: Vec<usize> = servers.iter().map(|s| s.requests().len()).collect();
        if now == last {
            stable += 1;
            if stable >= 8 {
                return;
            }
        } else {
            last = now;
            stable = 0;
        }
    }
}

/// A02: all four checkbox combinations decide the next start, and an unchecked
/// manual login removes the previous account.
#[gpui::test]
async fn a02_the_two_checkboxes_decide_the_next_start(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    for (address, login) in [(false, false), (false, true), (true, false), (true, true)] {
        let server = TestServer::spawn_concurrent_routed(vec![
            Reply::new(200, &login_body(7, "neva", "token-manual")).with_path("/api/v1/auth/login"),
            Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
            Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
            Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
            Reply::new(200, &profile_body(7, "neva")).with_path("/api/v1/me/profile"),
        ]);
        let dir = scratch("a02");
        let session_store = store(&dir);
        let (window, view) = open(cx, &server.base, &dir);
        let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        visual.run_until_parked();
        settled_startup(&mut visual, &view);
        if address {
            click_selector(&mut visual, "auth-remember-instance");
        }
        confirm(&mut visual, &view, &server.base);
        if login {
            click_selector(&mut visual, "auth-remember-login");
        }
        type_credentials(&mut visual, &view, "neva", "password123");
        wait_for(&mut visual, &view, |v| v.session.is_some());
        wait_store(&mut visual, &session_store, |store| {
            if login {
                store.state().unwrap().target.is_some()
            } else {
                store.state().unwrap().target.is_none()
            }
        });
        let pref = std::fs::read_to_string(dir.join("instance.json")).unwrap();
        if address {
            assert!(pref.contains(&server.base), "{pref}");
        } else {
            assert_eq!(pref, "{\"remember_instance\":false}");
        }
        visual.update(|window, _app| window.remove_window());
        visual.run_until_parked();
        drop(view);

        // A new process with the same files. With a remembered login the
        // workspace opens without any manual action; otherwise the address
        // form appears and is prefilled only by the separate address box.
        let (window, view) = open(cx, "", &dir);
        let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        visual.run_until_parked();
        if login {
            wait_for(&mut visual, &view, |v| v.session.is_some());
            draw(&mut visual);
            assert_no_form_painted(&mut visual);
        } else {
            settled_startup(&mut visual, &view);
            draw(&mut visual);
            assert!(view.read_with(&*visual, |v, _| v.session.is_none()));
            assert!(visual.debug_bounds("auth-instance-field").is_some());
            let value = view.read_with(&*visual, |v, cx| v.instance.read(cx).value().to_string());
            if address {
                assert_eq!(value, server.base);
            } else {
                assert_eq!(value, "");
            }
        }
        visual.update(|window, _app| window.remove_window());
        visual.run_until_parked();
        drop(view);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[gpui::test]
async fn a02_an_unchecked_login_does_not_reuse_the_previous_account(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server_a = TestServer::spawn_routed(vec![
        Reply::new(200, &login_body(7, "neva", "token-a")).with_path("/api/v1/auth/login"),
        Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
        Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
        Reply::new(200, &profile_body(7, "neva")).with_path("/api/v1/me/profile"),
    ]);
    let server_b = TestServer::spawn_routed(vec![
        Reply::new(200, &login_body(9, "bob", "token-b")).with_path("/api/v1/auth/login"),
        Reply::new(200, servers_body()).with_path("/api/v1/me/servers"),
    ]);
    let dir = scratch("a02-previous");
    let session_store = store(&dir);

    // Run 1: remember neva.
    let (window, view) = open(cx, &server_a.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);
    confirm(&mut visual, &view, &server_a.base);
    click_selector(&mut visual, "auth-remember-login");
    type_credentials(&mut visual, &view, "neva", "password123");
    wait_for(&mut visual, &view, |v| v.session.is_some());
    wait_store(&mut visual, &session_store, |store| {
        store.state().unwrap().target.is_some()
    });
    close_window(&mut visual, &view);
    drop(view);

    // Run 2: cancel the automatic restoration and sign in as bob without the
    // box. The previous target must not be used by run 3.
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| v.cancel_startup_restore(window, cx));
    });
    confirm(&mut visual, &view, &server_b.base);
    type_credentials(&mut visual, &view, "bob", "password123");
    wait_for(&mut visual, &view, |v| {
        v.session.as_ref().is_some_and(|s| s.user_id() == 9)
    });
    wait_store(&mut visual, &session_store, |store| {
        store.state().unwrap().target.is_none()
    });
    close_window(&mut visual, &view);
    drop(view);

    // Run 3: no hidden restoration of neva, no request at all. The window of
    // run 2 is already stopped, but its realtime handshake may still land, so
    // both counters must be quiet before they are snapshotted.
    settle_requests(&mut visual, &[&server_a, &server_b]);
    let a_before = server_a.requests().len();
    let b_before = server_b.requests().len();
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);
    assert!(view.read_with(&*visual, |v, _| v.session.is_none()));
    assert_eq!(server_a.requests().len(), a_before);
    let late: Vec<String> = server_b
        .requests()
        .into_iter()
        .skip(b_before)
        .map(|r| r.path)
        .collect();
    assert_eq!(
        late,
        Vec::<String>::new(),
        "the last window sent no request"
    );
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A03: one unambiguous legacy record migrates after the successful restore;
/// two suitable records open the form without any Bearer request.
#[gpui::test]
async fn a03_a_legacy_store_migrates_only_an_unambiguous_target(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    // Ambiguous: two records for the same origin but different accounts.
    let server = TestServer::spawn_routed(Vec::new());
    let dir = scratch("a03-ambiguous");
    let path = dir.join("sessions.bin");
    crate::session_store::write_legacy_file(
        &path,
        &[
            record(&server.base, "neva", 7, "token-1", 3_600),
            record(&server.base, "bob", 9, "token-2", 3_600),
        ],
    );
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);
    draw(&mut visual);
    assert!(
        visual.debug_bounds("auth-instance-field").is_some(),
        "the ambiguous legacy store opens the address form"
    );
    assert!(server.requests().is_empty(), "no token request is sent");
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);
    std::fs::remove_dir_all(dir).unwrap();

    // Exact address: one record migrates and its target is written.
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("a03-migrate");
    InstancePrefs {
        remember: true,
        instance: Some(server.base.clone()),
    }
    .save(&dir.join("instance.json"))
    .unwrap();
    crate::session_store::write_legacy_file(
        &dir.join("sessions.bin"),
        &[record(&server.base, "neva", 7, "token-legacy", 3_600)],
    );
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    wait_for(&mut visual, &view, |v| v.session.is_some());
    let session_store = store(&dir);
    wait_store(&mut visual, &session_store, |store| {
        store
            .state()
            .map(|state| !state.legacy && state.target.is_some())
            .unwrap_or(false)
    });
    let state = session_store.state().unwrap();
    assert!(!state.legacy);
    assert_eq!(
        state.target.as_ref().map(|target| target.origin.clone()),
        Some(server.base.clone())
    );
    std::fs::remove_dir_all(dir).unwrap();

    // An explicit «no target» in the new format never searches records.
    let dir = scratch("a03-cleared");
    let saved = record("https://chat.example.org", "neva", 7, "token-1", 3_600);
    crate::session_store::write_state_file_for_test(
        &dir.join("sessions.bin"),
        &StoreState {
            records: vec![saved],
            target: None,
            legacy: false,
            generation: 4,
            records_generation: 4,
            restore_blocked: false,
        },
    );
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);
    assert!(view.read_with(&*visual, |v, _| v.session.is_none()));
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A04: `-server` is an exact target, never a hint that moves the Bearer to a
/// different port, scheme or garbage address.
#[gpui::test]
async fn a04_server_flag_only_allows_the_exact_origin(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let first = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")),
        Reply::new(200, servers_body()),
    ]);
    let second = TestServer::spawn_routed(Vec::new());
    assert_ne!(first.base, second.base);
    let dir = scratch("a04");
    save(
        &store(&dir),
        &record(&first.base, "neva", 7, "token-first", 3_600),
    );

    // Same canonical origin: the target is restored.
    let (window, view) = open(cx, &first.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    wait_for(&mut visual, &view, |v| v.session.is_some());
    let first_contacted = first.requests().len();
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);

    // Another port: manual form, and the other server sees no Bearer at all.
    let (window, view) = open(cx, &second.base, &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    settled_startup(&mut visual, &view);
    draw(&mut visual);
    assert!(visual.debug_bounds("auth-instance-field").is_some());
    assert!(view.read_with(&*visual, |v, _| v.session.is_none()));
    assert!(second.requests().is_empty());
    assert_eq!(
        view.read_with(&*visual, |v, cx| v.instance.read(cx).value().to_string()),
        second.base
    );
    visual.update(|window, _app| window.remove_window());
    visual.run_until_parked();
    drop(view);

    // Another scheme and an invalid address also stay manual.
    for prefill in [
        format!("https://{}", &first.base["http://".len()..]),
        "not a url".to_string(),
    ] {
        let (window, view) = open(cx, &prefill, &dir);
        let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        visual.run_until_parked();
        settled_startup(&mut visual, &view);
        draw(&mut visual);
        assert!(view.read_with(&*visual, |v, _| v.session.is_none()));
        assert!(visual.debug_bounds("auth-instance-field").is_some());
        visual.update(|window, _app| window.remove_window());
        visual.run_until_parked();
        drop(view);
    }
    // The stored origin was only contacted by the first run (profile+servers).
    // Later mismatches add no request at all.
    assert_eq!(
        first.requests().len(),
        first_contacted,
        "a mismatching -server must not contact the stored origin"
    );
    let profiles = first
        .requests()
        .iter()
        .filter(|request| request.path == "/api/v1/me/profile")
        .count();
    assert_eq!(profiles, 1);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A05: an expiry that happens while the answer is on the wire deletes the
/// record and its target and opens the manual login for that instance.
#[gpui::test]
async fn a05_expiry_during_the_flight_deletes_the_record_and_target(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn_routed(vec![
        Reply::new(200, &profile_body(7, "neva")).with_delay(Duration::from_millis(4_000)),
        Reply::new(200, servers_body()),
    ]);
    let dir = scratch("a05");
    let session_store = store(&dir);
    let saved = record(&server.base, "neva", 7, "token-expiring", 3);
    save(&session_store, &saved);
    let (window, view) = open(cx, "", &dir);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    // The held answer is older than the stored expiry by the time it lands.
    wait_for(&mut visual, &view, |v| {
        !v.auth.auto_login && !v.startup_restoring
    });
    wait_for(&mut visual, &view, |v| {
        !v.auth.auto_login && v.auth.status.contains("Сессия истекла")
    });
    let state = session_store.state().unwrap();
    assert!(state.records.is_empty());
    assert!(state.target.is_none());
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none());
        assert_eq!(v.auth.screen, auth::AuthScreen::Login);
    });
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.path != "/api/v1/me/servers"),
        "the expired token is not used for the second request"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
