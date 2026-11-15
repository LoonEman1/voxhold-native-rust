use super::*;
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;

/// Build the real root view in a test window without touching the user's
/// preferences: the layout file points at a fresh temporary path. The window
/// root must be `gpui_component::Root`, exactly like the shipped window.
fn add_auth_window(cx: &mut TestAppContext) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
    let shell = std::env::temp_dir().join(format!(
        "voxhold-ui-test-{}-{}.json",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let slot: std::rc::Rc<std::cell::RefCell<Option<Entity<Voxhold>>>> = Default::default();
    let captured = slot.clone();
    let window = cx.add_window(move |window, cx| {
        let view = cx.new(|cx| Voxhold::new(window, cx, "", Some(shell)));
        *captured.borrow_mut() = Some(view.clone());
        Root::new(view.into(), window, cx)
    });
    let view = slot.borrow().clone().expect("view");
    (window, view)
}

fn focus_field(
    cx: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    pick: fn(&Voxhold) -> Entity<InputState>,
) {
    let view = view.clone();
    cx.update(|window, cx| {
        view.update(cx, |v, cx| {
            let field = pick(v);
            field.update(cx, |input, cx| input.focus(window, cx));
        });
    });
}

#[gpui::test]
async fn instance_validation_keeps_the_screen_and_the_typed_value(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    focus_field(&mut cx, &view, |v| v.instance.clone());
    cx.simulate_input("http://example.com");
    let raw = view.read_with(&*cx, |v, cx| v.instance.read(cx).text().to_string());
    assert_eq!(raw, "http://example.com", "raw input text");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let (screen, status, value) = view.read_with(&*cx, |v, cx| {
        (
            v.auth.screen,
            v.auth.status.clone(),
            v.instance.read(cx).value().to_string(),
        )
    });
    assert_eq!(screen, auth::AuthScreen::Instance);
    assert_eq!(status, "Введите корректный HTTPS-адрес инстанса");
    assert_eq!(value, "http://example.com");
}

#[gpui::test]
async fn loopback_advances_to_login_and_types_into_the_right_fields(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    focus_field(&mut cx, &view, |v| v.instance.clone());
    cx.simulate_input("http://127.0.0.1:1");
    cx.simulate_keystrokes("enter");

    let (screen, origin) = view.read_with(&*cx, |v, _| (v.auth.screen, v.auth.origin.clone()));
    assert_eq!(screen, auth::AuthScreen::Login);
    assert_eq!(origin, "http://127.0.0.1:1");

    // Enter must hand focus to the user name; otherwise these characters
    // would land in the address field behind the new screen.
    cx.simulate_input("neva");
    let (username, address) = view.read_with(&*cx, |v, cx| {
        (
            v.username.read(cx).value().to_string(),
            v.instance.read(cx).value().to_string(),
        )
    });
    assert_eq!(username, "neva");
    assert_eq!(address, "http://127.0.0.1:1");

    focus_field(&mut cx, &view, |v| v.password.clone());
    cx.simulate_input("password123");
    let password = view.read_with(&*cx, |v, cx| v.password.read(cx).unmask_value().to_string());
    assert_eq!(password, "password123");

    cx.simulate_keystrokes("enter");
    let (first, password_after) = view.read_with(&*cx, |v, cx| {
        (
            (v.auth.busy, v.auth.login_attempt(), v.auth.status.clone()),
            v.password.read(cx).unmask_value().to_string(),
        )
    });
    assert_eq!(first.1, 1, "Enter must start exactly one attempt");
    assert_eq!(
        password_after, "password123",
        "Enter must not reach the field"
    );
    // Nothing listens on 127.0.0.1:1, so the attempt either is still pending
    // or already finished with the Go network message.
    assert!(
        first.0 || first.2 == "Не удалось связаться с сервером",
        "unexpected state after Enter: {first:?}"
    );

    // After the failure the form stays on the same screen and a repeated
    // Enter starts a fresh attempt with the same confirmed origin.
    let (failed_attempt, screen) =
        view.read_with(&*cx, |v, _| (v.auth.login_attempt(), v.auth.screen));
    assert_eq!(screen, auth::AuthScreen::Login);
    if !first.0 {
        cx.simulate_keystrokes("enter");
        let retried = view.read_with(&*cx, |v, _| v.auth.login_attempt());
        assert_eq!(retried, failed_attempt + 1);
    }
}

fn message_json(id: i64, revision: i64, content: &str) -> serde_json::Value {
    message_json_with_reactions(id, revision, content, 0)
}

fn message_json_with_reactions(
    id: i64,
    revision: i64,
    content: &str,
    reactions_revision: i64,
) -> serde_json::Value {
    let reactions = if reactions_revision > 0 {
        serde_json::json!([{"emoji": "❤️", "count": 1, "me": false}])
    } else {
        serde_json::json!([])
    };
    serde_json::json!({
        "id": id,
        "channel_id": 7,
        "conversation_id": 0,
        "content": content,
        "author": {"user_id": 1, "username": "neva"},
        "created_at": 1,
        "revision": revision,
        "comment_count": 0,
        "client_message_id": "",
        "reactions": reactions,
        "reactions_revision": reactions_revision,
        "attachments": [],
        "poll": null
    })
}

/// Regression for the R02 merge policy: a stale revision cannot overwrite a
/// confirmed row, a deleted id never comes back from a late event and an
/// edit that omits `reactions` keeps the confirmed reaction revision.
#[gpui::test]
async fn realtime_merge_keeps_revisions_and_tombstones(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.channel = 7;
            v.apply_realtime_event(
                "message.created",
                &message_json_with_reactions(2, 5, "confirmed", 6),
            );
            v.apply_realtime_event("message.created", &message_json(2, 4, "stale"));
            assert_eq!(v.messages.len(), 1);
            assert_eq!(v.messages[0].content, "confirmed");
            assert_eq!(v.messages[0].reactions_revision, 6);

            // Backend `message.updated` omits reactions.
            v.apply_realtime_event("message.updated", &message_json(2, 6, "edited"));
            assert_eq!(v.messages[0].content, "edited");
            assert_eq!(v.messages[0].reactions.len(), 1);
            assert_eq!(v.messages[0].reactions_revision, 6);

            v.apply_realtime_event(
                "message.deleted",
                &serde_json::json!({"channel_id": 7, "message_id": 2}),
            );
            assert!(v.messages.is_empty());
            assert!(v.deleted_ids.contains(&2));

            v.apply_realtime_event("message.created", &message_json(2, 7, "resurrected"));
            assert!(v.messages.is_empty(), "a tombstone must win");

            v.apply_realtime_event("message.created", &message_json(3, 1, "next"));
            assert_eq!(v.messages.len(), 1);
            assert_eq!(v.messages[0].id, 3);

            v.server = 1;
            let effect =
                v.apply_realtime_event("channels.changed", &serde_json::json!({"server_id": 1}));
            assert!(effect.access, "an access event must invalidate the scope");
        });
    });
}

