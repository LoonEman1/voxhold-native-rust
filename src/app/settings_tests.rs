//! Regression tests for settings races: late answers, closed tabs, local edits
//! during a request, 403 and deleted resources. HTTP runs against the real
//! `TestServer` with `Gate`, the same path as the application.

use super::*;
use crate::settings::LoadState;
use crate::test_support::{Gate, Reply, TestServer};
use crate::voice_settings::Device;
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;
use std::time::Duration;

pub(super) fn profile(about: &str) -> voxhold_api::Profile {
    voxhold_api::Profile {
        user_id: 1,
        username: "owner".into(),
        about: about.into(),
        country_code: Some("RU".into()),
        created_at: 1_700_000_000,
    }
}

pub(super) fn make_app(
    cx: &mut TestAppContext,
    client: voxhold_api::Client,
) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
    let shell = std::env::temp_dir().join(format!(
        "voxhold-settings-test-{}-{}.json",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let slot: std::rc::Rc<std::cell::RefCell<Option<Entity<Voxhold>>>> = Default::default();
    let captured = slot.clone();
    let window = cx.add_window(move |window, cx| {
        let view = cx.new(|cx| {
            let mut view = Voxhold::new(window, cx, "", Some(shell));
            view.client = Some(client);
            view.self_user_id = 1;
            view.servers = vec![voxhold_api::Server {
                id: 1,
                name: "QA".into(),
                role: "owner".into(),
            }];
            view.server = 1;
            view.personal = Some(personal_settings::PersonalSettings::new(window, cx));
            view.server_settings = Some(server_settings::ServerSettings::new(window, cx));
            view.channel_settings = Some(channel_settings::ChannelSettings::new(window, cx));
            view.channel_notifications = Some(channel_settings::ChannelNotifications::new());
            view
        });
        *captured.borrow_mut() = Some(view.clone());
        Root::new(view.into(), window, cx)
    });
    let view = slot.borrow().clone().expect("view");
    (window, view)
}

/// Release the gate as soon as the request reaches the server, without
/// blocking the GPUI test thread that has to drive the background task.
pub(super) fn release_on_arrival(gate: std::sync::Arc<Gate>) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let arrived = gate.wait_arrived(Duration::from_secs(5));
        gate.release();
        arrived
    })
}

pub(super) fn wait_for(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    mut done: impl FnMut(&Voxhold) -> bool,
) {
    for _ in 0..1000 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| done(v)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("condition did not become true");
}

#[gpui::test]
async fn a_late_profile_answer_does_not_overwrite_a_new_edit(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let reply = serde_json::to_string(&profile("server answer")).unwrap();
    let server = TestServer::spawn(vec![Reply::new(200, &reply).with_gate(gate.clone())]);
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
                .update(cx, |input, cx| input.set_value("old", window, cx));
            gate.arm();
            v.save_profile(window, cx);
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal.as_mut().unwrap().about.update(cx, |input, cx| {
                input.set_value("typed after dispatch", window, cx)
            });
        });
    });
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the PUT reached the server");
    wait_for(&mut visual, &view, |v| {
        v.personal
            .as_ref()
            .is_some_and(|panel| panel.dialog.pending == 0)
    });
    view.read_with(&*visual, |v, cx| {
        let panel = v.personal.as_ref().unwrap();
        assert_eq!(
            panel.about.read(cx).value().to_string(),
            "typed after dispatch",
            "a late answer must keep the newer local edit"
        );
        assert_eq!(panel.profile.as_ref().unwrap().about, "server answer");
    });
}

