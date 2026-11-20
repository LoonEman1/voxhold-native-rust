//! Third review follow-up: search permissions after a personal-chat access
//! event, pins loaded with the channel selection, thread sends reopened after a
//! channel switch, and "К последним" preserving concurrent comments.

use super::super::*;
use super::{add_auth_window, message_json};
use crate::message_tools::{self, Action};
use crate::search::SearchPanel;
use crate::test_support::{Gate, Reply, TestServer};
use crate::thread_panel::ThreadPanel;
use gpui::{Entity, TestAppContext, VisualTestContext};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn conversation(id: i64) -> voxhold_api::Conversation {
    voxhold_api::Conversation {
        id,
        direct: true,
        ..Default::default()
    }
}

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

fn search_hit(id: i64, channel: i64, content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "channel_id": channel,
        "conversation_id": 0,
        "content": content,
        "author": {"user_id": 1, "username": "neva"},
        "created_at": 1,
        "revision": 1,
        "comment_count": 0,
        "client_message_id": "",
        "reactions": [],
        "reactions_revision": 0,
        "attachments": [],
        "poll": null,
        "channel_name": "channel",
    })
}

fn search_page(rows: serde_json::Value, next: Option<i64>, has_more: bool) -> String {
    serde_json::json!({
        "messages": rows,
        "pagination": {"next_before_id": next, "has_more": has_more},
    })
    .to_string()
}

fn thread_row(id: i64, content: &str) -> serde_json::Value {
    let mut row = message_json(id, 1, content);
    row["thread_root_id"] = 5.into();
    row
}

fn thread_page(rows: Vec<serde_json::Value>, next: Option<i64>, has_more: bool) -> String {
    serde_json::json!({
        "messages": rows,
        "pagination": {"next_before_id": next, "has_more": has_more},
    })
    .to_string()
}

fn empty_page() -> String {
    thread_page(Vec::new(), None, false)
}

fn install_search(
    window: &mut gpui::Window,
    app: &mut gpui::App,
    view: &Entity<Voxhold>,
    client: Client,
) -> Entity<SearchPanel> {
    let runtime = view.read_with(app, |v, _| v.runtime.clone());
    let panel = app.new(|cx| SearchPanel::new(client, runtime, window, cx));
    let weak = view.downgrade();
    panel.update(app, |panel, _| {
        panel.set_pins_handler(Box::new(move |pins, cx| {
            if let Some(view) = weak.upgrade() {
                view.update(cx, |view, cx| {
                    view.pinned_ids = pins.iter().map(|message| message.id).collect();
                    cx.notify();
                });
            }
        }));
    });
    view.update(app, |v, cx| {
        v.search = Some(panel.clone());
        v.show_search = true;
        cx.notify();
    });
    panel
}

fn queue_event(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    kind: &str,
    data: serde_json::Value,
) {
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    tx.send(voxhold_api::RealtimeEvent {
        kind: kind.into(),
        data,
        ..Default::default()
    })
    .unwrap();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.events = Some(Arc::new(Mutex::new(rx)));
            v.events_overflow = Default::default();
            v.schedule_events(v.realtime_generation, window, cx);
        });
    });
    visual.executor().advance_clock(Duration::from_millis(200));
}