/// A confirmed message must disappear as soon as the access scope changes,
/// before any new HTTP answer arrives.
#[gpui::test]
async fn access_change_hides_confirmed_history(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 7;
            v.apply_realtime_event("message.created", &message_json(4, 1, "private"));
            assert_eq!(v.messages.len(), 1);
            let before = v.access_generation;
            v.invalidate_access(window, cx);
            assert!(v.messages.is_empty());
            assert!(v.history_keys.is_empty());
            assert_eq!(v.access_generation, before + 1);
            assert!(!v.show_search);
        });
    });
}

#[gpui::test]
async fn snapshot_keeps_newer_ws_state_and_discards_absent_old_rows(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.channel = 7;
            let dto = |id, revision, text, reactions| {
                serde_json::from_value::<Message>(message_json_with_reactions(
                    id, revision, text, reactions,
                ))
                .unwrap()
            };
            v.merge_message(true, dto(1, 2, "absent from snapshot", 0));
            v.merge_message(true, dto(2, 12, "edit from WS", 18));
            v.merge_message(false, dto(3, 16, "edit before first page", 0));
            v.merge_message(true, dto(6, 1, "deleted while offline", 0));
            v.refresh_request = Some(1);
            v.merge_message(true, dto(7, 17, "new WS row", 0));
            v.apply_realtime_event(
                "message.deleted",
                &serde_json::json!({"channel_id":7,"message_id":4}),
            );
            let pending = std::mem::take(&mut v.refresh_new_ids);
            v.apply_history_snapshot(
                voxhold_api::MessagePage {
                    messages: vec![
                        dto(2, 11, "old body", 19),
                        dto(3, 10, "old row", 0),
                        dto(4, 10, "late deleted", 0),
                    ],
                    pagination: voxhold_api::Pagination {
                        next_before_id: Some(2),
                        has_more: true,
                    },
                },
                pending,
            );
            assert_eq!(
                v.messages.iter().map(|row| row.id).collect::<Vec<_>>(),
                [2, 3, 7]
            );
            assert_eq!(v.messages[0].content, "edit from WS");
            assert_eq!(v.messages[0].reactions_revision, 19);
            assert_eq!(v.messages[1].content, "edit before first page");
        })
    });
}