#[gpui::test]
async fn a_profile_patch_updates_the_snapshot_while_another_tab_is_active(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let reply = serde_json::to_string(&profile("late")).unwrap();
    let server = TestServer::spawn(vec![Reply::new(200, &reply).with_gate(gate.clone())]);
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
                .update(cx, |input, cx| input.set_value("old", window, cx));
            gate.arm();
            v.save_profile(window, cx);
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            // Move to the privacy tab, which invalidates tab 0 requests.
            v.personal.as_mut().unwrap().dialog.activate(2);
            cx.notify();
        });
    });
    visual.run_until_parked();
    assert!(waiter.join().unwrap());
    view.read_with(&*visual, |v, cx| {
        let panel = v.personal.as_ref().unwrap();
        assert_eq!(panel.about.read(cx).value().to_string(), "late");
        assert_eq!(panel.profile.as_ref().unwrap().about, "late");
        assert!(panel.notice.is_empty(), "the hidden tab shows no notice");
        assert_eq!(panel.dialog.pending, 0, "old tab did not leak pending");
    });
}

#[gpui::test]
async fn a_403_keeps_the_editor_value_and_shows_the_error(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![Reply::new(403, "{\"error\":\"forbidden\"}")]);
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
                .update(cx, |input, cx| input.set_value("edited", window, cx));
            v.save_profile(window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.personal
            .as_ref()
            .is_some_and(|panel| panel.dialog.pending == 0)
    });
    view.read_with(&*visual, |v, cx| {
        let panel = v.personal.as_ref().unwrap();
        assert_eq!(panel.about.read(cx).value().to_string(), "edited");
        assert!(panel.local_error.contains("403"));
        assert_eq!(panel.profile.as_ref().unwrap().about, "old");
    });
}

#[gpui::test]
async fn a_late_notification_save_after_close_is_ignored(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![Reply::new(200, "{}").with_gate(gate.clone())]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.channel_notifications.as_mut().unwrap().reopen(1, 9, "qa");
            let panel = v.channel_notifications.as_mut().unwrap();
            panel.mode = "all".into();
            panel.saved = "mentions".into();
            v.active_settings = Some(SettingsKind::ChannelNotifications);
            gate.arm();
            v.save_channel_notifications(_window, cx);
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.close_settings(false, window, cx);
        });
    });
    visual.run_until_parked();
    assert!(waiter.join().unwrap());
    view.read_with(&*visual, |v, _| {
        let panel = v.channel_notifications.as_ref().unwrap();
        assert_eq!(panel.saved, "mentions");
        assert!(panel.dialog.is_closed());
    });
}

#[gpui::test]
async fn a_role_event_updates_and_sorts_the_roster(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = make_app(cx, voxhold_api::Client::new("http://127.0.0.1:1").unwrap());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            v.members = vec![
                voxhold_api::Member {
                    user_id: 2,
                    username: "beta".into(),
                    role: "member".into(),
                    ..Default::default()
                },
                voxhold_api::Member {
                    user_id: 3,
                    username: "Alpha".into(),
                    role: "member".into(),
                    ..Default::default()
                },
            ];
            v.apply_realtime_event(
                "server.member_role_updated",
                &serde_json::json!({
                    "server_id": 1,
                    "member": {
                        "user_id": 2,
                        "username": "beta",
                        "role": "admin",
                        "avatar_version": null,
                        "last_seen_at": null
                    }
                }),
            );
        });
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.members[0].username, "Alpha", "case-insensitive sort");
        assert_eq!(v.members[1].role, "admin");
    });
}

