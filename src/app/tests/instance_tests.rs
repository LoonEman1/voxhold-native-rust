//! The «Запомнить адрес» checkbox on the real auth window: startup from the
//! saved file, the CLI target, toggling, the confirmed write, a failed write
//! and «Назад». Every path is a fresh temporary directory, never `%APPDATA%`.

use super::*;
use crate::instance_prefs::{InstancePrefs, SAVE_ERROR};
use gpui::{point, px, size, Modifiers, TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-instance-ui-{}-{}-{name}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

fn click(visual: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>) {
    visual.simulate_mouse_move(position, None, Modifiers::default());
    visual.simulate_click(position, Modifiers::default());
}

fn toggle_remember(visual: &mut VisualTestContext) {
    draw(visual);
    let bounds = visual
        .debug_bounds("auth-remember-instance")
        .expect("the checkbox is painted on the address step");
    click(
        visual,
        point(
            bounds.origin.x + px(9.),
            bounds.origin.y + bounds.size.height / 2.0,
        ),
    );
    visual.run_until_parked();
}

fn open(
    cx: &mut TestAppContext,
    prefill: &str,
    dir: &Path,
    instance: PathBuf,
) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
    super::add_auth_window_with(cx, prefill, dir.join("workspace-layout.json"), instance)
}

fn value(view: &Entity<Voxhold>, visual: &VisualTestContext) -> String {
    view.read_with(visual, |v, cx| v.instance.read(cx).value().to_string())
}

fn remembered(view: &Entity<Voxhold>, visual: &VisualTestContext) -> bool {
    view.read_with(visual, |v, _| v.auth.remember_instance)
}