#[gpui::test]
async fn painted_history_at_tail_reports_latest_row(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.client = Some(Client::new("http://127.0.0.1:1").unwrap());
            v.server = 1;
            v.channel = 7;
            v.channels.push(Channel {
                id: 7,
                kind: "text".into(),
                permissions: 1,
                ..Default::default()
            });
            for id in 1..=50 {
                v.apply_realtime_event("message.created", &message_json(id, id, "row"));
            }
            cx.notify();
        });
        window.draw(app).clear();
    });
    let painted = view.read_with(&*visual, |v, _| v.painted_read.get());
    assert_eq!(painted, 50);
}

#[gpui::test]
async fn news_history_starts_at_the_top_when_switching_channels(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.client = Some(Client::new("http://127.0.0.1:1").unwrap());
            v.server = 1;
            for (id, kind) in [(7, "news"), (8, "text")] {
                v.channels.push(Channel {
                    id,
                    kind: kind.into(),
                    permissions: 1,
                    ..Default::default()
                });
            }
        });
        for (channel, count) in [(8, 50), (7, 2), (8, 50)] {
            view.update(app, |v, cx| {
                v.select_channel(channel, window, cx);
                for id in (1..=count).rev() {
                    let mut message = message_json(id, id, "row");
                    message["channel_id"] = channel.into();
                    v.apply_realtime_event("message.created", &message);
                }
            });
            window.draw(app).clear();
            view.read_with(app, |v, _| {
                if channel == 7 {
                    let first = v.history_list.bounds_for_item(0).expect("first news row");
                    let second = v.history_list.bounds_for_item(1).expect("second news row");
                    assert_eq!(first.top(), v.history_list.viewport_bounds().top());
                    assert!(second.top() >= first.bottom());
                    assert_eq!((v.messages[0].id, v.messages[1].id), (1, 2));
                } else {
                    assert_eq!(v.history_list.logical_scroll_top().item_ix, count as usize);
                }
            });
        }
    });
}

#[gpui::test]
async fn presence_snapshot_and_changes_stay_in_the_current_server(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| view.update(app, |v, _| {
            v.server = 7;
            v.apply_realtime_event("presence.snapshot", &serde_json::json!({
                "servers": [
                    {"server_id": 7, "online_user_ids": [2,3]},
                    {"server_id": 8, "online_user_ids": [4]}
                ]
            }));
            v.apply_realtime_event("presence.updated", &serde_json::json!({"server_id":7,"user_id":2,"status":"offline","last_seen_at":42}));
            v.apply_realtime_event("presence.updated", &serde_json::json!({"server_id":8,"user_id":5,"status":"online"}));
            assert_eq!(v.online_by_server[&7], [3].into_iter().collect());
            assert_eq!(v.online_by_server[&8], [4, 5].into_iter().collect());
            assert_eq!(v.roster.last_seen.get(&2), Some(&42));
        }));
}

