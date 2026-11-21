//! VXTRAY: the window close button hides the existing window to the tray and
//! a real quit passes through the same callback. Every Win32 call in the tray
//! path has a `cfg(test)` seam, so these tests never touch the shell or HWNDs.

use super::tray::TrayAction;
use super::*;
use gpui::{TestAppContext, VisualTestContext};

fn message_event(id: i64) -> voxhold_api::RealtimeEvent {
    voxhold_api::RealtimeEvent {
        kind: "notification.message".into(),
        data: serde_json::json!({
            "channel_name": "qa-chat",
            "server_name": "QA",
            "message": {
                "id": id,
                "content": "hello",
                "author": {"username": "neva"}
            }
        }),
        request_id: String::new(),
    }
}

#[gpui::test]
async fn the_close_hook_hides_with_the_tray_and_keeps_the_window_without_it(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            // The fake registration replaces Shell_NotifyIcon in tests.
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(!v.window_active);
            assert!(v.hidden_to_tray);

            // A hidden window must not take a read mark from painted rows.
            v.mark_visible_read(1, window, cx);
            assert!(!v.read_pending);

            // `quitting` is the real close, even when the icon is present.
            v.quitting = true;
            assert!(v.should_close_window(window, cx));
            v.quitting = false;

            // Without a registered icon the window stays open: GPUI would drop
            // the error frame before it could be painted.
            v.show_main_window(window, cx);
            v.notification_tray.unregister(1);
            assert!(!v.should_close_window(window, cx));
            assert!(v.error.contains("трей"));
            assert_eq!(v.auth.status, v.error);
            assert!(v.tray_exit_offered());
            assert!(v.tray_task.is_none());
        });
    });
}

#[gpui::test]
async fn taskbar_restore_only_brings_back_a_hidden_window(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(v.hidden_to_tray);

            // A successful re-registration keeps the hidden window hidden.
            v.tray_action(TrayAction::Restore, window, cx);
            assert!(v.notification_tray.is_registered());
            assert!(v.hidden_to_tray);
            assert!(!v.window_active);

            // A failed one brings it back.
            v.notification_tray.set_test_add_result(false);
            v.tray_action(TrayAction::Restore, window, cx);
            assert!(!v.notification_tray.is_registered());
            assert!(!v.hidden_to_tray);
            assert!(v.window_active);
            assert!(v.error.contains("восстановить"));

            // A visible, merely inactive window is not activated and steals
            // no focus; only the message changes.
            v.window_active = false;
            v.error.clear();
            v.tray_action(TrayAction::RestoreFailed, window, cx);
            assert!(!v.window_active);
            assert!(!v.hidden_to_tray);
            assert!(v.error.contains("восстановить"));
        });
    });
}

#[gpui::test]
async fn a_hidden_window_shows_balloons_and_an_active_one_suppresses_them(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.notifications.enabled = true;
            v.auth.username = "owner".into();
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(!v.window_active);

            v.deliver_notification(&message_event(1), window);
            assert_eq!(v.notification_tray.test_balloon_calls(), 1);
            assert_eq!(v.notifications.shown, 1);

            // The restored window is active again: the next event is silent.
            v.show_main_window(window, cx);
            v.deliver_notification(&message_event(2), window);
            assert_eq!(v.notification_tray.test_balloon_calls(), 1);
            assert_eq!(v.notifications.shown, 1);
        });
    });
}

#[gpui::test]
async fn show_main_window_is_idempotent(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            // Visible and active: the duplicate call is a no-op.
            assert!(v.window_active);
            v.show_main_window(window, cx);
            assert!(v.window_active);
            assert!(!v.hidden_to_tray);

            // Inactive: the window is brought forward.
            v.window_active = false;
            v.show_main_window(window, cx);
            assert!(v.window_active);

            // Hidden: the same call restores it and clears the flag.
            v.hidden_to_tray = true;
            v.window_active = false;
            v.show_main_window(window, cx);
            assert!(v.window_active);
            assert!(!v.hidden_to_tray);
        });
    });
}