/// While a personal chat is open, `channels.changed` drops the confirmed
/// search snapshot before the new channel list arrives. The old rows and the
/// in-flight search page started under them cannot paint again.
#[gpui::test]
async fn dm_access_change_drops_search_results_before_the_new_snapshot(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate_more = Gate::new();
    let gate_channels = Gate::new();
    gate_more.arm();
    gate_channels.arm();
    let first = search_page(
        serde_json::json!([search_hit(10, 7, "visible")]),
        Some(10),
        true,
    );
    let old_more = search_page(
        serde_json::json!([search_hit(9, 7, "SECRET-OLD")]),
        None,
        false,
    );
    let channels_without_x = serde_json::json!([{
        "id": 9,
        "name": "kept",
        "kind": "text",
        "permissions": 1,
        "e2ee_required": false
    }])
    .to_string();
    let fresh = search_page(
        serde_json::json!([search_hit(8, 7, "SECRET-REVOKED")]),
        None,
        false,
    );
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &first),
        Reply::new(200, &old_more).with_gate(gate_more.clone()),
        Reply::new(200, &channels_without_x).with_gate(gate_channels.clone()),
        Reply::new(200, &fresh),
    ]);
    let client = Client::new(&server.base).unwrap();
    let panel_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = panel_slot.clone();
    visual.update(|window, app| {
        let panel = install_search(window, app, &view, client.clone());
        view.update(app, |v, cx| {
            v.client = Some(client);
            v.server = 1;
            v.channel = 5;
            v.conversation = Some(conversation(5));
            v.channels = vec![
                Channel {
                    id: 7,
                    kind: "text".into(),
                    permissions: 1,
                    ..Default::default()
                },
                Channel {
                    id: 9,
                    kind: "text".into(),
                    permissions: 1,
                    ..Default::default()
                },
            ];
            v.search_permissions_ready = true;
            panel.update(cx, |panel, cx| {
                panel.set_allowed_channels(HashSet::from([7, 9]), cx)
            });
        });
        panel.update(app, |panel, cx| {
            panel.open(1, 5, search::Mode::Search, cx);
            panel.set_query_for_test("secret", window, cx);
        });
        *captured.borrow_mut() = Some(panel);
    });
    let panel = panel_slot.borrow().clone().expect("search panel");
    visual.run_until_parked();
    visual.update(|_window, app| {
        panel.update(app, |panel, cx| panel.submit_for_test(cx));
    });
    wait_entity(&mut visual, &panel, |panel| !panel.search_loading());
    panel.read_with(&*visual, |panel, _| {
        assert_eq!(panel.results().len(), 1);
        assert_eq!(panel.results()[0].message.channel_id, 7);
    });

    // A second search page is already in flight while the confirmed X row is
    // still visible.
    visual.update(|_window, app| {
        panel.update(app, |panel, cx| panel.load_more_for_test(cx));
    });
    assert!(gate_more.wait_arrived(Duration::from_secs(3)));
    panel.read_with(&*visual, |panel, _| {
        assert_eq!(panel.results().len(), 1);
        assert!(panel.search_loading() || !panel.results().is_empty());
    });

    queue_event(
        &mut visual,
        &view,
        "channels.changed",
        serde_json::json!({"server_id": 1}),
    );
    assert!(gate_channels.wait_arrived(Duration::from_secs(3)));
    visual.run_until_parked();
    panel.read_with(&*visual, |panel, _| {
        assert!(!panel.permissions_ready(), "the snapshot is unconfirmed");
        assert!(panel.results().is_empty(), "old rows disappear immediately");
        assert!(panel.allowed_for_test().is_empty());
        assert!(!panel.search_loading(), "no query may start yet");
    });

    gate_channels.release();
    wait_entity(&mut visual, &panel, |panel| panel.permissions_ready());
    panel.read_with(&*visual, |panel, _| {
        assert_eq!(panel.allowed_for_test(), &HashSet::from([9]));
        assert!(!panel.allowed_for_test().contains(&7));
    });

    // A page that still names the revoked channel is filtered out.
    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_query_for_test("after", window, cx)
        });
    });
    visual.run_until_parked();
    visual.update(|_window, app| {
        panel.update(app, |panel, cx| panel.submit_for_test(cx));
    });
    wait_entity(&mut visual, &panel, |panel| !panel.search_loading());
    gate_more.release();
    visual.run_until_parked();
    panel.read_with(&*visual, |panel, _| {
        assert!(
            panel.results().is_empty(),
            "the revoked row and the stale page stay hidden"
        );
    });
}

