//! CF-04: one forward dialog keeps one idempotency key per destination, so a
//! retry after a server error repeats the same logical send instead of creating
//! a second message. A new dialog starts with fresh keys.

use super::super::*;
use super::{add_auth_window, focus_field, message_json};
use crate::test_support::{Reply, TestServer};
use gpui::{Entity, TestAppContext, VisualTestContext};
use std::time::Duration;

const FORWARD_DESTINATIONS: &str = "/api/v1/me/forward-destinations";
const FORWARD: &str = "/api/v1/me/forward";

fn wait_entity<T: 'static>(
    visual: &mut VisualTestContext,
    entity: &Entity<T>,
    mut done: impl FnMut(&T) -> bool,
) {
    for _ in 0..600 {
        visual.run_until_parked();
        if entity.read_with(&*visual, |value, _| done(value)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the asynchronous operation did not finish");
}

fn destination(kind: &str, id: i64) -> voxhold_api::ForwardDestination {
    voxhold_api::ForwardDestination {
        kind: kind.into(),
        id,
        name: format!("{kind} {id}"),
        server_id: if kind == "conversation" { 0 } else { 1 },
    }
}

fn destinations_json(values: &[voxhold_api::ForwardDestination]) -> String {
    serde_json::to_string(values).unwrap()
}

fn install_channel_source(v: &mut Voxhold, client: Client) {
    v.client = Some(client);
    v.server = 1;
    v.channel = 7;
    v.self_user_id = 1;
    v.shell_initialized = true;
    v.show_members = false;
    v.channels = vec![Channel {
        id: 7,
        kind: "text".into(),
        permissions: 3,
        ..Default::default()
    }];
    v.messages
        .push(serde_json::from_value(message_json(42, 1, "source")).unwrap());
}

/// Open the real forward dialog and wait for its destination list, exactly
/// like the message menu does.
fn open_forward(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    message_id: i64,
    count: usize,
) {
    visual.update(|_window, app| {
        view.update(app, |v, cx| v.open_forward(message_id, cx));
    });
    wait_entity(visual, view, |v| {
        v.forward
            .as_ref()
            .is_some_and(|state| !state.loading && state.destinations.len() == count)
    });
}

fn click_destination(visual: &mut VisualTestContext, view: &Entity<Voxhold>, index: usize) {
    visual.update(|window, app| {
        view.update(app, |v, cx| v.forward_to(index, window, cx));
    });
}

fn wait_failed(visual: &mut VisualTestContext, view: &Entity<Voxhold>) {
    wait_entity(visual, view, |v| {
        v.forward
            .as_ref()
            .is_some_and(|state| !state.sending && !state.error.is_empty())
    });
}

fn forward_bodies(server: &TestServer) -> Vec<serde_json::Value> {
    server
        .requests()
        .iter()
        .filter(|request| request.method == "POST" && request.path == FORWARD)
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

fn saved_channel_key(v: &Voxhold) -> Option<String> {
    v.forward
        .as_ref()?
        .keys
        .get(&("channel".to_string(), 99))
        .cloned()
}

/// The mock answers the first POST with `504`. The user clicks the same
/// destination again: both requests must carry the same `client_message_id`.
#[gpui::test]
async fn forward_retry_after_504_reuses_the_key_for_the_same_destination(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let destinations = destinations_json(&[destination("channel", 99)]);
    let forwarded = message_json(500, 1, "forwarded").to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
        Reply::new(504, "{\"error\":\"gateway_timeout\"}"),
        Reply::new(201, &forwarded),
    ]);
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            install_channel_source(v, Client::new(&server.base).unwrap());
        });
    });
    open_forward(&mut visual, &view, 42, 1);

    click_destination(&mut visual, &view, 0);
    wait_failed(&mut visual, &view);
    click_destination(&mut visual, &view, 0);
    wait_entity(&mut visual, &view, |v| v.forward.is_none());

    let bodies = forward_bodies(&server);
    assert_eq!(bodies.len(), 2, "{:?}", server.requests());
    assert_eq!(bodies[0]["destination"]["kind"], "channel");
    assert_eq!(bodies[0]["destination"]["id"], 99);
    assert_eq!(bodies[1]["destination"]["kind"], "channel");
    assert_eq!(bodies[1]["destination"]["id"], 99);
    assert!(bodies[0]["client_message_id"]
        .as_str()
        .is_some_and(|key| !key.is_empty()));
    assert_eq!(
        bodies[0]["client_message_id"], bodies[1]["client_message_id"],
        "the retry of the same destination must repeat the idempotency key"
    );
}