#[gpui::test]
async fn a_saved_record_fills_the_field_and_the_box(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("saved");
    let path = dir.join("instance.json");
    InstancePrefs {
        remember: true,
        instance: Some("http://127.0.0.1:8080".into()),
    }
    .save(&path)
    .unwrap();
    let (window, view) = open(cx, "", &dir, path);
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    let (screen, origin) = view.read_with(&*visual, |v, _| (v.auth.screen, v.auth.origin.clone()));
    assert_eq!(screen, auth::AuthScreen::Instance);
    assert_eq!(origin, "http://127.0.0.1:8080");
    assert_eq!(value(&view, &visual), "http://127.0.0.1:8080");
    assert!(remembered(&view, &visual));
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_cli_target_wins_over_the_saved_origin(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("cli");
    let path = dir.join("instance.json");
    InstancePrefs {
        remember: true,
        instance: Some("https://saved.example.org:8443".into()),
    }
    .save(&path)
    .unwrap();
    let (window, view) = open(cx, "https://cli.example.org:9443", &dir, path);
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    assert_eq!(value(&view, &visual), "https://cli.example.org:9443");
    let origin = view.read_with(&*visual, |v, _| v.auth.origin.clone());
    assert_eq!(origin, "https://cli.example.org:9443");
    // The checkbox still reflects the saved choice; the saved address does
    // not replace the explicit target.
    assert!(remembered(&view, &visual));
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn toggling_the_box_only_changes_the_state(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("toggle");
    let path = dir.join("instance.json");
    let (window, view) = open(cx, "", &dir, path.clone());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    assert!(!remembered(&view, &visual));

    toggle_remember(&mut visual);
    assert!(remembered(&view, &visual));
    assert!(
        !path.exists(),
        "toggling alone must not write the preference"
    );

    toggle_remember(&mut visual);
    assert!(!remembered(&view, &visual));
    assert!(!path.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_confirmed_checked_form_writes_the_canonical_origin(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("checked");
    let path = dir.join("instance.json");
    let (window, view) = open(cx, "", &dir, path.clone());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    super::focus_field(&mut visual, &view, |v| v.instance.clone());
    visual.simulate_input("http://127.0.0.1:8080/");
    toggle_remember(&mut visual);
    super::focus_field(&mut visual, &view, |v| v.instance.clone());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();

    let (screen, origin) = view.read_with(&*visual, |v, _| (v.auth.screen, v.auth.origin.clone()));
    assert_eq!(screen, auth::AuthScreen::Login);
    assert_eq!(origin, "http://127.0.0.1:8080");
    assert_eq!(value(&view, &visual), "http://127.0.0.1:8080");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"remember_instance\":true,\"instance\":\"http://127.0.0.1:8080\"}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn an_unchecked_confirmed_form_clears_the_saved_origin(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("cleared");
    let path = dir.join("instance.json");
    InstancePrefs {
        remember: true,
        instance: Some("https://saved.example.org".into()),
    }
    .save(&path)
    .unwrap();
    let (window, view) = open(cx, "", &dir, path.clone());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    assert!(remembered(&view, &visual));

    toggle_remember(&mut visual);
    assert!(!remembered(&view, &visual));
    super::focus_field(&mut visual, &view, |v| v.instance.clone());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"remember_instance\":false}"
    );
    assert_eq!(InstancePrefs::load(&path), InstancePrefs::default());
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_failed_write_keeps_the_canonical_origin_and_shows_a_readable_error(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("write-error");
    // The destination is a directory, so the atomic rename cannot replace it.
    let (window, view) = open(cx, "", &dir, dir.clone());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    super::focus_field(&mut visual, &view, |v| v.instance.clone());
    visual.simulate_input("http://127.0.0.1:8080");
    toggle_remember(&mut visual);
    super::focus_field(&mut visual, &view, |v| v.instance.clone());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();

    let (screen, origin, status) = view.read_with(&*visual, |v, _| {
        (v.auth.screen, v.auth.origin.clone(), v.auth.status.clone())
    });
    assert_eq!(screen, auth::AuthScreen::Login);
    assert_eq!(origin, "http://127.0.0.1:8080");
    assert_eq!(status, SAVE_ERROR);
    assert!(!status.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn back_keeps_the_confirmed_address_and_the_box(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("back");
    let path = dir.join("instance.json");
    let (window, view) = open(cx, "https://cli.example.org:9443", &dir, path);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    toggle_remember(&mut visual);
    assert!(remembered(&view, &visual));

    super::focus_field(&mut visual, &view, |v| v.instance.clone());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let confirmed = view.read_with(&*visual, |v, _| v.auth.origin.clone());
    assert_eq!(confirmed, "https://cli.example.org:9443");

    visual.update(|window, app| {
        view.update(app, |v, cx| v.back_to_instance(window, cx));
    });
    visual.run_until_parked();

    let (screen, origin, status) = view.read_with(&*visual, |v, _| {
        (v.auth.screen, v.auth.origin.clone(), v.auth.status.clone())
    });
    assert_eq!(screen, auth::AuthScreen::Instance);
    assert_eq!(origin, confirmed);
    assert_eq!(value(&view, &visual), confirmed);
    assert!(remembered(&view, &visual));
    assert!(status.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn back_is_ignored_while_a_login_request_is_pending(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("pending");
    let path = dir.join("instance.json");
    let (window, view) = open(cx, "http://127.0.0.1:1", &dir, path);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();
    toggle_remember(&mut visual);

    // The same state the form holds between Enter and the answer: the login
    // step is showing and the request is still pending.
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.auth.screen = auth::AuthScreen::Login;
            v.auth.origin = "http://127.0.0.1:1".into();
            v.auth.status.clear();
            v.auth.busy = true;
            cx.notify();
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.back_to_instance(window, cx));
    });
    visual.run_until_parked();

    let (screen, origin, busy) = view.read_with(&*visual, |v, _| {
        (v.auth.screen, v.auth.origin.clone(), v.auth.busy)
    });
    assert_eq!(screen, auth::AuthScreen::Login);
    assert_eq!(origin, "http://127.0.0.1:1");
    assert!(busy);
    assert!(remembered(&view, &visual));
    assert_eq!(value(&view, &visual), "http://127.0.0.1:1");
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn a_broken_record_does_not_break_the_start(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("broken");
    let path = dir.join("instance.json");
    std::fs::write(&path, b"{\"remember_instance\":true,\"instance\":").unwrap();
    let (window, view) = open(cx, "", &dir, path);
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    let screen = view.read_with(&*visual, |v, _| v.auth.screen);
    assert_eq!(screen, auth::AuthScreen::Instance);
    assert_eq!(value(&view, &visual), "");
    assert!(!remembered(&view, &visual));
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
async fn the_painted_box_stays_inside_the_card_at_every_breakpoint(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let dir = scratch("geometry");
    let path = dir.join("instance.json");
    let (window, view) = open(cx, "", &dir, path);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.run_until_parked();

    for (width, height) in [
        (390.0, 760.0),
        (560.0, 800.0),
        (720.0, 900.0),
        (1280.0, 900.0),
        (390.0, 600.0),
        (320.0, 360.0),
    ] {
        visual.simulate_resize(size(px(width), px(height)));
        draw(&mut visual);
        let (vw, vh) = view.read_with(&*visual, |v, _| v.viewport);
        assert!((vw - width).abs() < 1.0 && (vh - height).abs() < 1.0);
        let layout = auth::layout_auth(vw, vh, true, false);
        let bounds = visual
            .debug_bounds("auth-remember-instance")
            .unwrap_or_else(|| panic!("{width}x{height}: checkbox is painted"));
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        assert!(
            (x - layout.remember.x).abs() < 1.0
                && (y - layout.remember.y).abs() < 1.0
                && (w - layout.remember.w).abs() < 1.0
                && (h - layout.remember.h).abs() < 1.0,
            "{width}x{height}: painted ({x},{y},{w},{h}) vs layout {:?}",
            layout.remember
        );
        assert!(
            x >= layout.card.x
                && x + w <= layout.card.right()
                && y >= layout.user.bottom()
                && y + h <= layout.primary.y,
            "{width}x{height}: painted checkbox leaves its free row"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
