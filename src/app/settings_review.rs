//! Session-level save locks and avatar preview races from the settings review.
//! Kept separate from `settings_tests.rs` to respect the file size limit.

use super::settings_tests::{make_app, profile, release_on_arrival, wait_for};
use super::*;
use crate::test_support::{Gate, Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use std::time::Duration;

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
async fn a_successful_patch_refreshes_a_reopened_profile_but_keeps_new_edits(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server_profile = voxhold_api::Profile {
        about: "server about".into(),
        country_code: Some("RU".into()),
        ..profile("old")
    };
    let final_profile = voxhold_api::Profile {
        about: "server about".into(),
        country_code: Some("KZ".into()),
        ..profile("old")
    };
    let old_server = TestServer::spawn_concurrent(vec![Reply::new(
        200,
        &serde_json::to_string(&server_profile).unwrap(),
    )
    .with_gate(gate.clone())]);
    let new_server = TestServer::spawn(vec![
        Reply::new(200, &serde_json::to_string(&profile("old")).unwrap()),
        Reply::new(200, "[]"),
        Reply::new(200, &serde_json::to_string(&final_profile).unwrap()),
    ]);
    let mut old_client = voxhold_api::Client::new(&old_server.base).unwrap();
    old_client.set_token("token".into());
    let mut new_client = voxhold_api::Client::new(&new_server.base).unwrap();
    new_client.set_token("token".into());
    let (window, view) = make_app(cx, old_client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (gate, view_a, view_b) = (gate, view.clone(), view.clone());
    visual.update(|window, app| {
        view_a.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            let panel = v.personal.as_mut().unwrap();
            panel.profile = Some(profile("old"));
            panel
                .about
                .update(cx, |input, cx| input.set_value("typed new", window, cx));
            gate.arm();
            v.save_profile(window, cx);
        });
    });
    visual.update(|window, app| {
        view_b.update(app, |v, cx| {
            v.client = Some(new_client.clone());
            v.close_settings(false, window, cx);
            let focus = v.settings_entry_focus.clone();
            v.open_settings(SettingsKind::Personal, focus, window, cx);
        });
    });
    // Let the GET answer the old snapshot, then release the older PATCH.
    let release = std::thread::spawn(move || {
        let arrived = gate.wait_arrived(Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(250));
        gate.release();
        arrived
    });
    visual.run_until_parked();
    assert!(release.join().unwrap(), "the PATCH reached the old server");
    wait_for(&mut visual, &view, |v| v.active_profile_save.is_none());
    view.read_with(&*visual, |v, cx| {
        let panel = v.personal.as_ref().unwrap();
        assert_eq!(panel.profile.as_ref().unwrap().about, "server about");
        assert_eq!(
            panel.about.read(cx).value().to_string(),
            "server about",
            "the reopened form follows the confirmed PATCH snapshot"
        );
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let index = v
                .personal
                .as_ref()
                .unwrap()
                .countries
                .iter()
                .position(|country| country.code == "KZ")
                .map(|index| index + 1)
                .unwrap();
            v.settings_action(
                SettingsAct::Personal(personal_settings::Action::ChooseCountry(index)),
                window,
                cx,
            );
            v.save_profile(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.active_profile_save.is_none()
            && v.personal
                .as_ref()
                .and_then(|panel| panel.profile.as_ref())
                .is_some_and(|profile| profile.country_code.as_deref() == Some("KZ"))
    });
    let requests = new_server.requests();
    let patch = requests
        .iter()
        .find(|request| request.method == "PATCH")
        .expect("the country-only save sends one PATCH");
    let body = String::from_utf8_lossy(&patch.body);
    assert!(
        body.contains("server about"),
        "the next PATCH must not resend the old about text: {body}"
    );
    assert!(body.contains("KZ"), "the edited country is sent: {body}");
}

#[gpui::test]
async fn a_get_answer_keeps_an_inflight_patch_edit(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let get_gate = Gate::new();
    let patch_gate = Gate::new();
    let patched = voxhold_api::Profile {
        about: "patched about".into(),
        country_code: Some("RU".into()),
        ..profile("old")
    };
    let final_profile = voxhold_api::Profile {
        about: "patched about".into(),
        country_code: Some("KZ".into()),
        ..profile("old")
    };
    let get_server = TestServer::spawn_concurrent(vec![Reply::new(
        200,
        &serde_json::to_string(&profile("old")).unwrap(),
    )
    .with_gate(get_gate.clone())]);
    let patch_server = TestServer::spawn_concurrent(vec![
        Reply::new(200, &serde_json::to_string(&patched).unwrap()).with_gate(patch_gate.clone()),
        Reply::new(200, &serde_json::to_string(&final_profile).unwrap()),
    ]);
    let mut get_client = voxhold_api::Client::new(&get_server.base).unwrap();
    get_client.set_token("token".into());
    let mut patch_client = voxhold_api::Client::new(&patch_server.base).unwrap();
    patch_client.set_token("token".into());
    let (window, view) = make_app(cx, patch_client.clone());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (get_gate, patch_gate, view_a) = (get_gate, patch_gate, view.clone());
    visual.update(|window, app| {
        view_a.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            let panel = v.personal.as_mut().unwrap();
            panel.reopen(v.theme_mode, false);
            panel.profile = Some(profile("old"));
            panel.about_form.load("old".into());
            panel.country_form.load(Some("RU".into()));
            panel.country_index = crate::settings::country_index(&panel.countries, Some("RU"));
            panel
                .about
                .update(cx, |input, cx| input.set_value("old", window, cx));
            v.client = Some(get_client.clone());
            get_gate.arm();
            v.load_personal_profile(window, cx);
            v.client = Some(patch_client.clone());
            v.personal
                .as_mut()
                .unwrap()
                .about
                .update(cx, |input, cx| input.set_value("local edit", window, cx));
            patch_gate.arm();
            v.save_profile(window, cx);
        });
    });
    // The old GET answers first, while the PATCH is still in flight.
    let release = std::thread::spawn(move || {
        let get = get_gate.wait_arrived(Duration::from_secs(5));
        get_gate.release();
        let patch = patch_gate.wait_arrived(Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(200));
        patch_gate.release();
        (get, patch)
    });
    visual.run_until_parked();
    let (get, patch) = release.join().unwrap();
    assert!(get, "the GET reached the server");
    assert!(patch, "the PATCH reached the server");
    wait_for(&mut visual, &view, |v| v.active_profile_save.is_none());
    view.read_with(&*visual, |v, cx| {
        let panel = v.personal.as_ref().unwrap();
        assert_eq!(panel.profile.as_ref().unwrap().about, "patched about");
        assert_eq!(
            panel.about.read(cx).value().to_string(),
            "patched about",
            "the GET must not clear the PATCH dispatch or its confirmed answer"
        );
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let index = v
                .personal
                .as_ref()
                .unwrap()
                .countries
                .iter()
                .position(|country| country.code == "KZ")
                .map(|index| index + 1)
                .unwrap();
            v.settings_action(
                SettingsAct::Personal(personal_settings::Action::ChooseCountry(index)),
                window,
                cx,
            );
            v.save_profile(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.active_profile_save.is_none()
            && v.personal
                .as_ref()
                .and_then(|panel| panel.profile.as_ref())
                .is_some_and(|profile| profile.country_code.as_deref() == Some("KZ"))
    });
    let requests = patch_server.requests();
    let patch = &requests[1];
    let body = String::from_utf8_lossy(&patch.body);
    assert!(
        body.contains("patched about"),
        "the next PATCH must not resend the old about text: {body}"
    );
    assert!(body.contains("KZ"));
}