#[gpui::test]
async fn a_missing_channel_closes_its_settings_form(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    // The reload asks for channels and members; both are empty.
    let server = TestServer::spawn(vec![Reply::new(200, "[]"), Reply::new(200, "[]")]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.generation = 1;
            v.access_generation = 1;
            v.channel_settings
                .as_mut()
                .unwrap()
                .reopen(1, 9, "text", "qa", true, _window, cx);
            v.active_settings = Some(SettingsKind::Channel);
            v.reload_channels(_window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| v.active_settings.is_none());
}

#[gpui::test]
async fn every_settings_tab_renders_without_touching_the_network(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let client = voxhold_api::Client::new("http://127.0.0.1:1").unwrap();
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            let panel = v.personal.as_mut().unwrap();
            panel.profile = Some(profile("about"));
            panel.privacy = Some(voxhold_api::MessagePrivacy {
                mode: "shared".into(),
                blocked_user_ids: vec![2],
            });
            panel.sessions = vec![voxhold_api::AccountSession {
                id: 1,
                user_agent: "test".into(),
                client_ip: "127.0.0.1".into(),
                current: true,
                created_at: 1_700_000_000,
                last_used_at: Some(1_700_000_100),
            }];
            v.active_settings = Some(SettingsKind::Personal);
            cx.notify();
        });
    });
    for tab in [0, 1, 2] {
        visual.update(|window, app| {
            view.update(app, |v, cx| {
                v.personal.as_mut().unwrap().dialog.active = tab;
                v.settings_scroll = gpui::ScrollHandle::new();
                let _ = window;
                cx.notify();
            });
        });
        visual.run_until_parked();
        view.read_with(&*visual, |v, _| {
            assert!(!v.settings_ring.is_empty(), "tab {tab} has focus targets");
        });
    }

    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Server);
            let panel = v.server_settings.as_mut().unwrap();
            panel.server = 1;
            panel.general_loaded = true;
            panel.roles_loaded = true;
            panel.roles = vec![voxhold_api::CustomRole {
                id: 1,
                name: "Mods".into(),
                color: "#8b93a7".into(),
                permissions: 15,
                user_ids: vec![2],
            }];
            panel.role_edit = Some(server_settings::RoleEdit::new_role());
            panel.members = vec![voxhold_api::Member {
                user_id: 2,
                username: "neva".into(),
                role: "member".into(),
                ..Default::default()
            }];
            panel.storage = Some(voxhold_api::StorageState::default());
            panel.storage_loaded = true;
            panel.bans = vec![voxhold_api::ServerBan {
                user_id: 2,
                username: "neva".into(),
                reason: "spam".into(),
                created_at: 1,
            }];
            panel.bans_loaded = true;
            cx.notify();
        });
    });
    for tab in 0..4 {
        visual.update(|_window, app| {
            view.update(app, |v, cx| {
                v.server_settings.as_mut().unwrap().dialog.active = tab;
                cx.notify();
            });
        });
        visual.run_until_parked();
        view.read_with(&*visual, |v, _| assert!(!v.settings_ring.is_empty()));
    }

    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.channels = vec![voxhold_api::Channel {
                id: 9,
                name: "qa".into(),
                kind: "news".into(),
                permissions: 3,
                ..Default::default()
            }];
            v.channel_settings
                .as_mut()
                .unwrap()
                .reopen(1, 9, "news", "qa", true, _window, cx);
            let panel = v.channel_settings.as_mut().unwrap();
            panel.access = Some(voxhold_api::ChannelAccess::default());
            panel.news = Some(voxhold_api::NewsSettings::default());
            panel.members = vec![voxhold_api::Member {
                user_id: 2,
                username: "neva".into(),
                role: "member".into(),
                ..Default::default()
            }];
            v.active_settings = Some(SettingsKind::Channel);
            cx.notify();
        });
    });
    for tab in 0..3 {
        visual.update(|_window, app| {
            view.update(app, |v, cx| {
                v.channel_settings.as_mut().unwrap().dialog.active = tab;
                cx.notify();
            });
        });
        visual.run_until_parked();
        view.read_with(&*visual, |v, _| assert!(!v.settings_ring.is_empty()));
    }

    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.channel_notifications.as_mut().unwrap().reopen(1, 9, "qa");
            let panel = v.channel_notifications.as_mut().unwrap();
            panel.mode = "mentions".into();
            panel.saved = "mentions".into();
            panel.loaded = true;
            v.active_settings = Some(SettingsKind::ChannelNotifications);
            cx.notify();
        });
    });
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| assert!(!v.settings_ring.is_empty()));
}