/// When the popup was closed during a personal-chat access event, the next
/// opening starts the fresh channel read that confirms the snapshot.
#[gpui::test]
async fn closed_dm_search_refreshes_on_the_next_open(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate = Gate::new();
    gate.arm();
    let fresh = serde_json::json!([{
        "id": 7,
        "name": "kept",
        "kind": "text",
        "permissions": 1,
        "e2ee_required": false
    }])
    .to_string();
    let server =
        TestServer::spawn_concurrent(vec![Reply::new(200, &fresh).with_gate(gate.clone())]);
    let client = Client::new(&server.base).unwrap();
    let panel_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = panel_slot.clone();
    visual.update(|window, app| {
        let panel = install_search(window, app, &view, client.clone());
        view.update(app, |v, cx| {
            v.client = Some(client);
            v.server = 1;
            v.channel = 5;
            v.conversation = Some(conversation(5));
            v.channels = vec![Channel {
                id: 7,
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            }];
            v.search_permissions_ready = true;
            v.show_search = false;
            panel.update(cx, |panel, cx| {
                panel.set_allowed_channels(HashSet::from([7]), cx)
            });
        });
        *captured.borrow_mut() = Some(panel);
    });
    let panel = panel_slot.borrow().clone().expect("search panel");

    queue_event(
        &mut visual,
        &view,
        "channels.changed",
        serde_json::json!({"server_id": 1}),
    );
    panel.read_with(&*visual, |panel, _| {
        assert!(!panel.permissions_ready());
        assert!(panel.allowed_for_test().is_empty());
    });
    assert!(
        server.requests().is_empty(),
        "a closed popup does not start a hidden request"
    );

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.conversation = None;
            v.channel = 7;
            v.open_search_popup(false, window, cx);
        });
    });
    assert!(gate.wait_arrived(Duration::from_secs(3)));
    view.read_with(&*visual, |v, _| {
        assert!(v.search_permissions_request.is_some());
        assert!(v.show_search);
    });
    gate.release();
    wait_entity(&mut visual, &panel, |panel| panel.permissions_ready());
    panel.read_with(&*visual, |panel, _| {
        assert!(panel.allowed_for_test().contains(&7));
    });
}

/// A `/channels` answer started by the `403` recheck before `channels.changed`
/// may not confirm the new snapshot even when it is the first to arrive.
#[gpui::test]
async fn dm_403_recheck_cannot_restore_search_after_access_event(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate_old = Gate::new();
    let gate_new = Gate::new();
    gate_old.arm();
    gate_new.arm();
    let old_channels = serde_json::json!([{
        "id": 7,
        "name": "revoked",
        "kind": "text",
        "permissions": 1,
        "e2ee_required": false
    }])
    .to_string();
    let new_channels = serde_json::json!([{
        "id": 9,
        "name": "kept",
        "kind": "text",
        "permissions": 1,
        "e2ee_required": false
    }])
    .to_string();
    let page = search_page(
        serde_json::json!([search_hit(8, 7, "SECRET-REVOKED")]),
        None,
        false,
    );
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &old_channels).with_gate(gate_old.clone()),
        Reply::new(200, &new_channels).with_gate(gate_new.clone()),
        Reply::new(200, &page),
    ]);
    let client = Client::new(&server.base).unwrap();
    let panel_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = panel_slot.clone();
    visual.update(|window, app| {
        let panel = install_search(window, app, &view, client.clone());
        view.update(app, |v, cx| {
            v.client = Some(client);
            v.server = 1;
            v.channel = 5;
            v.conversation = Some(conversation(5));
            v.channels = vec![Channel {
                id: 7,
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            }];
            v.search_permissions_ready = true;
            panel.update(cx, |panel, cx| {
                panel.set_allowed_channels(HashSet::from([7]), cx)
            });
            // The reaction `403` starts this request and owns the separate
            // `channel_snapshot_request` slot.
            v.recheck_channel_snapshot_no_window(cx);
        });
        panel.update(app, |panel, cx| panel.open(1, 5, search::Mode::Search, cx));
        *captured.borrow_mut() = Some(panel);
    });
    let panel = panel_slot.borrow().clone().expect("search panel");
    assert!(gate_old.wait_arrived(Duration::from_secs(3)));
    view.read_with(&*visual, |v, _| {
        assert!(v.channel_snapshot_request.is_some());
    });

    queue_event(
        &mut visual,
        &view,
        "channels.changed",
        serde_json::json!({"server_id": 1}),
    );
    assert!(gate_new.wait_arrived(Duration::from_secs(3)));
    view.read_with(&*visual, |v, _| {
        assert!(
            v.channel_snapshot_request.is_none(),
            "the access event released the old recheck slot"
        );
        assert!(v.search_permissions_request.is_some());
        assert!(!v.search_permissions_ready);
    });
    gate_old.release();
    visual.run_until_parked();
    panel.read_with(&*visual, |panel, _| {
        assert!(!panel.permissions_ready());
        assert!(panel.allowed_for_test().is_empty());
    });

    gate_new.release();
    wait_entity(&mut visual, &panel, |panel| panel.permissions_ready());
    panel.read_with(&*visual, |panel, _| {
        assert_eq!(panel.allowed_for_test(), &HashSet::from([9]));
    });

    visual.update(|window, app| {
        panel.update(app, |panel, cx| {
            panel.set_query_for_test("secret", window, cx)
        });
    });
    visual.run_until_parked();
    visual.update(|_window, app| {
        panel.update(app, |panel, cx| panel.submit_for_test(cx));
    });
    wait_entity(&mut visual, &panel, |panel| !panel.search_loading());
    panel.read_with(&*visual, |panel, _| {
        assert!(panel.results().is_empty());
    });
}