#[gpui::test]
async fn a_patch_answer_updates_the_profile_while_privacy_is_active(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server_profile = voxhold_api::Profile {
        about: "server about".into(),
        country_code: Some("RU".into()),
        ..profile("old")
    };
    let final_profile = voxhold_api::Profile {
        about: "server about".into(),
        country_code: Some("KZ".into()),
        ..profile("old")
    };
    let old_server = TestServer::spawn_concurrent(vec![Reply::new(
        200,
        &serde_json::to_string(&server_profile).unwrap(),
    )
    .with_gate(gate.clone())]);
    let new_server = TestServer::spawn(vec![Reply::new(
        200,
        &serde_json::to_string(&final_profile).unwrap(),
    )]);
    let mut old_client = voxhold_api::Client::new(&old_server.base).unwrap();
    old_client.set_token("token".into());
    let mut new_client = voxhold_api::Client::new(&new_server.base).unwrap();
    new_client.set_token("token".into());
    let (window, view) = make_app(cx, old_client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (gate, view_a, view_b) = (gate, view.clone(), view.clone());
    visual.update(|window, app| {
        view_a.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            let panel = v.personal.as_mut().unwrap();
            panel.profile = Some(profile("old"));
            panel
                .about
                .update(cx, |input, cx| input.set_value("old", window, cx));
            gate.arm();
            v.save_profile(window, cx);
        });
    });
    visual.update(|window, app| {
        view_b.update(app, |v, cx| {
            v.client = None;
            v.close_settings(false, window, cx);
            let panel = v.personal.as_mut().unwrap();
            panel.reopen(v.theme_mode, false);
            panel.profile = Some(profile("old"));
            panel.about_form.load("old".into());
            panel.country_form.load(Some("RU".into()));
            panel.country_index = crate::settings::country_index(&panel.countries, Some("RU"));
            panel
                .about
                .update(cx, |input, cx| input.set_value("old", window, cx));
            // The reopened GET already answered the old snapshot.
            panel.dialog.loads[0] = crate::settings::LoadState::Loaded;
            v.active_settings = Some(SettingsKind::Personal);
            v.activate_settings_tab(2, window, cx);
            v.client = Some(new_client.clone());
            assert_eq!(v.personal.as_ref().unwrap().tab(), 2);
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the PATCH reached the server");
    wait_for(&mut visual, &view, |v| v.active_profile_save.is_none());
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.personal.as_ref().unwrap().profile.as_ref().unwrap().about,
            "server about"
        );
        assert_eq!(v.personal.as_ref().unwrap().tab(), 2);
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.activate_settings_tab(0, window, cx);
            assert_eq!(
                v.personal.as_ref().unwrap().dialog.loads[0],
                crate::settings::LoadState::Loaded
            );
        });
    });
    view.read_with(&*visual, |v, cx| {
        assert_eq!(
            v.personal
                .as_ref()
                .unwrap()
                .about
                .read(cx)
                .value()
                .to_string(),
            "server about",
            "the return shows the confirmed PATCH snapshot without a new GET"
        );
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let index = v
                .personal
                .as_ref()
                .unwrap()
                .countries
                .iter()
                .position(|country| country.code == "KZ")
                .map(|index| index + 1)
                .unwrap();
            v.settings_action(
                SettingsAct::Personal(personal_settings::Action::ChooseCountry(index)),
                window,
                cx,
            );
            v.save_profile(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.active_profile_save.is_none()
            && v.personal
                .as_ref()
                .and_then(|panel| panel.profile.as_ref())
                .is_some_and(|profile| profile.country_code.as_deref() == Some("KZ"))
    });
    let requests = new_server.requests();
    let patch = requests
        .iter()
        .find(|request| request.method == "PATCH")
        .expect("the country-only save sends one PATCH");
    let body = String::from_utf8_lossy(&patch.body);
    assert!(
        body.contains("server about"),
        "the next PATCH must not resend the old about text: {body}"
    );
    assert!(body.contains("KZ"));
}

