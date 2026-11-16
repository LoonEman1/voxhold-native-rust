//! Session-level save locks and avatar preview races from the settings review.
//! Kept separate from `settings_tests.rs` to respect the file size limit.

use super::settings_tests::{make_app, profile, release_on_arrival, wait_for};
use super::*;
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};

#[gpui::test]
async fn reopening_the_profile_form_does_not_reset_its_patch_lock(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let first = serde_json::to_string(&profile("first answer")).unwrap();
    let second = serde_json::to_string(&profile("second answer")).unwrap();
    let server = TestServer::spawn(vec![
        Reply::new(200, &first).with_gate(gate.clone()),
        Reply::new(200, &second),
    ]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            let panel = v.personal.as_mut().unwrap();
            panel.profile = Some(profile("old"));
            panel
                .about
                .update(cx, |input, cx| input.set_value("first", window, cx));
            gate.arm();
            v.save_profile(window, cx);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.close_settings(false, window, cx);
            v.personal.as_mut().unwrap().reopen(v.theme_mode, false);
            v.active_settings = Some(SettingsKind::Personal);
            v.personal
                .as_mut()
                .unwrap()
                .about
                .update(cx, |input, cx| input.set_value("second", window, cx));
            // The first PATCH is still in flight; a reopened dialog must not
            // start a second write to the same profile.
            v.save_profile(window, cx);
            assert!(v.active_profile_save.is_some());
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the first PATCH reached the server");
    assert_eq!(
        server.requests().len(),
        1,
        "reopen did not send a second PATCH"
    );
    wait_for(&mut visual, &view, |v| v.active_profile_save.is_none());
    visual.update(|window, app| {
        view.update(app, |v, cx| v.save_profile(window, cx));
    });
    wait_for(&mut visual, &view, |v| {
        v.personal
            .as_ref()
            .and_then(|panel| panel.profile.as_ref())
            .is_some_and(|profile| profile.about == "second answer")
    });
    assert_eq!(
        server.requests().len(),
        2,
        "after the first answer the form can save again"
    );
}

#[gpui::test]
async fn notification_modes_are_frozen_while_the_put_is_pending(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![Reply::new(200, "{}").with_gate(gate.clone())]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let panel = v.channel_notifications.as_mut().unwrap();
            panel.reopen(1, 9, "qa");
            panel.mode = "all".into();
            panel.saved = "none".into();
            panel.loaded = true;
            v.active_settings = Some(SettingsKind::ChannelNotifications);
            gate.arm();
            v.save_channel_notifications(window, cx);
            v.notifications_action(channel_settings::NotifAction::Choose(2), window, cx);
            assert_eq!(v.channel_notifications.as_ref().unwrap().mode, "all");
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the PUT reached the server");
    wait_for(&mut visual, &view, |v| v.active_settings.is_none());
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.channel_notifications.as_ref().unwrap().saved, "all");
    });
}

#[gpui::test]
async fn reopening_notifications_does_not_reset_the_put_lock(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, "{}").with_gate(gate.clone()),
        Reply::new(200, "{}"),
    ]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let panel = v.channel_notifications.as_mut().unwrap();
            panel.reopen(1, 9, "qa");
            panel.mode = "all".into();
            panel.saved = "none".into();
            panel.loaded = true;
            v.active_settings = Some(SettingsKind::ChannelNotifications);
            gate.arm();
            v.save_channel_notifications(window, cx);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.close_settings(false, window, cx);
            let panel = v.channel_notifications.as_mut().unwrap();
            panel.reopen(1, 9, "qa");
            panel.mode = "none".into();
            panel.saved = String::new();
            panel.loaded = true;
            v.active_settings = Some(SettingsKind::ChannelNotifications);
            v.save_channel_notifications(window, cx);
            assert!(v.notification_saves.contains_key(&(1, 9)));
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the first PUT reached the server");
    assert_eq!(
        server.requests().len(),
        1,
        "reopen did not send a second PUT"
    );
    wait_for(&mut visual, &view, |v| {
        !v.notification_saves.contains_key(&(1, 9))
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| v.save_channel_notifications(window, cx));
    });
    wait_for(&mut visual, &view, |v| v.active_settings.is_none());
    assert_eq!(
        server.requests().len(),
        2,
        "after the first answer the form can save again"
    );
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.channel_notifications.as_ref().unwrap().saved,
            "none",
            "the second PUT is the one that closes the dialog"
        );
    });
}

#[gpui::test]
async fn a_late_preview_for_the_old_avatar_file_does_not_replace_the_new_one(
    cx: &mut TestAppContext,
) {
    fn preview(color: image::Rgba<u8>) -> std::sync::Arc<gpui::RenderImage> {
        std::sync::Arc::new(gpui::RenderImage::new(smallvec::smallvec![
            image::Frame::new(image::RgbaImage::from_pixel(4, 4, color))
        ]))
    }

    fn draft(width: u32, height: u32) -> crate::personal_settings::CropDraft {
        crate::personal_settings::CropDraft {
            data: std::sync::Arc::new(Vec::new()),
            source: std::sync::Arc::new(image::DynamicImage::ImageRgb8(image::RgbImage::new(
                width.max(1),
                height.max(1),
            ))),
            width,
            height,
            crop: crate::settings::Crop::default(),
            preview: None,
            preview_token: 1,
        }
    }

    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = make_app(cx, voxhold_api::Client::new("http://127.0.0.1:1").unwrap());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let generation = visual.update(|_window, app| {
        view.update(app, |v, _| {
            v.active_settings = Some(SettingsKind::Personal);
            let panel = v.personal.as_mut().unwrap();
            panel.reopen(v.theme_mode, false);
            panel.avatar_pick = 1;
            panel.crop = Some(draft(200, 200));
            panel.dialog.generation
        })
    });
    let old_preview = preview(image::Rgba([255, 0, 0, 255]));
    let new_preview = preview(image::Rgba([0, 0, 255, 255]));
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            let panel = v.personal.as_mut().unwrap();
            panel.avatar_pick = 2;
            panel.crop = Some(draft(300, 300));
            // The preview for the second file has the same `preview_token = 1`
            // as the first draft; only `avatar_pick` tells them apart.
            v.apply_crop_preview(generation, 2, 1, new_preview.clone(), cx);
        });
    });
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.apply_crop_preview(generation, 1, 1, old_preview.clone(), cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        let shown = v
            .personal
            .as_ref()
            .unwrap()
            .crop
            .as_ref()
            .unwrap()
            .preview
            .as_ref()
            .unwrap();
        assert!(
            std::sync::Arc::ptr_eq(shown, &new_preview),
            "the late preview of the first file must not replace the second"
        );
    });
}