#[test]
fn gate_releases_a_patch_answer() {
    let reply = serde_json::to_string(&profile("gated")).unwrap();
    let gate = Gate::new();
    let server =
        TestServer::spawn_concurrent(vec![Reply::new(200, &reply).with_gate(gate.clone())]);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = voxhold_api::Client::new(&server.base).unwrap();
    gate.arm();
    let handle = runtime.spawn(async move { client.update_profile("about", "RU").await });
    assert!(gate.wait_arrived(Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(50));
    gate.release();
    let result = runtime.block_on(handle).unwrap();
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn profile_patch_reaches_the_test_server() {
    let reply = serde_json::to_string(&profile("direct")).unwrap();
    let server = TestServer::spawn(vec![Reply::new(200, &reply)]);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = voxhold_api::Client::new(&server.base).unwrap();
    let result = runtime.block_on(client.update_profile("about", "RU"));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(result.unwrap().about, "direct");
}

#[test]
fn device_choices_and_voice_prefs_are_local() {
    let mut prefs = crate::voice_settings::VoicePrefs::default();
    prefs.input = "missing".into();
    let devices = vec![Device {
        id: "mic".into(),
        name: "Mic".into(),
        input: true,
    }];
    let (_, _, current) = crate::voice_settings::device_choices(&prefs, &devices, true);
    assert_eq!(current, crate::voice_settings::UNAVAILABLE_DEVICE);
}

#[gpui::test]
async fn an_access_answer_for_another_channel_never_reaches_the_new_form(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let old_gate = Gate::new();
    let old_server = TestServer::spawn_concurrent(vec![Reply::new(
        200,
        r#"{"restricted":true,"user_ids":[2],"roles":["member"]}"#,
    )
    .with_gate(old_gate.clone())]);
    let new_server = TestServer::spawn(vec![Reply::new(
        200,
        r#"{"restricted":false,"user_ids":[],"roles":[]}"#,
    )]);
    let mut old_client = voxhold_api::Client::new(&old_server.base).unwrap();
    old_client.set_token("token".into());
    let mut new_client = voxhold_api::Client::new(&new_server.base).unwrap();
    new_client.set_token("token".into());
    let (window, view) = make_app(cx, old_client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (old_gate, view_a, view_b) = (old_gate, view.clone(), view.clone());
    visual.update(|window, app| {
        view_a.update(app, |v, cx| {
            v.channel_settings
                .as_mut()
                .unwrap()
                .reopen(1, 9, "text", "A", true, window, cx);
            v.channel_settings.as_mut().unwrap().dialog.activate(1);
            v.active_settings = Some(SettingsKind::Channel);
            old_gate.arm();
            v.load_channel_tab(1, window, cx);
        });
    });
    visual.update(|window, app| {
        view_b.update(app, |v, cx| {
            // A different origin makes it impossible for the old answer to
            // consume the reply meant for B.
            v.client = Some(new_client.clone());
            v.close_settings(false, window, cx);
            v.channel_settings
                .as_mut()
                .unwrap()
                .reopen(1, 10, "text", "B", true, window, cx);
            v.channel_settings.as_mut().unwrap().dialog.activate(1);
            v.active_settings = Some(SettingsKind::Channel);
            v.load_channel_tab(1, window, cx);
            assert!(
                v.channel_settings.as_ref().unwrap().access.is_none(),
                "the save button stays unavailable until B answers"
            );
        });
    });
    // Let B answer first, then deliver the stale A response.
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        old_gate.release();
    });
    visual.run_until_parked();
    release.join().unwrap();
    wait_for(&mut visual, &view, |v| {
        v.channel_settings
            .as_ref()
            .is_some_and(|panel| panel.access.is_some())
    });
    view.read_with(&*visual, |v, _| {
        let access = v.channel_settings.as_ref().unwrap().access.clone().unwrap();
        assert!(
            !access.restricted && access.user_ids.is_empty(),
            "the late answer of channel A must not replace channel B rights"
        );
    });
}

#[gpui::test]
async fn reopening_the_same_channel_gets_a_fresh_generation(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let old_gate = Gate::new();
    let old_server = TestServer::spawn_concurrent(vec![Reply::new(
        200,
        r#"{"restricted":true,"user_ids":[2],"roles":["member"]}"#,
    )
    .with_gate(old_gate.clone())]);
    let new_server = TestServer::spawn(vec![Reply::new(
        200,
        r#"{"restricted":false,"user_ids":[],"roles":[]}"#,
    )]);
    let mut old_client = voxhold_api::Client::new(&old_server.base).unwrap();
    old_client.set_token("token".into());
    let mut new_client = voxhold_api::Client::new(&new_server.base).unwrap();
    new_client.set_token("token".into());
    let (window, view) = make_app(cx, old_client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (old_gate, view_a, view_b) = (old_gate, view.clone(), view.clone());
    visual.update(|window, app| {
        view_a.update(app, |v, cx| {
            v.channel_settings
                .as_mut()
                .unwrap()
                .reopen(1, 9, "text", "A", true, window, cx);
            v.channel_settings.as_mut().unwrap().dialog.activate(1);
            v.active_settings = Some(SettingsKind::Channel);
            old_gate.arm();
            v.load_channel_tab(1, window, cx);
        });
    });
    visual.update(|window, app| {
        view_b.update(app, |v, cx| {
            v.client = Some(new_client.clone());
            v.close_settings(false, window, cx);
            v.channel_settings
                .as_mut()
                .unwrap()
                .reopen(1, 9, "text", "A", true, window, cx);
            v.channel_settings.as_mut().unwrap().dialog.activate(1);
            v.active_settings = Some(SettingsKind::Channel);
            v.load_channel_tab(1, window, cx);
            assert!(
                v.channel_settings.as_ref().unwrap().access.is_none(),
                "the save button stays unavailable until the new answer"
            );
        });
    });
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        old_gate.release();
    });
    visual.run_until_parked();
    release.join().unwrap();
    wait_for(&mut visual, &view, |v| {
        v.channel_settings
            .as_ref()
            .is_some_and(|panel| panel.access.is_some())
    });
    view.read_with(&*visual, |v, _| {
        assert!(
            !v.channel_settings
                .as_ref()
                .unwrap()
                .access
                .as_ref()
                .unwrap()
                .restricted,
            "the old dialog generation is rejected even for the same scope"
        );
    });
}