/// The unread divider is data-driven: the first confirmed snapshot places it
/// once, and reads sent during the visit never move it forward.
#[gpui::test]
async fn read_snapshot_places_the_unread_divider_once(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.self_user_id = 9;
            v.apply_realtime_event(
                "channel.read_snapshot",
                &serde_json::json!({"server_id":1,"channel_id":7,"reads":[
                    {"server_id":1,"channel_id":7,"user_id":9,"last_read_message_id":42},
                    {"server_id":1,"channel_id":7,"user_id":2,"last_read_message_id":40}
                ]}),
            );
            assert_eq!(v.unread_after, 42);
            assert_eq!(v.channel_reads.get(&(1, 7)), Some(&42));
            assert_eq!(v.reads.get(&2), Some(&40));

            v.read_sent = 42;
            v.apply_realtime_event(
                "channel.read_snapshot",
                &serde_json::json!({"server_id":1,"channel_id":7,"reads":[
                    {"server_id":1,"channel_id":7,"user_id":9,"last_read_message_id":44}
                ]}),
            );
            assert_eq!(v.unread_after, 42, "the divider stays at the open boundary");
            assert_eq!(v.channel_reads.get(&(1, 7)), Some(&44));
        });
    });
}

#[gpui::test]
async fn context_merge_keeps_ws_edits_reactions_and_pending_events(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            let dto = |id, revision, text, reactions| {
                serde_json::from_value::<Message>(message_json_with_reactions(
                    id, revision, text, reactions,
                ))
                .unwrap()
            };
            v.merge_message(true, dto(10, 5, "confirmed", 7));
            v.merge_message(true, dto(11, 2, "other", 0));
            // WS edit and reactions arrive while /context is in flight.
            v.apply_realtime_event("message.updated", &message_json(10, 6, "edited by ws"));
            v.apply_realtime_event(
                "message.reactions_changed",
                &serde_json::json!({"channel_id":7,"message_id":10,"revision":9,
                        "reactions":[{"emoji":"🔥","count":1,"me":true}]}),
            );
            // The target is outside the window, so its changes are pending.
            v.apply_realtime_event("message.updated", &message_json(12, 8, "late target edit"));
            v.apply_realtime_event(
                "message.reactions_changed",
                &serde_json::json!({"channel_id":7,"message_id":12,"revision":11,
                        "reactions":[{"emoji":"🔥","count":2,"me":false}]}),
            );
            let context = voxhold_api::MessageContext {
                messages: vec![
                    dto(9, 1, "deleted before context", 0),
                    dto(10, 2, "stale context", 1),
                    dto(11, 1, "old other", 0),
                    dto(12, 1, "old target", 0),
                ],
                target_message_id: 12,
                target_index: 3,
            };
            v.deleted_ids.insert(9);
            assert!(v.apply_context_snapshot(context));
            let ids: Vec<_> = v.messages.iter().map(|message| message.id).collect();
            assert_eq!(ids, [10, 11, 12], "deleted context rows stay tombstoned");
            assert_eq!(v.messages[0].content, "edited by ws");
            assert_eq!(v.messages[0].reactions_revision, 9);
            assert!(v.messages[0].reactions[0].me);
            assert_eq!(v.messages[2].content, "late target edit");
            assert_eq!(v.messages[2].reactions_revision, 11);
            assert!(!v.pending_edits.contains_key(&12));
            assert!(!v.pending_reactions.contains_key(&12));
            assert_eq!(v.before, Some(10));
            assert_eq!(v.pending_jump, Some(12));
            assert!(v.error.is_empty());
        });
    });
}

#[gpui::test]
async fn context_target_deleted_during_request_keeps_the_window(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            let dto = |id| serde_json::from_value::<Message>(message_json(id, 1, "row")).unwrap();
            v.merge_message(true, dto(10));
            v.merge_message(true, dto(11));
            v.apply_realtime_event(
                "message.deleted",
                &serde_json::json!({"channel_id":7,"message_id":12}),
            );
            let context = voxhold_api::MessageContext {
                messages: vec![dto(10), dto(11), dto(12)],
                target_message_id: 12,
                target_index: 2,
            };
            assert!(!v.apply_context_snapshot(context));
            assert_eq!(
                v.messages
                    .iter()
                    .map(|message| message.id)
                    .collect::<Vec<_>>(),
                [10, 11],
                "the old window must survive a deleted target"
            );
            assert_eq!(v.error, "Сообщение недоступно");
        });
    });
}