/// Selecting a channel installs the already-confirmed snapshot before the pin
/// refresh, so B's pins arrive without opening the popup.
#[gpui::test]
async fn selecting_a_channel_loads_pins_before_the_popup_opens(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let pins = serde_json::json!([{
        // One array element is both a Channel and a PinnedMessage. The
        // concurrent server can then answer the channel-snapshot and pin
        // requests in either order; history arrays simply fail to parse.
        "id": 8,
        "name": "kept",
        "kind": "text",
        "position": 0,
        "permissions": 3,
        "access_restricted": false,
        "e2ee_required": false,
        "comments_enabled": false,
        "can_publish": false,
        "unread_count": 0,
        "message": {
            "id": 42,
            "channel_id": 8,
            "conversation_id": 0,
            "content": "pinned",
            "author": {"user_id": 1, "username": "neva"},
            "created_at": 1,
            "revision": 1,
            "comment_count": 0,
            "client_message_id": "",
            "reactions": [],
            "reactions_revision": 0,
            "attachments": [],
            "poll": null
        },
        "pinned_by": {"user_id": 1, "username": "neva"},
        "pinned_at": 1
    }])
    .to_string();
    // Every queued answer is valid for both `/channels` and `/pins`.
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &pins),
        Reply::new(200, &pins),
        Reply::new(200, &pins),
        Reply::new(200, &pins),
        Reply::new(200, &pins),
        Reply::new(200, &pins),
        Reply::new(200, &pins),
        Reply::new(200, &pins),
    ]);
    let client = Client::new(&server.base).unwrap();
    visual.update(|window, app| {
        let panel = install_search(window, app, &view, client.clone());
        view.update(app, |v, cx| {
            v.client = Some(client);
            v.server = 1;
            v.channel = 7;
            v.self_user_id = 1;
            v.shell_initialized = true;
            v.show_members = false;
            v.channels = vec![
                Channel {
                    id: 7,
                    kind: "text".into(),
                    permissions: 3,
                    ..Default::default()
                },
                Channel {
                    id: 8,
                    kind: "text".into(),
                    permissions: 3,
                    ..Default::default()
                },
            ];
            v.search_permissions_ready = true;
            panel.update(cx, |panel, cx| {
                panel.set_allowed_channels(HashSet::from([7, 8]), cx)
            });
            // The real DM -> channel path leaves the snapshot unconfirmed.
            v.select_conversation(conversation(5), window, cx);
            v.select_channel(8, window, cx);
        });
    });
    wait_entity(&mut visual, &view, |v| v.pinned_ids.contains(&42));
    view.read_with(&*visual, |v, _| {
        assert!(!v.show_search, "the pins arrived before the popup");
        assert_eq!(v.pinned_ids, HashSet::from([42]));
        let permissions = crate::message_tools::MessagePermissions {
            pin: true,
            ..Default::default()
        };
        let actions = message_tools::menu_actions(permissions, true, false, false);
        assert!(actions.contains(&Action::Unpin));
        assert_eq!(Action::Unpin.label(), "Открепить");
    });
    let requests = server.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.path == "/api/v1/servers/1/channels"),
        "the unconfirmed snapshot was re-read: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|request| request.path.contains("/channels/8/pins")),
        "the selected channel requested its pins: {requests:?}"
    );
}