#[gpui::test]
async fn returning_to_the_profile_tab_starts_a_new_get_and_discards_the_old_one(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let old_gate = Gate::new();
    let old = serde_json::to_string(&profile("old answer")).unwrap();
    let new = serde_json::to_string(&profile("new answer")).unwrap();
    let old_server =
        TestServer::spawn_concurrent(vec![Reply::new(200, &old).with_gate(old_gate.clone())]);
    let new_server = TestServer::spawn(vec![Reply::new(200, &new)]);
    let mut old_client = voxhold_api::Client::new(&old_server.base).unwrap();
    old_client.set_token("token".into());
    let mut new_client = voxhold_api::Client::new(&new_server.base).unwrap();
    new_client.set_token("token".into());
    let (window, view) = make_app(cx, old_client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (old_gate, view_a, view_b) = (old_gate, view.clone(), view.clone());
    visual.update(|window, app| {
        view_a.update(app, |v, cx| {
            v.personal.as_mut().unwrap().reopen(v.theme_mode, false);
            v.active_settings = Some(SettingsKind::Personal);
            old_gate.arm();
            v.load_personal_profile(window, cx);
        });
    });
    visual.update(|window, app| {
        view_b.update(app, |v, cx| {
            v.client = Some(new_client.clone());
            v.activate_settings_tab(1, window, cx);
            assert!(!v.personal.as_ref().unwrap().profile_loading);
            assert_eq!(
                v.personal.as_ref().unwrap().dialog.loads[0],
                LoadState::Idle
            );
            let first = v.personal.as_mut().unwrap().dialog.activate(0);
            assert!(first, "returning must start the request again");
            v.load_personal_profile(window, cx);
        });
    });
    // The new GET answers first; the delayed old one must be ignored.
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        old_gate.release();
    });
    visual.run_until_parked();
    release.join().unwrap();
    wait_for(&mut visual, &view, |v| {
        v.personal
            .as_ref()
            .and_then(|panel| panel.profile.as_ref())
            .is_some()
    });
    view.read_with(&*visual, |v, cx| {
        let panel = v.personal.as_ref().unwrap();
        assert_eq!(
            panel.profile.as_ref().unwrap().about,
            "new answer",
            "the late old GET is discarded"
        );
        assert_eq!(panel.about.read(cx).value().to_string(), "new answer");
    });
}

