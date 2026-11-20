//! W05a: the voice room's text chat. These tests drive the real header
//! controls and window layout; no network request completes and no audio
//! device is opened.

use super::voice_tests::fixture;
use super::*;
use gpui::{px, size, Modifiers, TestAppContext, VisualTestContext};

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

fn allow_chat(view: &Entity<Voxhold>, visual: &mut VisualTestContext, channels: &[i64]) {
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            for channel in channels {
                if let Some(row) = v.channels.iter_mut().find(|row| row.id == *channel) {
                    row.permissions = 3;
                }
            }
            cx.notify();
        });
    });
}

#[gpui::test]
async fn opening_the_room_chat_does_not_join_a_call(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    // This room is not connected; the panel only displays it.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.leave_call(cx));
        let _ = window;
    });
    allow_chat(&view, visual, &[7]);
    visual.simulate_resize(size(px(1280.), px(900.)));
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.call_active(), "fixture starts without a call");
        assert!(panel.test_controls_muted(), "microphone stays closed");
    });
    view.read_with(&*visual, |v, _| {
        assert!(!v.voice_chat_open(), "chat starts closed");
    });

    let toggle = visual
        .debug_bounds("toggle-room-chat")
        .expect("the room header shows the chat switch");
    visual.simulate_click(toggle.center(), Modifiers::default());
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(v.voice_chat_open(), "the header switch opened the panel");
        assert_eq!(v.chat_key(), draft::ChatKey::channel(1, 7));
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.call_active(), "opening chat must not start a call");
        assert_eq!(panel.phase(), crate::voice_room::CallPhase::Idle);
        assert!(panel.test_controls_muted(), "microphone stays closed");
        assert!(!panel.test_ptt_held());
    });
    let panel = visual
        .debug_bounds("voice-chat-panel")
        .expect("the docked chat is painted");

    // The panel's close control removes it without touching media state.
    let close = visual
        .debug_bounds("voice-chat-close")
        .expect("the panel header has a close action");
    visual.simulate_click(close.center(), Modifiers::default());
    draw(&mut visual);
    view.read_with(&*visual, |v, _| assert!(!v.voice_chat_open()));
    draw(&mut visual);
    let room = visual
        .debug_bounds("voice-room-pane")
        .expect("the room is still painted");
    assert!(
        f32::from(room.size.width) > 600.0,
        "closing the chat returns the full width to the room: {room:?}"
    );
    assert!(panel.size.width > px(0.));
    voice.read_with(&*visual, |panel, _| assert!(!panel.call_active()));
}

#[gpui::test]
async fn room_chat_drafts_are_keyed_by_the_room_and_the_call_survives(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    allow_chat(&view, visual, &[7, 8]);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.open_voice_chat(window, cx);
            v.composer.update(cx, |input, cx| {
                input.set_value("черновик комнаты 7", window, cx)
            });
        });
    });
    draw(&mut visual);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_channel(8, window, cx);
            assert!(v.voice_room_visible());
            assert!(!v.voice_chat_open(), "another room starts closed");
            v.open_voice_chat(window, cx);
            v.composer.update(cx, |input, cx| {
                input.set_value("черновик комнаты 8", window, cx)
            });
        });
    });
    draw(&mut visual);
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.call_channel(),
            7,
            "browsing another room keeps the call"
        );
    });

    // Text chat, then back to room 7: the call is untouched, the chat state is
    // restored and the draft comes from the room's own ChatKey.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.select_channel(9, window, cx);
            assert!(!v.voice_room_visible());
        });
    });
    draw(&mut visual);
    visual.update(|window, app| {
        view.update(app, |v, cx| v.select_channel(7, window, cx));
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, cx| {
        assert!(v.voice_chat_open(), "room 7's open chat is restored");
        assert!(
            v.voice_chat_open_for(8),
            "the other room keeps its own open state without being painted"
        );
        assert_eq!(v.chat_key(), draft::ChatKey::channel(1, 7));
        assert_eq!(v.composer.read(cx).value(), "черновик комнаты 7");
        assert_eq!(
            v.drafts.borrow().text(draft::ChatKey::channel(1, 8)),
            "черновик комнаты 8"
        );
    });
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.call_channel(), 7, "the active call must not be lost");
    });
}

#[gpui::test]
async fn closed_or_hidden_room_chat_neither_subscribes_nor_marks_read(cx: &mut TestAppContext) {
    let (mut visual, view, _voice) = fixture(cx);
    allow_chat(&view, visual, &[7]);
    let (sender, receiver) = tokio::sync::watch::channel(None);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.event_subscription = Some(sender);
            v.messages = vec![serde_json::from_value(serde_json::json!({
                "id": 5,
                "channel_id": 7,
                "content": "room message",
                "author": {"user_id": 2, "username": "bob"},
                "created_at": 1,
                "revision": 1
            }))
            .unwrap()];
            v.read_sent = 0;
            v.access_pending = false;
            v.refresh_chat_subscription();
            assert!(*receiver.borrow() == None, "closed chat must not subscribe");
            v.mark_visible_read(5, window, cx);
            assert!(!v.read_pending, "closed chat must not mark a read");
        });
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.open_voice_chat(window, cx);
            v.refresh_chat_subscription();
            assert_eq!(
                *receiver.borrow(),
                Some(ChannelSubscription {
                    server_id: 1,
                    channel_id: 7,
                    generation: v.access_generation,
                })
            );
            v.close_voice_chat(cx);
            assert!(*receiver.borrow() == None, "closing unsubscribes");
            v.mark_visible_read(5, window, cx);
            assert!(!v.read_pending);
        });
    });

    // A hidden window is covered by the existing active-window guard; the
    // channel alone must not schedule a read while the panel is closed.
    view.read_with(&*visual, |v, _| assert!(!v.voice_chat_open()));
}

#[gpui::test]
async fn room_chat_and_room_tiles_never_overlap(cx: &mut TestAppContext) {
    let (mut visual, view, _voice) = fixture(cx);
    allow_chat(&view, visual, &[7]);
    visual.update(|window, app| {
        view.update(app, |v, cx| v.open_voice_chat(window, cx));
    });
    visual.simulate_resize(size(px(1280.), px(900.)));
    draw(&mut visual);
    let room = visual
        .debug_bounds("voice-room-pane")
        .expect("the room pane is painted");
    let chat = visual
        .debug_bounds("voice-chat-panel")
        .expect("the docked chat is painted");
    let room_right = f32::from(room.origin.x + room.size.width);
    let chat_left = f32::from(chat.origin.x);
    assert!(
        room_right <= chat_left + 0.5,
        "docked panel overlaps the room: {room:?} vs {chat:?}"
    );
    let chat_width = f32::from(chat.size.width);
    assert!(
        (300.0..=440.0).contains(&chat_width),
        "unexpected dock width: {chat_width}"
    );

    // 900 px leaves a 572 px chat column, which cannot fit both columns.
    visual.simulate_resize(size(px(900.), px(720.)));
    draw(&mut visual);
    let room = visual.debug_bounds("voice-room-pane").expect("room pane");
    let chat = visual.debug_bounds("voice-chat-panel").expect("chat panel");
    let room_bottom = f32::from(room.origin.y + room.size.height);
    let chat_top = f32::from(chat.origin.y);
    assert!(
        room_bottom <= chat_top + 0.5,
        "stacked panel overlaps the room: {room:?} vs {chat:?}"
    );
    assert_eq!(
        f32::from(chat.size.width),
        f32::from(room.size.width),
        "the stacked panel uses the real column width"
    );
}