#[gpui::test]
async fn a_reopened_profile_keeps_a_new_edit_from_the_patch_snapshot(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let reply = voxhold_api::Profile {
        about: "server about".into(),
        country_code: Some("KZ".into()),
        ..profile("old")
    };
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &serde_json::to_string(&reply).unwrap(),
    )]);
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
                .update(cx, |input, cx| input.set_value("sent", window, cx));
            v.save_profile(window, cx);

            // Close the form before the PATCH task gets to run, then make a
            // newer local edit with the old GET snapshot as the baseline.
            v.close_settings(false, window, cx);
            let panel = v.personal.as_mut().unwrap();
            panel.reopen(v.theme_mode, false);
            panel.profile = Some(profile("old"));
            panel.about_form.load("old".into());
            panel.country_form.load(Some("RU".into()));
            panel.country_index = crate::settings::country_index(&panel.countries, Some("RU"));
            panel
                .about
                .update(cx, |input, cx| input.set_value("local edit", window, cx));
            v.active_settings = Some(SettingsKind::Personal);
        });
    });
    visual.run_until_parked();
    wait_for(&mut visual, &view, |v| v.active_profile_save.is_none());
    view.read_with(&*visual, |v, cx| {
        let panel = v.personal.as_ref().unwrap();
        assert_eq!(
            panel.about.read(cx).value().to_string(),
            "local edit",
            "a new local edit survives the older PATCH answer"
        );
        assert_eq!(panel.profile.as_ref().unwrap().about, "server about");
        assert_eq!(
            panel.profile.as_ref().unwrap().country_code.as_deref(),
            Some("KZ"),
            "fields that were not edited follow the confirmed PATCH"
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

    fn draft(width: u32, height: u32, pick: u64) -> crate::personal_settings::CropDraft {
        crate::personal_settings::CropDraft {
            data: std::sync::Arc::new(Vec::new()),
            source: std::sync::Arc::new(image::DynamicImage::ImageRgb8(image::RgbImage::new(
                width.max(1),
                height.max(1),
            ))),
            pick,
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
            // File B is selected, but decoding has not finished yet, so the
            // draft and the slider still belong to file A.
            panel.avatar_pick = 2;
            panel.crop = Some(draft(200, 200, 1));
            panel.dialog.generation
        })
    });
    let old_preview = preview(image::Rgba([255, 0, 0, 255]));
    let new_preview = preview(image::Rgba([0, 0, 255, 255]));
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            let panel = v.personal.as_mut().unwrap();
            panel.crop = Some(draft(300, 300, 2));
            if let Some(draft) = &mut panel.crop {
                // Moving the slider for B makes it token 2 as well.
                draft.preview_token = 2;
            }
            v.apply_crop_preview(generation, 2, 2, new_preview.clone(), cx);
        });
    });
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            // The preview computed from draft A after B was selected carries
            // A's file id and can coincidentally have B's token.
            v.apply_crop_preview(generation, 1, 2, old_preview.clone(), cx);
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
