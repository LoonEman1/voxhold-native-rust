use super::{add_auth_window, message_json, *};
use crate::test_support::{Reply, TestServer};
use gpui::{px, size, Modifiers, Pixels, Point};
use serde_json::json;

fn show_workspace(
    cx: &mut TestAppContext,
    base: &str,
) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    window
        .update(cx, |_, window, app| {
            view.update(app, |v, cx| {
                v.client = Some(Client::new(base).unwrap());
                v.self_user_id = 1;
                v.server = 1;
                v.channel = 7;
                v.shell_initialized = true;
                v.show_members = true;
                v.roster.channel_tab = false;
                v.servers = vec![Server {
                    id: 1,
                    name: "Пространство".into(),
                    role: "owner".into(),
                    ..Default::default()
                }];
                v.channels = vec![
                    Channel {
                        id: 7,
                        name: "Обсуждение".into(),
                        kind: "text".into(),
                        permissions: 3,
                        ..Default::default()
                    },
                    Channel {
                        id: 8,
                        name: "Новости".into(),
                        kind: "news".into(),
                        permissions: 3,
                        can_publish: true,
                        ..Default::default()
                    },
                ];
                v.members.push(Member {
                    user_id: 1,
                    username: "neva".into(),
                    role: "owner".into(),
                    ..Default::default()
                });
                v.channel_settings = Some(channel_settings::ChannelSettings::new(window, cx));
                v.channel_notifications = Some(channel_settings::ChannelNotifications::new());
                v.read_sent = 100;
                cx.notify();
            })
        })
        .unwrap();
    (window, view)
}

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

fn center(visual: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("Missing {selector}"))
        .center()
}

fn right_click(visual: &mut VisualTestContext, position: Point<Pixels>) {
    visual.simulate_mouse_move(position, None, Modifiers::default());
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    draw(visual);
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let position = center(visual, selector);
    visual.simulate_mouse_move(position, None, Modifiers::default());
    draw(visual);
    visual.simulate_click(position, Modifiers::default());
    draw(visual);
}

#[gpui::test]
async fn hover_buttons_work_through_mouse_input(cx: &mut TestAppContext) {
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &json!({
            "channel_id": 7, "message_id": 5, "revision": 2,
            "reactions": [{"emoji": "🤣", "count": 1, "me": true}]
        })
        .to_string(),
    )
    .with_path("/api/v1/servers/1/channels/7/messages/5/reactions/")]);
    let (window, view) = show_workspace(cx, &server.base);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.simulate_resize(size(px(1280.), px(900.)));
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.messages.push(
                serde_json::from_value(message_json(5, 1, "Сообщение для действий")).unwrap(),
            );
            cx.notify();
        })
    });
    draw(&mut visual);

    let row = center(&mut visual, "message-row-5");
    visual.simulate_mouse_move(row, None, Modifiers::default());
    draw(&mut visual);
    click(&mut visual, "message-action-5-🤣");
    for _ in 0..200 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| {
            v.messages[0].reactions.iter().any(|r| r.me)
        }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(view.read_with(&*visual, |v, _| v.messages[0]
        .reactions
        .iter()
        .any(|r| r.me)));

    click(&mut visual, "message-action-5-reply");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.drafts.borrow().reply(v.chat_key()), 5);
        assert!(v.selected_messages.is_empty());
    });
    click(&mut visual, "message-action-5-forward");
    assert!(view.read_with(&*visual, |v, _| v
        .forward
        .as_ref()
        .is_some_and(|state| state.message_id == 5)));
}

#[gpui::test]
async fn right_click_can_configure_an_unselected_news_channel(cx: &mut TestAppContext) {
    let (window, view) = show_workspace(cx, "http://127.0.0.1:1");
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.simulate_resize(size(px(1280.), px(900.)));
    draw(&mut visual);
    let channel = center(&mut visual, "channel-8");
    right_click(&mut visual, channel);
    assert!(visual.debug_bounds("channel-menu-notifications").is_some());
    click(&mut visual, "channel-menu-edit");
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.channel, 7);
        assert_eq!(v.active_settings, Some(SettingsKind::Channel));
        assert_eq!(v.channel_settings.as_ref().unwrap().channel, 8);
    });
}

