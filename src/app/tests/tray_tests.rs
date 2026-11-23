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
            v.notification_tray.set_proc_available(true);
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(!v.window_active);
            assert!(v.hidden_to_tray);

            // A conversation makes every other read gate pass; only the
            // hidden visibility flag may block the mark.
            v.conversation = Some(voxhold_api::Conversation {
                id: 99,
                direct: true,
                ..Default::default()
            });
            v.mark_visible_read(1, window, cx);
            assert!(!v.can_mark_visible_read());
            assert!(!v.read_pending);

            // `quitting` is the real close, even when the icon is present.
            v.quitting = true;
            assert!(v.should_close_window(window, cx));
            v.quitting = false;

            // Without a registered icon the window stays open: GPUI would drop
            // the error frame before it could be painted.
            v.show_main_window(window, cx);
            v.notification_tray.unregister(1);
            v.auth.status = "Ошибка входа.".into();
            assert!(!v.should_close_window(window, cx));
            assert!(v.error.contains("трей"));
            // The tray text is its own banner, not the login error.
            assert_eq!(v.auth.status, "Ошибка входа.");
            assert!(v.tray_exit_offered());
            assert!(v.tray_error_text().contains("трей"));

            // Another message may overwrite the general error; the exit path
            // is driven by the blocked flag and stays available.
            v.error = "Другая ошибка.".into();
            assert!(v.tray_exit_offered());
            assert!(v.tray_error_text().contains("трей"));

            // A registered tray (for example after a successful restore)
            // clears the banner and the redundant exit path.
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(!v.tray_exit_offered());
            assert!(v.tray_notice.is_empty());
        });
    });
}

#[gpui::test]
async fn a_hidden_window_ignores_activation_from_the_tray_menu(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.notifications.enabled = true;
            v.auth.username = "owner".into();
            v.notification_tray.set_proc_available(true);
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(v.hidden_to_tray);

            // `show_menu` calls SetForegroundWindow, so Win32 reports WA_ACTIVE
            // for the hidden HWND. It must not count as a visible window.
            v.window_activation_changed(true);
            assert!(!v.window_active);

            // A balloon is still offered; read marks are not taken from a
            // window that is not on screen.
            let event = message_event(1);
            assert!(v
                .notifications
                .offer(&event.kind, &event.data, &v.auth.username, v.window_active)
                .is_some());
            v.deliver_notification(&event, window);
            assert_eq!(v.notification_tray.test_balloon_calls(), 1);
            assert_eq!(v.notifications.shown, 1);
            v.conversation = Some(voxhold_api::Conversation {
                id: 99,
                direct: true,
                ..Default::default()
            });
            v.mark_visible_read(1, window, cx);
            assert!(!v.can_mark_visible_read());
            assert!(!v.read_pending);
        });
    });
}

#[gpui::test]
async fn quitting_retires_the_icon_and_clears_the_visible_state(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.notification_tray.set_proc_available(true);
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(v.notification_tray.is_available());

            v.quit_from_tray(window, cx);

            assert!(v.quitting);
            assert!(!v.notification_tray.is_registered());
            assert!(!v.notification_tray.is_available());
            assert_eq!(v.notification_tray.test_delete_calls(), 1);
            // A late Explorer restart or notification cannot resurrect it.
            assert!(v.notification_tray.register(1).is_err());
            assert!(v.notification_tray.restore(1).is_err());
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
            v.notification_tray.set_proc_available(true);
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(v.hidden_to_tray);

            // A successful re-registration keeps the hidden window hidden and
            // clears any stale tray failure.
            v.tray_action(TrayAction::Restore, window, cx);
            assert!(v.notification_tray.is_registered());
            assert!(v.hidden_to_tray);
            assert!(!v.window_active);
            assert!(!v.tray_exit_offered());

            // A failed one brings it back and keeps the exit path visible.
            v.notification_tray.set_test_add_result(false);
            v.tray_action(TrayAction::Restore, window, cx);
            assert!(!v.notification_tray.is_registered());
            assert!(!v.hidden_to_tray);
            assert!(!v.window_active);
            assert!(v.error.contains("восстановить"));
            assert!(v.tray_exit_offered());

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
            v.notification_tray.set_proc_available(true);
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(!v.window_active);

            v.deliver_notification(&message_event(1), window);
            assert_eq!(v.notification_tray.test_balloon_calls(), 1);
            assert_eq!(v.notifications.shown, 1);

            // Showing the window without foreground access still allows notifications.
            v.show_main_window(window, cx);
            v.deliver_notification(&message_event(2), window);
            assert_eq!(v.notification_tray.test_balloon_calls(), 2);
            assert_eq!(v.notifications.shown, 2);

            v.window_activation_changed(true);
            v.deliver_notification(&message_event(3), window);
            assert_eq!(v.notification_tray.test_balloon_calls(), 2);
            assert_eq!(v.notifications.shown, 2);
        });
    });
}

#[gpui::test]
async fn a_balloon_registration_clears_a_stale_tray_error(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.notifications.enabled = true;
            v.auth.username = "owner".into();
            v.notification_tray.set_proc_available(true);
            // The window procedure is live, but the first registration fails.
            v.notification_tray.set_test_add_result(false);
            assert!(v.notification_tray.register(1).is_err());
            assert!(!v.should_close_window(window, cx));
            assert!(v.tray_exit_offered());
            assert!(!v.tray_notice.is_empty());

            // The hidden window's balloon retries the registration and wins.
            v.notification_tray.set_test_add_result(true);
            v.window_active = false;
            v.deliver_notification(&message_event(1), window);
            assert!(v.notification_tray.is_registered());
            assert!(v.notification_tray.is_available());
            assert!(!v.tray_exit_offered());
            assert!(v.tray_notice.is_empty());
            assert!(v.error.is_empty());
            assert_eq!(v.notification_tray.test_balloon_calls(), 1);
        });
    });
}

#[gpui::test]
async fn a_destroyed_window_invalidates_the_tray_icon(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.notifications.enabled = true;
            v.auth.username = "owner".into();
            v.notification_tray.set_proc_available(true);
            v.notification_tray.set_test_add_result(true);
            v.notification_tray.register(1).unwrap();
            assert!(!v.should_close_window(window, cx));
            assert!(v.hidden_to_tray);
            v.show_main_window(window, cx);
            assert!(!v.hidden_to_tray);

            // `WM_NCDESTROY` invalidates the state instead of trusting a flag
            // whose HWND is gone.
            v.tray_action(TrayAction::WindowGone(1), window, cx);
            assert!(!v.notification_tray.is_registered());
            assert!(!v.notification_tray.is_available());

            // The close hook is blocked again and offers the exit button.
            assert!(!v.should_close_window(window, cx));
            assert!(v.tray_exit_offered());

            // A balloon must not re-add the icon for the dead window.
            let adds = v.notification_tray.test_add_calls();
            let balloons = v.notification_tray.test_balloon_calls();
            v.window_active = false;
            v.deliver_notification(&message_event(2), window);
            assert_eq!(v.notification_tray.test_add_calls(), adds);
            assert_eq!(v.notification_tray.test_balloon_calls(), balloons);
            assert!(!v.notification_tray.is_registered());
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

            // Inactive: a foreground request can leave the window inactive.
            v.window_active = false;
            v.show_main_window(window, cx);
            assert!(!v.window_active);

            // Hidden: the same call restores it and clears the flag.
            v.hidden_to_tray = true;
            v.window_active = false;
            v.show_main_window(window, cx);
            assert!(!v.window_active);
            assert!(!v.hidden_to_tray);
        });
    });
}