/// A thread POST that finishes after A -> B -> A still settles the reopened
/// panel: "Отправка…" clears, the sent draft is consumed, and only the POST row
/// from the matching access generation is inserted.
#[gpui::test]
async fn reopened_thread_send_settles_after_a_channel_switch(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate_success = Gate::new();
    let gate_error = Gate::new();
    gate_success.arm();
    gate_error.arm();
    let comment = thread_row(31, "pending send").to_string();
    let retry = thread_row(32, "retry").to_string();
    let empty = empty_page();
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &empty),
        Reply::new(201, &comment).with_gate(gate_success.clone()),
        Reply::new(200, &empty),
        Reply::new(200, &empty),
        Reply::new(500, "{\"error\":\"boom\"}").with_gate(gate_error.clone()),
        Reply::new(200, &empty),
        Reply::new(200, &empty),
        Reply::new(201, &retry),
    ]);
    let key = draft::ChatKey::thread(1, 7, 5);
    let root = serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 7;
            // No search panel: channel switches should only request history.
            v.search = None;
            v.shell_initialized = true;
            v.show_members = false;
            v.channels = vec![
                Channel {
                    id: 7,
                    kind: "text".into(),
                    permissions: 3,
                    ..Default::default()
                },
                Channel {
                    id: 8,
                    // A non-history channel makes the A -> B switch a real
                    // scope change without an extra history request that would
                    // consume the gated POST answer.
                    kind: "category".into(),
                    permissions: 3,
                    ..Default::default()
                },
            ];
            v.open_thread(root.clone(), 0, window, cx);
        });
    });
    let first = view
        .read_with(&*visual, |v, _| v.thread.clone())
        .expect("thread panel");
    wait_entity(&mut visual, &first, |panel| !panel.loading());
    visual.update(|window, app| {
        first.update(app, |panel, cx| {
            panel.set_composer_for_test("pending send", window, cx);
            panel.send_for_test(window, cx);
        });
    });
    assert!(gate_success.wait_arrived(Duration::from_secs(3)));
    let attempt = first.read_with(&*visual, |panel, _| panel.attempt());

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_channel(8, window, cx);
            v.select_channel(7, window, cx);
            v.open_thread(root.clone(), 0, window, cx);
        });
    });
    let reopened = view
        .read_with(&*visual, |v, _| v.thread.clone())
        .expect("reopened panel");
    reopened.read_with(&*visual, |panel, _| {
        assert!(panel.sending(), "the reopened panel keeps Отправка…");
        assert_eq!(panel.attempt(), attempt);
    });
    gate_success.release();
    wait_entity(&mut visual, &reopened, |panel| !panel.sending());
    reopened.read_with(&*visual, |panel, app| {
        assert!(panel.send_error().is_empty());
        assert_eq!(panel.composer_text(app), "");
        assert!(
            panel.messages().iter().all(|row| row.id != 31),
            "the old access generation did not insert its POST row"
        );
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.pending_sends.contains_key(&key));
        assert_eq!(v.drafts.borrow().text(key), "");
    });

    // The error path after another switch settles the same way and keeps the
    // draft for a retry with the same idempotency key.
    visual.update(|window, app| {
        reopened.update(app, |panel, cx| {
            panel.set_composer_for_test("retry text", window, cx);
            panel.send_for_test(window, cx);
        });
    });
    assert!(gate_error.wait_arrived(Duration::from_secs(3)));
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_channel(8, window, cx);
            v.select_channel(7, window, cx);
            v.open_thread(root.clone(), 0, window, cx);
        });
    });
    let after_error = view
        .read_with(&*visual, |v, _| v.thread.clone())
        .expect("panel after the error");
    after_error.read_with(&*visual, |panel, _| {
        assert!(panel.sending(), "the reopened panel sees the failed POST");
    });
    gate_error.release();
    wait_entity(&mut visual, &after_error, |panel| !panel.sending());
    after_error.read_with(&*visual, |panel, app| {
        assert!(
            !panel.send_error().is_empty(),
            "requests: {:?}",
            server
                .requests()
                .iter()
                .map(|request| format!("{} {}", request.method, request.path))
                .collect::<Vec<_>>()
        );
        assert_eq!(panel.composer_text(app), "retry text");
    });
    let send_key = view.read_with(&*visual, |v, _| {
        v.drafts
            .borrow()
            .get(key)
            .map(|draft| draft.send_key.clone())
    });
    assert!(send_key.as_ref().is_some_and(|key| !key.is_empty()));
    visual.update(|window, app| {
        after_error.update(app, |panel, cx| panel.send_for_test(window, cx));
    });
    wait_entity(&mut visual, &after_error, |panel| !panel.sending());
    let requests = server.requests();
    let bodies: Vec<serde_json::Value> = requests
        .iter()
        .filter(|request| request.method == "POST" && request.path.ends_with("/messages"))
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(bodies.len(), 3, "{requests:?}");
    assert_eq!(
        bodies[1]["client_message_id"],
        bodies[2]["client_message_id"]
    );
}

