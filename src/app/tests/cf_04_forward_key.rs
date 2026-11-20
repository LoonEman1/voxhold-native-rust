//! CF-04: one forward dialog keeps one idempotency key per destination, so a
//! retry after a server error repeats the same logical send instead of creating
//! a second message. A new dialog starts with fresh keys.

use super::super::*;
use super::{add_auth_window, message_json};
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