#[gpui::test]
async fn context_replacement_invalidates_the_old_older_page(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.before = Some(100);
            v.older_request = 42;
            v.loading_older = true;
            v.older_error = "старая ошибка".into();
            v.older_retry_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(60));
            v.older_scheduled = true;
            let (generation, access) = (v.generation, v.access_generation);
            assert!(v.accepts_older_reply(42, 100, 1, 7, generation, access));

            let messages = (30..=80)
                .map(|id| serde_json::from_value::<Message>(message_json(id, 1, "row")).unwrap())
                .collect();
            let context = voxhold_api::MessageContext {
                messages,
                target_message_id: 55,
                target_index: 25,
            };
            assert!(v.apply_context_snapshot(context));
            assert_eq!(v.before, Some(30));
            assert!(!v.loading_older);
            assert_eq!(v.older_request, 0);
            assert!(v.older_error.is_empty());
            assert!(v.older_retry_at.is_none());
            assert!(!v.older_scheduled);
            assert!(v.has_more);
            assert!(v.history_has_newer);

            assert!(!v.accepts_older_reply(42, 100, 1, 7, generation, access));
            v.older_request = 43;
            assert!(v.accepts_older_reply(43, 30, 1, 7, generation, access));
            assert!(!v.accepts_older_reply(43, 100, 1, 7, generation, access));
            assert!(!v.accepts_older_reply(42, 30, 1, 7, generation, access));
        });
    });
}

#[gpui::test]
async fn reaction_on_old_row_keeps_the_window_and_revision_order(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            for id in 1..=60 {
                let message =
                    serde_json::from_value::<Message>(message_json(id, 1, "row")).unwrap();
                v.merge_message(true, message);
            }
            let added = voxhold_api::ReactionsUpdate {
                channel_id: 7,
                message_id: 1,
                revision: 1,
                reactions: vec![voxhold_api::Reaction {
                    emoji: "🔥".into(),
                    count: 1,
                    me: true,
                }],
            };
            v.apply_reaction_update(added.clone());
            assert_eq!(v.messages[0].reactions_revision, 1);
            assert!(v.messages[0].reactions[0].me);
            // Same revision and a foreign row must not overwrite the accepted
            // reaction, and the old window stays loaded.
            v.apply_reaction_update(voxhold_api::ReactionsUpdate {
                reactions: Vec::new(),
                ..added
            });
            v.apply_reaction_update(voxhold_api::ReactionsUpdate {
                channel_id: 7,
                message_id: 41,
                revision: 9,
                reactions: Vec::new(),
            });
            assert_eq!(v.messages[0].reactions_revision, 1);
            assert!(v.messages[0].reactions[0].me);
            assert_eq!(v.messages.len(), 60);
            assert_eq!(v.messages[0].id, 1);

            v.apply_reaction_update(voxhold_api::ReactionsUpdate {
                channel_id: 7,
                message_id: 1,
                revision: 2,
                reactions: Vec::new(),
            });
            assert_eq!(v.messages[0].reactions_revision, 2);
            assert!(!v.messages[0].reactions.iter().any(|reaction| reaction.me));
        });
    });
}

#[gpui::test]
async fn latest_cancels_a_late_context_reply(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            v.generation = 3;
            v.access_generation = 4;
            v.context_request = Some(7);
            assert!(v.accepts_context_reply(7, 3, 4, 1, 7));

            v.reset_for_latest();
            assert!(v.context_request.is_none());
            assert!(v.history_reset_pending);
            assert!(
                !v.accepts_context_reply(7, 3, 4, 1, 7),
                "latest owns the window; the old context reply is stale"
            );

            v.context_request = Some(8);
            assert!(!v.accepts_context_reply(7, 3, 4, 1, 7));
            assert!(v.accepts_context_reply(8, 3, 4, 1, 7));
        });
    });
}

#[test]
fn history_retry_delay_matches_go() {
    use std::time::Duration;
    assert_eq!(
        Voxhold::history_retry_delay(&voxhold_api::Error::Http(429)),
        Duration::from_secs(20)
    );
    assert_eq!(
        Voxhold::history_retry_delay(&voxhold_api::Error::Http(503)),
        Duration::from_secs(8)
    );
    assert_eq!(
        Voxhold::history_retry_delay(&voxhold_api::Error::Network),
        Duration::from_secs(3)
    );
}