/// A channel and a personal chat with the same numeric id are different
/// recipients. Each one gets its own key inside one open dialog, and a retry
/// of the first one still finds its saved key.
#[gpui::test]
async fn forward_recipients_with_the_same_numeric_id_keep_separate_keys(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let destinations =
        destinations_json(&[destination("channel", 99), destination("conversation", 99)]);
    let forwarded = message_json(501, 1, "forwarded").to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
        Reply::new(504, "{\"error\":\"gateway_timeout\"}"),
        Reply::new(504, "{\"error\":\"gateway_timeout\"}"),
        Reply::new(201, &forwarded),
    ]);
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            install_channel_source(v, Client::new(&server.base).unwrap());
        });
    });
    open_forward(&mut visual, &view, 42, 2);

    click_destination(&mut visual, &view, 0);
    wait_failed(&mut visual, &view);
    click_destination(&mut visual, &view, 1);
    wait_failed(&mut visual, &view);
    click_destination(&mut visual, &view, 0);
    wait_entity(&mut visual, &view, |v| v.forward.is_none());

    let bodies = forward_bodies(&server);
    assert_eq!(bodies.len(), 3, "{:?}", server.requests());
    assert_eq!(bodies[0]["destination"]["kind"], "channel");
    assert_eq!(bodies[0]["destination"]["id"], 99);
    assert_eq!(bodies[1]["destination"]["kind"], "conversation");
    assert_eq!(bodies[1]["destination"]["id"], 99);
    assert_eq!(bodies[2]["destination"]["kind"], "channel");
    assert_eq!(bodies[2]["destination"]["id"], 99);
    assert_ne!(
        bodies[0]["client_message_id"], bodies[1]["client_message_id"],
        "channel 99 and conversation 99 must not share a key"
    );
    assert_eq!(
        bodies[0]["client_message_id"], bodies[2]["client_message_id"],
        "the channel retry keeps the key saved for the channel"
    );
}

/// A deliberate new forward dialog of the same message to the same recipient is
/// a new operation and must get a new key, otherwise the backend would return
/// the previous forwarded row instead of sending it again.
#[gpui::test]
async fn a_new_forward_operation_gets_a_new_key_after_success(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let destinations = destinations_json(&[destination("channel", 99)]);
    let forwarded = message_json(502, 1, "forwarded").to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
        Reply::new(201, &forwarded),
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
        Reply::new(201, &forwarded),
    ]);
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            install_channel_source(v, Client::new(&server.base).unwrap());
        });
    });

    open_forward(&mut visual, &view, 42, 1);
    click_destination(&mut visual, &view, 0);
    wait_entity(&mut visual, &view, |v| v.forward.is_none());

    // The user opens forwarding again for the same row after the first success.
    open_forward(&mut visual, &view, 42, 1);
    click_destination(&mut visual, &view, 0);
    wait_entity(&mut visual, &view, |v| v.forward.is_none());

    let bodies = forward_bodies(&server);
    assert_eq!(bodies.len(), 2, "{:?}", server.requests());
    assert_eq!(bodies[0]["destination"], bodies[1]["destination"]);
    assert_ne!(
        bodies[0]["client_message_id"], bodies[1]["client_message_id"],
        "a new dialog is a new operation and must not reuse the old key"
    );
}

/// The close button, the backdrop and Escape must leave a pending forward
/// dialog (and its saved key) intact. Once the failed POST settles, the retry
/// repeats the key that was already on the wire.
#[gpui::test]
async fn a_pending_forward_dialog_cannot_be_closed_and_keeps_its_key(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let destinations = destinations_json(&[destination("channel", 99)]);
    let forwarded = message_json(510, 1, "forwarded").to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
        Reply::new(504, "{\"error\":\"gateway_timeout\"}"),
        Reply::new(201, &forwarded),
    ]);
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            install_channel_source(v, Client::new(&server.base).unwrap());
        });
    });
    open_forward(&mut visual, &view, 42, 1);
    // `forward_to` marks the dialog pending and stores the key before the POST
    // task is polled, so these close attempts happen while it is sending.
    click_destination(&mut visual, &view, 0);
    assert!(view.read_with(&*visual, |v, _| v.forward_is_sending()));

    // 1. The header close button.
    visual.update(|_window, app| {
        view.update(app, |v, cx| v.close_forward(cx));
    });
    let key = view
        .read_with(&*visual, |v, _| saved_channel_key(v))
        .expect("the close button must keep the pending dialog and its key");
    assert!(!key.is_empty());

    // 2. The backdrop dismiss.
    visual.update(|_window, app| {
        view.update(app, |v, cx| v.dismiss_overlays(cx));
    });
    view.read_with(&*visual, |v, _| {
        assert!(
            v.forward.is_some(),
            "the backdrop must not drop a pending forward"
        );
        assert!(v.forward_is_sending());
        assert_eq!(saved_channel_key(v).as_deref(), Some(key.as_str()));
    });

    // 3. Escape on the workspace. `simulate_keystrokes` dispatches the key and
    // then pumps the executor, which settles the held 504 below; the key is
    // delivered while the dialog is still sending.
    focus_field(&mut visual, &view, |v| v.composer.clone());
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    visual.simulate_keystrokes("escape");
    view.read_with(&*visual, |v, _| {
        assert!(
            v.forward.is_some(),
            "Escape must not drop a pending forward"
        );
        assert_eq!(saved_channel_key(v).as_deref(), Some(key.as_str()));
    });

    wait_failed(&mut visual, &view);
    click_destination(&mut visual, &view, 0);
    wait_entity(&mut visual, &view, |v| v.forward.is_none());

    let bodies = forward_bodies(&server);
    assert_eq!(bodies.len(), 2, "{:?}", server.requests());
    assert_eq!(bodies[0]["client_message_id"].as_str(), Some(key.as_str()));
    assert_eq!(
        bodies[0]["client_message_id"], bodies[1]["client_message_id"],
        "the blocked close must not lose the retry key"
    );
}