#[gpui::test]
async fn a_second_profile_save_is_blocked_until_the_first_patch_answers(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let reply = serde_json::to_string(&profile("server answer")).unwrap();
    let server = TestServer::spawn(vec![Reply::new(200, &reply).with_gate(gate.clone())]);
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
                .update(cx, |input, cx| input.set_value("old", window, cx));
            gate.arm();
            v.save_profile(window, cx);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.personal.as_mut().unwrap().about.update(cx, |input, cx| {
                input.set_value("typed during request", window, cx)
            });
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
        "the second handler call must not send a PATCH"
    );
    wait_for(&mut visual, &view, |v| {
        v.active_profile_save.is_none()
            && v.personal
                .as_ref()
                .is_some_and(|panel| panel.dialog.pending == 0)
    });
    view.read_with(&*visual, |v, cx| {
        let panel = v.personal.as_ref().unwrap();
        assert_eq!(
            panel.about.read(cx).value().to_string(),
            "typed during request"
        );
        assert_eq!(panel.profile.as_ref().unwrap().about, "server answer");
    });
}

#[gpui::test]
async fn a_realtime_notification_setting_keeps_the_local_choice_and_beats_an_old_get(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![
        Reply::new(200, r#"{"mode":"all"}"#).with_gate(gate.clone())
    ]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            let panel = v.channel_notifications.as_mut().unwrap();
            panel.reopen(1, 9, "qa");
            panel.mode = "none".into();
            panel.saved = "all".into();
            panel.loaded = true;
            v.active_settings = Some(SettingsKind::ChannelNotifications);
            gate.arm();
            v.load_channel_notifications(cx);
        });
    });
    // The load captured its epoch synchronously; the event is newer even
    // before the GET reaches the test server.
    visual.update(|_window, app| {
        view.update(app, |v, _| {
            v.apply_realtime_event(
                "notification.settings_updated",
                &serde_json::json!({"server_id":1,"channel_id":9,"mode":"all"}),
            );
        });
    });
    view.read_with(&*visual, |v, _| {
        let panel = v.channel_notifications.as_ref().unwrap();
        assert_eq!(panel.mode, "none", "an unsaved local choice survives");
        assert_eq!(panel.saved, "all");
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the GET reached the server");
    wait_for(&mut visual, &view, |v| {
        v.channel_notifications
            .as_ref()
            .is_some_and(|panel| panel.dialog.pending == 0)
    });
    view.read_with(&*visual, |v, _| {
        let panel = v.channel_notifications.as_ref().unwrap();
        assert_eq!(
            panel.mode, "none",
            "the GET started before the event is old"
        );
        assert_eq!(panel.saved, "all");
    });
}

#[gpui::test]
async fn a_notification_save_cannot_be_repeated_before_the_put_answers(cx: &mut TestAppContext) {
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
            v.save_channel_notifications(window, cx);
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the PUT reached the server");
    assert_eq!(
        server.requests().len(),
        1,
        "a repeated save is ignored while the PUT is in flight"
    );
    wait_for(&mut visual, &view, |v| v.active_settings.is_none());
    view.read_with(&*visual, |v, _| {
        let panel = v.channel_notifications.as_ref().unwrap();
        assert_eq!(panel.saved, "all");
        assert!(!v.notification_saves.contains_key(&(1, 9)));
    });
}

#[gpui::test]
async fn a_regular_member_can_read_and_save_channel_notifications(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let server = TestServer::spawn(vec![
        Reply::new(200, r#"{"mode":"mentions"}"#),
        Reply::new(200, "{}"),
    ]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.servers[0].role = "member".into();
            v.channels = vec![voxhold_api::Channel {
                id: 9,
                name: "qa".into(),
                kind: "text".into(),
                permissions: 3,
                ..Default::default()
            }];
            v.channel = 9;
            let focus = v.notifications_entry_focus.clone();
            v.open_settings(SettingsKind::ChannelNotifications, focus, window, cx);
        });
    });
    wait_for(&mut visual, &view, |v| {
        v.channel_notifications
            .as_ref()
            .is_some_and(|panel| panel.loaded)
    });
    view.read_with(&*visual, |v, _| {
        let panel = v.channel_notifications.as_ref().unwrap();
        assert_eq!(panel.mode, "mentions");
        assert_eq!(panel.saved, "mentions");
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.settings_action(
                SettingsAct::Notifications(channel_settings::NotifAction::Choose(0)),
                window,
                cx,
            );
            v.settings_action(
                SettingsAct::Notifications(channel_settings::NotifAction::Save),
                window,
                cx,
            );
        });
    });
    wait_for(&mut visual, &view, |v| v.active_settings.is_none());
    assert_eq!(
        server.requests().len(),
        2,
        "the member reads and writes the per-account preference"
    );

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.channel_notifications.as_mut().unwrap().reopen(1, 9, "qa");
            v.active_settings = Some(SettingsKind::ChannelNotifications);
            v.channels[0].permissions = 0;
            v.close_stale_settings(1, 9, window, cx);
        });
    });
    view.read_with(&*visual, |v, _| assert!(v.active_settings.is_none()));
}