#[gpui::test]
async fn the_viewer_owns_enter_and_escape_while_it_is_open(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, _| writable_workspace(v));
    });
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    focus_field(&mut visual, &view, |v| v.composer.clone());
    visual.simulate_input("текст");
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.media.viewer = Some(crate::media::Viewer::new(crate::media::ViewerKey {
                chat: v.chat_key(),
                access: v.access_generation,
                message_id: 5,
                attachment_id: "a".repeat(32),
                open_id: 1,
            }));
        });
        window.draw(app).clear();
    });
    visual.simulate_keystrokes("enter");
    assert_eq!(
        view.read_with(&*visual, |v, _| v.send_attempt),
        0,
        "the viewer consumes Enter instead of sending"
    );
    visual.simulate_keystrokes("escape");
    assert!(view.read_with(&*visual, |v, _| v.media.viewer.is_none()));
}

fn writable_workspace(v: &mut Voxhold) {
    v.client = Some(Client::new("http://127.0.0.1:1").unwrap());
    v.server = 1;
    v.channel = 7;
    // Keep the test scope free of the first-frame members request unless a
    // test explicitly opens the panel.
    v.shell_initialized = true;
    v.show_members = false;
    v.channels.push(Channel {
        id: 7,
        kind: "text".into(),
        permissions: 3,
        ..Default::default()
    });
}

fn wait_until(cx: &mut VisualTestContext, mut done: impl FnMut() -> bool) {
    for _ in 0..600 {
        cx.run_until_parked();
        if done() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the asynchronous operation did not finish");
}

#[gpui::test]
async fn enter_sends_once_and_shift_enter_adds_a_newline(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, _| writable_workspace(v));
    });
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    focus_field(&mut visual, &view, |v| v.composer.clone());
    visual.update(|window, app| {
        window.draw(app).clear();
    });

    visual.simulate_input("первая");
    visual.simulate_keystrokes("shift-enter");
    visual.simulate_input("вторая");
    let (text, attempt) = view.read_with(&*visual, |v, cx| {
        (v.composer.read(cx).value().to_string(), v.send_attempt)
    });
    assert_eq!(text, "первая\nвторая");
    assert_eq!(attempt, 0, "Shift+Enter must not send");

    visual.simulate_keystrokes("enter");
    assert_eq!(
        view.read_with(&*visual, |v, _| v.send_attempt),
        1,
        "a plain Enter submits exactly once"
    );
    // Nothing listens on 127.0.0.1:1: the failure keeps the text and the
    // draft so the same key can be retried.
    let mut settled = false;
    for _ in 0..600 {
        visual.run_until_parked();
        settled = view.read_with(&*visual, |v, cx| {
            !v.is_sending() && v.composer.read(cx).value().as_ref() == "первая\nвторая"
        });
        if settled {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(settled, "the failed send must settle");
    let error = view.read_with(&*visual, |v, _| v.composer_error.clone());
    assert_eq!(error, "Не удалось связаться с сервером.");
    // A retry of the unchanged draft reuses the same idempotency key.
    let key = draft::ChatKey::channel(1, 7);
    let first_key = view.read_with(&*visual, |v, _| {
        v.drafts
            .borrow()
            .get(key)
            .map(|draft| draft.send_key.clone())
    });
    assert!(first_key.as_ref().is_some_and(|key| !key.is_empty()));
    visual.simulate_keystrokes("enter");
    for _ in 0..200 {
        visual.run_until_parked();
        if !view.read_with(&*visual, |v, _| v.is_sending()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let second_key = view.read_with(&*visual, |v, _| {
        v.drafts
            .borrow()
            .get(key)
            .map(|draft| draft.send_key.clone())
    });
    assert_eq!(first_key, second_key, "the retry keeps client_message_id");
    let draft = view.read_with(&*visual, |v, _| {
        v.drafts.borrow().text(draft::ChatKey::channel(1, 7))
    });
    assert_eq!(draft, "первая\nвторая");
}

#[gpui::test]
async fn drafts_belong_to_the_chat_and_survive_switching(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_, app| {
        view.update(app, |v, _| {
            writable_workspace(v);
            v.channels.push(Channel {
                id: 8,
                kind: "text".into(),
                permissions: 3,
                ..Default::default()
            });
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.select_channel(7, window, cx));
    });
    focus_field(&mut visual, &view, |v| v.composer.clone());
    visual.simulate_input("черновик 7");

    visual.update(|window, app| {
        view.update(app, |v, cx| v.select_channel(8, window, cx));
    });
    let empty = view.read_with(&*visual, |v, cx| v.composer.read(cx).value().to_string());
    assert!(
        empty.is_empty(),
        "another channel starts with its own draft"
    );
    focus_field(&mut visual, &view, |v| v.composer.clone());
    visual.simulate_input("черновик 8");

    visual.update(|window, app| {
        view.update(app, |v, cx| v.select_channel(7, window, cx));
    });
    let restored = view.read_with(&*visual, |v, cx| v.composer.read(cx).value().to_string());
    assert_eq!(restored, "черновик 7");
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.drafts.borrow().text(draft::ChatKey::channel(1, 7)),
            "черновик 7"
        );
        assert_eq!(
            v.drafts.borrow().text(draft::ChatKey::channel(1, 8)),
            "черновик 8"
        );
    });
}