/// A transport failure (`Network`, no HTTP status) is retried with the same
/// key, not only an HTTP error such as `504`.
#[gpui::test]
async fn a_transport_error_keeps_the_key_for_the_retry(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let destinations = destinations_json(&[destination("channel", 99)]);
    let forwarded = message_json(511, 1, "forwarded").to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
        Reply::new(201, &forwarded),
    ]);
    let client = Client::new(&server.base).unwrap();
    visual.update(|_window, app| {
        view.update(app, |v, _| install_channel_source(v, client.clone()));
    });
    open_forward(&mut visual, &view, 42, 1);

    // Nothing listens on the loopback port, so the POST fails without a status.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new("http://127.0.0.1:1").unwrap());
            v.forward_to(0, window, cx);
        });
    });
    wait_failed(&mut visual, &view);
    let key = view.read_with(&*visual, |v, _| {
        let state = v.forward.as_ref().expect("the dialog stays open");
        assert_eq!(state.error, "Не удалось связаться с сервером.");
        saved_channel_key(v).expect("the key is saved before the failed POST")
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(client);
            v.forward_to(0, window, cx);
        });
    });
    wait_entity(&mut visual, &view, |v| v.forward.is_none());

    let bodies = forward_bodies(&server);
    assert_eq!(bodies.len(), 1, "{:?}", server.requests());
    assert_eq!(
        bodies[0]["client_message_id"].as_str(),
        Some(key.as_str()),
        "the retry after a network error repeats the saved key"
    );
}

/// A late success of the previous dialog must not close a dialog opened later
/// for the same row. The successful send still settles normally, but the new
/// dialog owns its own state.
#[gpui::test]
async fn a_late_success_does_not_close_a_new_dialog_for_the_same_message(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let destinations = destinations_json(&[destination("channel", 99)]);
    let forwarded = message_json(512, 1, "late forward").to_string();
    let server = TestServer::spawn(vec![
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
        Reply::new(201, &forwarded),
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
    ]);
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            install_channel_source(v, Client::new(&server.base).unwrap());
        });
    });
    open_forward(&mut visual, &view, 42, 1);
    // The POST task is spawned but not polled yet: replacing the dialog before
    // the next executor step is the late-answer race the guard must survive.
    click_destination(&mut visual, &view, 0);

    // A new dialog for the same message replaces the pending one while the old
    // POST is still in flight.
    open_forward(&mut visual, &view, 42, 1);
    let new_request = view.read_with(&*visual, |v, _| {
        v.forward.as_ref().expect("the new dialog").request
    });

    wait_entity(&mut visual, &view, |v| {
        !v.action_requests.contains_key("42:forward")
    });
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        let state = v
            .forward
            .as_ref()
            .expect("the late success must not close the new dialog");
        assert_eq!(state.request, new_request);
        assert!(!state.sending);
        assert!(state.error.is_empty());
    });
}

/// A late error of the previous dialog must not paint itself into a dialog
/// opened later for the same row.
#[gpui::test]
async fn a_late_error_does_not_show_in_a_new_dialog_for_the_same_message(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let destinations = destinations_json(&[destination("channel", 99)]);
    let server = TestServer::spawn(vec![
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
        Reply::new(504, "{\"error\":\"gateway_timeout\"}"),
        Reply::new(200, &destinations).with_path(FORWARD_DESTINATIONS),
    ]);
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            install_channel_source(v, Client::new(&server.base).unwrap());
        });
    });
    open_forward(&mut visual, &view, 42, 1);
    click_destination(&mut visual, &view, 0);

    open_forward(&mut visual, &view, 42, 1);
    let new_request = view.read_with(&*visual, |v, _| {
        v.forward.as_ref().expect("the new dialog").request
    });

    wait_entity(&mut visual, &view, |v| {
        !v.action_requests.contains_key("42:forward")
    });
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        let state = v
            .forward
            .as_ref()
            .expect("the late error must not close the new dialog");
        assert_eq!(state.request, new_request);
        assert!(!state.sending);
        assert!(
            state.error.is_empty(),
            "the old dialog's error must not paint here: {}",
            state.error
        );
    });
}