#[gpui::test]
async fn deleting_an_account_logs_out_even_after_the_form_was_closed(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![Reply::new(200, "{}").with_gate(gate.clone())]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.servers[0].role = "member".into();
            v.active_settings = Some(SettingsKind::Personal);
            let panel = v.personal.as_mut().unwrap();
            panel.profile = Some(profile("old"));
            panel
                .delete_name
                .update(cx, |input, cx| input.set_value("owner", window, cx));
            panel
                .delete_password
                .update(cx, |input, cx| input.set_value("secret", window, cx));
            gate.arm();
            v.settings_action(
                SettingsAct::Personal(personal_settings::Action::ConfirmDelete),
                window,
                cx,
            );
            v.close_settings(false, window, cx);
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the DELETE reached the server");
    wait_for(&mut visual, &view, |v| v.client.is_none());
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.auth.screen, auth::AuthScreen::Login);
        assert!(v.active_settings.is_none());
    });
}

#[gpui::test]
async fn a_late_delete_answer_from_an_old_session_does_not_log_out_the_new_one(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| gpui_component::init(cx));
    let gate = Gate::new();
    let server = TestServer::spawn(vec![Reply::new(200, "{}").with_gate(gate.clone())]);
    let mut client = voxhold_api::Client::new(&server.base).unwrap();
    client.set_token("token".into());
    let (window, view) = make_app(cx, client);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.servers[0].role = "member".into();
            let panel = v.personal.as_mut().unwrap();
            panel.profile = Some(profile("old"));
            panel
                .delete_name
                .update(cx, |input, cx| input.set_value("owner", window, cx));
            panel
                .delete_password
                .update(cx, |input, cx| input.set_value("secret", window, cx));
            gate.arm();
            v.settings_action(
                SettingsAct::Personal(personal_settings::Action::ConfirmDelete),
                window,
                cx,
            );
            v.session_generation = v.session_generation.wrapping_add(1);
        });
    });
    let waiter = release_on_arrival(gate.clone());
    visual.run_until_parked();
    assert!(waiter.join().unwrap(), "the DELETE reached the server");
    visual.run_until_parked();
    std::thread::sleep(Duration::from_millis(50));
    visual.run_until_parked();
    view.read_with(&*visual, |v, _| {
        assert!(v.client.is_some(), "the new session stays signed in");
        assert_ne!(v.auth.screen, auth::AuthScreen::Login);
    });
}

