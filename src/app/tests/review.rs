//! Review regression tests for the composer dialog, send lifetime, link
//! cards and clipboard limits. Kept separate to respect the file size limit.

use super::*;
use super::{add_auth_window, focus_field, message_json, writable_workspace};

#[gpui::test]
async fn attachments_changed_marks_the_file_and_requests_a_refresh(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (_window, view) = add_auth_window(cx);
    cx.update(|app| {
        view.update(app, |v, _| {
            v.server = 1;
            v.channel = 7;
            let id = "a".repeat(32);
            let mut dto = message_json(1, 1, "файл");
            dto["attachments"] = serde_json::json!([{
                "id": id,
                "filename": "photo.png",
                "mime_type": "image/png",
                "size_bytes": 12,
                "width": 10,
                "height": 10,
                "deleted": false
            }]);
            v.merge_message(true, serde_json::from_value::<Message>(dto).unwrap());
            let effect = v.apply_realtime_event(
                "attachments.changed",
                &serde_json::json!({"channel_id": 7, "attachment_ids": [id]}),
            );
            assert_eq!(effect.attachments, vec![id.clone()]);
            assert!(effect.resync, "the row is rebuilt from a fresh page");
        });
    });
}

#[gpui::test]
async fn a_rejected_clipboard_picture_leaves_no_temporary_file(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let drafts = draft::Drafts::shared();
    let key = draft::ChatKey::channel(1, 7);
    // The draft is full: ten files already occupy the message.
    {
        let mut store = drafts.borrow_mut();
        let files = (0..10)
            .map(|index| draft::DraftFile {
                id: format!("{index:032x}"),
                path: std::path::PathBuf::from("missing"),
                name: format!("file{index}.bin"),
                size: 1,
                modified: None,
                temporary: false,
            })
            .collect();
        store.add_files(key, files);
    }
    let clipboard_files = || {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("voxhold-clipboard-")
            })
            .count()
    };
    let before = clipboard_files();
    let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = slot.clone();
    visual.update(|window, app| {
        view.update(app, |v, _| writable_workspace(v));
        let (client, runtime) = view.read_with(app, |v, _| {
            (v.client.clone().expect("client"), v.runtime.clone())
        });
        let panel = app
            .new(|cx| attachments::Attachments::new(client, runtime, drafts.clone(), window, cx));
        panel.update(app, |panel, cx| {
            panel.set_scope(Some(key), true, cx);
            assert!(panel.paste(
                crate::clipboard::Paste {
                    paths: Vec::new(),
                    image: Some(vec![1u8; 4096]),
                },
                cx
            ));
        });
        *captured.borrow_mut() = Some(panel);
    });
    visual.run_until_parked();
    assert_eq!(
        clipboard_files(),
        before,
        "a picture rejected by the draft limits is not written to %TEMP%"
    );
    let panel = slot.borrow().clone().expect("attachments");
    assert!(!panel.read_with(&*visual, |panel, _| panel.is_reading()));
    assert!(!panel.read_with(&*visual, |panel, _| panel.error().is_empty()));
}

#[gpui::test]
async fn a_link_card_reserves_its_space_before_the_preview_arrives(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_workspace(v);
            v.client = Some(Client::new("http://127.0.0.1:1").unwrap());
            v.select_channel(7, window, cx);
            for id in 1..=30 {
                v.apply_realtime_event(
                    "message.created",
                    &message_json(id, 1, "смотри https://example.com/article"),
                );
            }
            cx.notify();
        });
        window.draw(app).clear();
    });
    visual.run_until_parked();
    visual.update(|window, app| {
        window.draw(app).clear();
    });
    let before = view.read_with(&*visual, |v, _| {
        v.history_list.max_offset_for_scrollbar().height
    });
    assert!(before > px(0.), "the list is scrollable");
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.media.put_preview(
                "https://example.com/article",
                crate::media::PreviewEntry {
                    meta: voxhold_api::LinkPreview {
                        url: "https://example.com/article".into(),
                        title: "Статья".into(),
                        description: "Описание".into(),
                        site_name: "Example".into(),
                        ..Default::default()
                    },
                    retry_at: None,
                    failed: false,
                },
            );
        });
        window.draw(app).clear();
    });
    let after = view.read_with(&*visual, |v, _| {
        v.history_list.max_offset_for_scrollbar().height
    });
    assert_eq!(before, after, "the card slot exists before the metadata");
}