#[gpui::test]
async fn a_visible_steam_card_loads_before_the_bottom_history_is_scrolled(cx: &mut TestAppContext) {
    let href = "https://store.steampowered.com/app/4096830/SONAR_%D0%9F/";
    let image_path = format!("/api/v1/link-preview/images/{}", "a".repeat(64));
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(320, 180))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let server = TestServer::spawn(vec![
        Reply::new(200, &json!({"url": href, "title": "S.O.N.A.R. on Steam", "site_name": "Steam", "image_path": image_path}).to_string())
            .with_path("/api/v1/link-preview?"),
        Reply::image(200, png.into_inner()).with_path(&image_path),
    ]);
    let (window, view) = show_workspace(cx, &server.base);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.simulate_resize(size(px(1280.), px(900.)));
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.messages = (1..=10)
                .map(|id| {
                    serde_json::from_value(message_json(
                        id,
                        1,
                        if id == 5 { href } else { "строка" },
                    ))
                    .unwrap()
                })
                .collect();
            cx.notify();
        })
    });
    draw(&mut visual);
    for _ in 0..200 {
        draw(&mut visual);
        if visual.debug_bounds("link-preview-image-5").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let card = visual.debug_bounds("link-card-5").expect("card is visible");
    let picture = visual
        .debug_bounds("link-preview-image-5")
        .expect("image was loaded without scrolling");
    assert!(picture.size.width >= px(150.));
    assert!(picture.size.height > px(76.));
    assert!(card.contains(&picture.center()));
}

#[gpui::test]
async fn right_click_on_a_member_opens_the_profile_menu(cx: &mut TestAppContext) {
    let server = TestServer::spawn(vec![Reply::new(
        200,
        &json!({"user_id": 1, "username": "neva", "about": "Тестовый профиль"}).to_string(),
    )
    .with_path("/api/v1/users/1/profile")]);
    let (window, view) = show_workspace(cx, &server.base);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.simulate_resize(size(px(1440.), px(900.)));
    draw(&mut visual);
    let member = center(&mut visual, "member-row-1");
    right_click(&mut visual, member);
    click(&mut visual, "member-profile");
    for _ in 0..200 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| {
            v.member_menu
                .as_ref()
                .is_some_and(|menu| menu.profile_result.is_some())
        }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    view.read_with(&*visual, |v, _| {
        let profile = v
            .member_menu
            .as_ref()
            .unwrap()
            .profile_result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        assert_eq!(profile.about, "Тестовый профиль");
    });
}

#[cfg(feature = "voxhold-media")]
#[gpui::test]
async fn a_voice_room_shows_live_status_and_participant_context(cx: &mut TestAppContext) {
    use voxhold_media::controller::Publication;
    let (visual, view, _) = crate::app::voice_tests::fixture(cx);
    visual.simulate_resize(size(px(1440.), px(900.)));
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.voice_rooms.insert(
                (1, 8),
                vec![crate::app::voice_tests::participant("carol", 3)],
            );
            v.voice_streams.insert(
                (1, 8),
                vec![Publication {
                    server_id: 1,
                    channel_id: 8,
                    publisher_user_id: 3,
                    publisher_connection_id: "carol".into(),
                    viewer_count: 12,
                    started_at_ms: 0,
                    codec: "vp8".into(),
                    mode: "server".into(),
                    has_audio: true,
                }],
            );
            v.channel = 8;
            cx.notify();
        })
    });
    visual.update(|window, app| view.update(app, |v, cx| v.open_voice_room(8, window, cx)));
    draw(visual);
    assert!(visual.debug_bounds("channel-live-3").is_some());
    assert!(visual.debug_bounds("stream-metadata:12 зрителей").is_some());
    let voice_row = center(visual, "voice-member-carol");
    right_click(visual, voice_row);
    assert!(visual.debug_bounds("member-volume-track").is_some());
    assert!(view.read_with(&*visual, |v, _| v
        .member_menu
        .as_ref()
        .is_some_and(|menu| menu.user_id == 3 && menu.voice)));
}
