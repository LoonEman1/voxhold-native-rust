//! Regression tests for settings races: late answers, closed tabs, local edits
//! during a request, 403 and deleted resources. HTTP runs against the real
//! `TestServer` with `Gate`, the same path as the application.

use super::*;
use crate::test_support::{Gate, Reply, TestServer};
use crate::voice_settings::Device;
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;
use std::time::Duration;

fn profile(about: &str) -> voxhold_api::Profile {
    voxhold_api::Profile {
        user_id: 1,
        username: "owner".into(),
        about: about.into(),
        country_code: Some("RU".into()),
        created_at: 1_700_000_000,
    }
}

fn make_app(
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
fn release_on_arrival(gate: std::sync::Arc<Gate>) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let arrived = gate.wait_arrived(Duration::from_secs(5));
        gate.release();
        arrived
    })
}

fn wait_for(
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
async fn closing_the_tab_discards_a_pending_profile_answer(cx: &mut TestAppContext) {
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
        assert_eq!(panel.about.read(cx).value().to_string(), "old");
        assert!(panel.notice.is_empty());
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
    assert_eq!(current, "Устройство недоступно");
}