#[cfg(feature = "voxhold-media")]
#[gpui::test]
async fn push_to_talk_stays_closed_until_the_join_is_confirmed(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let client = voxhold_api::Client::new("http://127.0.0.1:1").unwrap();
    let (window, view) = make_app(cx, client.clone());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let voice = visual.update(|window, cx| {
        cx.new(|cx| voice_panel::VoicePanel::new(client, 1, 2, true, window, cx))
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.test_push_to_talk());
        assert!(panel.test_controls_muted(), "PTT starts with a closed mic");
        assert!(!panel.test_ptt_held());
    });
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.voice = Some(voice.clone());
            v.settings_action(
                SettingsAct::Personal(personal_settings::Action::SetInputMode(0)),
                _window,
                cx,
            );
        });
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_push_to_talk());
        // No confirmed join yet: the microphone stays closed even in the
        // voice-activity mode.
        assert!(panel.test_controls_muted());
    });
    visual.update(|_window, app| {
        view.update(app, |v, cx| {
            v.settings_action(
                SettingsAct::Personal(personal_settings::Action::SetInputMode(1)),
                _window,
                cx,
            );
        });
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.test_push_to_talk());
        assert!(panel.test_controls_muted());
    });
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.set_push_to_talk_held(true);
            cx.notify();
        });
    });
    // The hold is recorded, but an unconfirmed call still keeps the capture
    // closed: `microphone_open` requires a joined call.
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.test_ptt_held());
        assert!(panel.test_controls_muted());
    });
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.set_push_to_talk_held(false);
            panel.release_push_to_talk();
            cx.notify();
        });
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.test_ptt_held());
        assert!(panel.test_controls_muted());
    });
}

#[gpui::test]
async fn a_large_avatar_is_decoded_in_the_background_and_an_older_choice_loses(
    cx: &mut TestAppContext,
) {
    let directory = std::env::temp_dir().join(format!(
        "voxhold-avatar-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let first_path = directory.join("first.jpg");
    let second_path = directory.join("second.jpg");
    let first = image::RgbImage::from_fn(2_000, 2_000, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, 80])
    });
    let second = image::RgbImage::from_fn(900, 700, |x, y| {
        image::Rgb([40, (x % 233) as u8, (y % 227) as u8])
    });
    image::DynamicImage::ImageRgb8(first)
        .save_with_format(&first_path, image::ImageFormat::Jpeg)
        .unwrap();
    image::DynamicImage::ImageRgb8(second)
        .save_with_format(&second_path, image::ImageFormat::Jpeg)
        .unwrap();
    let first = super::avatar_ops::decode_avatar(&first_path).unwrap();
    let second = super::avatar_ops::decode_avatar(&second_path).unwrap();
    assert_eq!((first.2, first.3), (2_000, 2_000));
    assert!(first.1.width().max(first.1.height()) <= 640);

    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = make_app(cx, voxhold_api::Client::new("http://127.0.0.1:1").unwrap());
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (generation, session) = visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            let panel = v.personal.as_mut().unwrap();
            panel.reopen(v.theme_mode, false);
            panel.avatar_pick = 2;
            let _ = window;
            let _ = cx;
            (panel.dialog.generation, v.session_generation)
        })
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            // A decode of the first choice finishes after the second file was
            // picked: it must not replace the newer crop draft.
            v.apply_avatar_decoded(session, generation, 1, Ok(first), window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.personal.as_ref().unwrap().crop.is_none());
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.apply_avatar_decoded(session, generation, 2, Ok(second), window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        let crop = v.personal.as_ref().unwrap().crop.as_ref().unwrap();
        assert_eq!((crop.width, crop.height), (900, 700));
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.apply_avatar_decoded(
                session,
                generation,
                1,
                super::avatar_ops::decode_avatar(&first_path),
                window,
                cx,
            );
        });
    });
    view.read_with(&*visual, |v, _| {
        let crop = v.personal.as_ref().unwrap().crop.as_ref().unwrap();
        assert_eq!(
            (crop.width, crop.height),
            (900, 700),
            "the late first decode is discarded"
        );
    });
    let _ = std::fs::remove_dir_all(directory);
}