#[gpui::test]
async fn a_late_send_keeps_the_edited_draft_and_does_not_duplicate_ws_rows(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channel = 7;
            let key = draft::ChatKey::channel(1, 7);
            v.drafts.borrow_mut().set_text(key, "текст");
            let plan = v.drafts.borrow_mut().begin_send(key, "текст").unwrap();
            // The server row arrives over WS before the HTTP result.
            v.apply_realtime_event("message.created", &message_json(5, 1, "текст"));
            // The user edits the composer during the request.
            v.drafts.borrow_mut().set_text(key, "текст и ещё");
            let message = serde_json::from_value::<Message>(message_json(5, 1, "текст")).unwrap();
            let clear = v.apply_send_success(plan.clone(), message, "текст и ещё", cx);
            assert!(!clear, "an edited draft must not be cleared");
            assert_eq!(v.messages.len(), 1, "WS and HTTP converge to one row");
            assert_eq!(
                v.drafts.borrow().text(key),
                "текст и ещё",
                "the edit survives the late answer"
            );

            // A non-edited draft is cleared and reuses the same key.
            v.drafts.borrow_mut().set_text(key, "второй");
            let plan = v.drafts.borrow_mut().begin_send(key, "второй").unwrap();
            let message = serde_json::from_value::<Message>(message_json(6, 1, "второй")).unwrap();
            assert!(v.apply_send_success(plan, message, "второй", cx));
            assert_eq!(v.drafts.borrow().text(key), "");
            assert_eq!(v.messages.len(), 2);
            cx.notify();
        });
    });
}

#[gpui::test]
async fn paste_and_drop_add_only_metadata_to_the_draft(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let path = std::env::temp_dir().join(format!(
        "voxhold-paste-{}-{}.bin",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, vec![1u8; 4096]).unwrap();
    let drafts = draft::Drafts::shared();
    let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = slot.clone();
    let key = draft::ChatKey::channel(1, 7);
    visual.update(|window, app| {
        view.update(app, |v, _| writable_workspace(v));
        let (client, runtime) = view.read_with(app, |v, _| {
            (v.client.clone().expect("test client"), v.runtime.clone())
        });
        let panel = app
            .new(|cx| attachments::Attachments::new(client, runtime, drafts.clone(), window, cx));
        panel.update(app, |panel, cx| {
            panel.set_scope(Some(key), true, cx);
            assert!(panel.paste(
                crate::clipboard::Paste {
                    paths: vec![path.clone()],
                    image: None,
                },
                cx
            ));
        });
        *captured.borrow_mut() = Some(panel);
    });
    let panel = slot.borrow().clone().expect("attachments");
    wait_until(&mut visual, || !drafts.borrow().files(key).is_empty());
    let files = drafts.borrow().files(key);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].size, 4096);
    assert_eq!(files[0].path, path);
    assert!(!panel.read_with(&*visual, |panel, _| panel.is_reading()));
    std::fs::remove_file(path).unwrap();
}

mod fixes;
mod media;
mod r04;
mod review;
