//! VXTRAY: the window close button hides the existing window to the tray and
//! a real quit passes through the same callback.

use super::{add_auth_window, *};
use gpui::{TestAppContext, VisualTestContext};

#[gpui::test]
async fn the_close_hook_hides_with_the_tray_and_closes_without_it(cx: &mut TestAppContext) {
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

            // `quitting` is the real close, even when the icon is present.
            v.quitting = true;
            assert!(v.should_close_window(window, cx));
            v.quitting = false;

            // Without a registered icon the window must not be hidden.
            v.notification_tray.unregister(1);
            assert!(v.should_close_window(window, cx));
            assert!(v.error.contains("трей"));
            assert!(v.tray_task.is_none());
        });
    });
}