/// "К последним" replaces an anchored window but keeps a comment that arrived
/// over WS while the latest page was in flight. A tombstone received before the
/// answer still wins. The test starts from a real anchored target page and then
/// drives the same latest-page transition the button uses.
#[gpui::test]
async fn latest_thread_page_keeps_concurrent_comments_and_tombstones(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let target_page = thread_page(
        vec![thread_row(40, "new"), thread_row(50, "newer")],
        Some(40),
        true,
    );
    let anchored_page = thread_page(
        vec![thread_row(10, "old"), thread_row(20, "older")],
        None,
        false,
    );
    let latest_page = || voxhold_api::MessagePage {
        messages: vec![
            serde_json::from_value(thread_row(40, "new")).unwrap(),
            serde_json::from_value(thread_row(50, "newer")).unwrap(),
        ],
        pagination: voxhold_api::Pagination {
            next_before_id: None,
            has_more: false,
        },
    };
    let server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &target_page),
        Reply::new(200, &anchored_page),
    ]);
    let client = Client::new(&server.base).unwrap();
    let runtime = view.read_with(&*visual, |v, _| v.runtime.clone());
    let drafts = view.read_with(&*visual, |v, _| v.drafts.clone());
    let root = serde_json::from_value::<Message>(message_json(5, 1, "root")).unwrap();
    let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = slot.clone();
    visual.update(|window, app| {
        let panel = app.new(|cx| {
            ThreadPanel::new(
                client.clone(),
                runtime.clone(),
                1,
                7,
                root.clone(),
                true,
                0,
                drafts.clone(),
                window,
                cx,
            )
        });
        panel.update(app, |panel, cx| {
            panel.set_read_for_test(1000);
            panel.set_target(30);
            panel.load_for_test(0, cx);
        });
        *captured.borrow_mut() = Some(panel);
    });
    let panel = slot.borrow().clone().expect("thread panel");
    wait_entity(&mut visual, &panel, |panel| !panel.loading());
    panel.read_with(&*visual, |panel, _| {
        assert!(panel.anchored(), "the test starts from a real old context");
        assert_eq!(
            panel
                .messages()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            [10, 20]
        );
    });

    visual.update(|_window, app| {
        panel.update(app, |panel, cx| {
            panel.begin_latest_for_test();
            panel.handle_event("message.created", &thread_row(80, "ws comment"), 0, cx);
            panel.apply_latest_page_for_test(latest_page());
        });
    });
    panel.read_with(&*visual, |panel, _| {
        let ids: Vec<i64> = panel.messages().iter().map(|row| row.id).collect();
        assert_eq!(ids, [40, 50, 80]);
        assert_eq!(ids.iter().filter(|id| **id == 80).count(), 1);
        assert!(!panel.anchored(), "the old window was replaced");
    });

    // Second panel: start anchored, create and delete 80 while the latest page
    // is in flight, then make sure the tombstone survives the replacement.
    let second_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let second_captured = second_slot.clone();
    visual.update(|window, app| {
        let panel = app
            .new(|cx| ThreadPanel::new(client, runtime, 1, 7, root, true, 0, drafts, window, cx));
        panel.update(app, |panel, cx| {
            panel.set_read_for_test(1000);
            panel.apply_page_for_test(
                voxhold_api::MessagePage {
                    messages: vec![
                        serde_json::from_value(thread_row(10, "old")).unwrap(),
                        serde_json::from_value(thread_row(20, "older")).unwrap(),
                    ],
                    pagination: voxhold_api::Pagination {
                        next_before_id: None,
                        has_more: false,
                    },
                },
                0,
                None,
                true,
            );
            assert!(panel.anchored());
            panel.begin_latest_for_test();
            panel.handle_event("message.created", &thread_row(80, "ws comment"), 0, cx);
            panel.handle_event(
                "message.deleted",
                &serde_json::json!({"channel_id": 7, "message_id": 80}),
                0,
                cx,
            );
            panel.apply_latest_page_for_test(latest_page());
        });
        *second_captured.borrow_mut() = Some(panel);
    });
    let second = second_slot.borrow().clone().expect("second panel");
    second.read_with(&*visual, |panel, _| {
        let ids: Vec<i64> = panel.messages().iter().map(|row| row.id).collect();
        assert_eq!(ids, [40, 50]);
        assert!(!ids.contains(&80), "the tombstone survives the page");
        assert!(!panel.anchored());
    });
}