#[gpui::test]
async fn a_picked_file_is_added_and_the_dialog_state_does_not_block_it(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let path = std::env::temp_dir().join(format!(
        "voxhold-picked-{}-{}.bin",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, vec![3u8; 2048]).unwrap();
    let drafts = draft::Drafts::shared();
    let key = draft::ChatKey::channel(1, 7);
    let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = slot.clone();
    visual.update(|window, app| {
        view.update(app, |v, _| writable_workspace(v));
        let (client, runtime) = view.read_with(app, |v, _| {
            (v.client.clone().expect("client"), v.runtime.clone())
        });
        let panel = app
            .new(|cx| attachments::Attachments::new(client, runtime, drafts.clone(), window, cx));
        panel.update(app, |panel, cx| {
            panel.set_scope(Some(key), true, cx);
            // The dialog closed successfully: the result must clear the
            // "preparing files" state before metadata intake starts.
            let generation = panel.generation();
            panel.finish_pick(Ok(vec![path.clone()]), Some(key), generation, cx);
        });
        *captured.borrow_mut() = Some(panel);
    });
    let panel = slot.borrow().clone().expect("attachments");
    wait_until(&mut visual, || !drafts.borrow().files(key).is_empty());
    assert_eq!(drafts.borrow().files(key)[0].size, 2048);
    assert!(!panel.read_with(&*visual, |panel, _| panel.is_reading()));

    // A result from another chat cannot reach this draft.
    let stale = std::env::temp_dir().join(format!(
        "voxhold-stale-{}-{}.bin",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&stale, vec![4u8; 10]).unwrap();
    panel.update(&mut *visual, |panel, cx| {
        let generation = panel.generation();
        panel.finish_pick(
            Ok(vec![stale.clone()]),
            Some(draft::ChatKey::channel(1, 99)),
            generation,
            cx,
        );
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    visual.run_until_parked();
    assert_eq!(
        drafts.borrow().files(key).len(),
        1,
        "the stale pick is dropped"
    );
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(stale).unwrap();
}

fn send_test_server(message: serde_json::Value) -> (crate::test_support::TestServer, String) {
    let body = message.to_string();
    // The channel switch also issues history/member requests; every request
    // gets the message so the POST cannot accidentally read a 500.
    let replies = (0..8)
        .map(|_| crate::test_support::Reply::new(201, &body))
        .collect();
    let server = crate::test_support::TestServer::spawn(replies);
    let base = server.base.clone();
    (server, base)
}

#[gpui::test]
async fn logout_cancels_a_pending_send(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (_server, base) = send_test_server(message_json(9, 1, "текст"));
    let key = draft::ChatKey::channel(1, 7);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_workspace(v);
            v.client = Some(Client::new(&base).unwrap());
            v.composer
                .update(cx, |input, cx| input.set_value("текст", window, cx));
            v.send(window, cx);
            // Log out while the request is in flight.
            v.logout(window, cx);
        });
    });
    for _ in 0..60 {
        visual.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    view.read_with(&*visual, |v, _| {
        assert!(v.messages.is_empty(), "the confirmed row is discarded");
        assert_eq!(v.drafts.borrow().text(key), "");
        assert!(v.session_generation > 0);
    });
}

#[gpui::test]
async fn channel_switch_applies_the_confirmation_to_the_original_draft(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let (_server, base) = send_test_server(message_json(9, 1, "текст"));
    let key = draft::ChatKey::channel(1, 7);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_workspace(v);
            v.client = Some(Client::new(&base).unwrap());
            v.channels.push(Channel {
                id: 8,
                kind: "text".into(),
                permissions: 3,
                ..Default::default()
            });
            v.composer
                .update(cx, |input, cx| input.set_value("текст", window, cx));
            v.send(window, cx);
            // Leave the chat before the HTTP result arrives.
            v.select_channel(8, window, cx);
        });
    });
    for _ in 0..200 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| v.drafts.borrow().text(key).is_empty()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    view.read_with(&*visual, |v, cx| {
        assert_eq!(
            v.drafts.borrow().text(key),
            "",
            "the delivered message clears the draft it came from"
        );
        assert_eq!(v.channel, 8);
        assert!(v.messages.is_empty(), "another chat keeps its own history");
        assert_eq!(v.composer.read(cx).value().as_ref(), "");
    });
}
