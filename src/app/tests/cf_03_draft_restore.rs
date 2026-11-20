//! W13 CF-03: composer drafts survive a forced session expiry and are restored
//! for the same `(origin, user_id)` in this process. An explicit logout, another
//! account and another origin still start with an empty draft set, and a late
//! file-dialog answer cannot append to the drafts kept for a later login.

use super::super::*;
use crate::auth_session::{now_unix, Session};
use crate::test_support::{Reply, TestServer};
use gpui::{TestAppContext, VisualTestContext};
use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};

fn make_app(
    cx: &mut TestAppContext,
    client: Client,
) -> (&'static mut VisualTestContext, Entity<Voxhold>) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = super::settings_tests::make_app(cx, client);
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    (visual, view)
}

fn client_for(base: &str, token: &str) -> Client {
    let mut client = Client::new(base).expect("origin");
    client.set_token(token.into());
    client
}

fn session_for(base: &str, user_id: i64, username: &str, token: &str) -> Session {
    Session::new(
        base.into(),
        user_id,
        username.into(),
        token.into(),
        now_unix() + 3600,
    )
    .expect("session")
}

fn install_session(v: &mut Voxhold, base: &str, user_id: i64, username: &str) {
    // Already past its expiry: one timer tick is the forced loss of the session,
    // the same entry point the running clock uses.
    v.session = Some(
        Session::new(
            base.into(),
            user_id,
            username.into(),
            "token-1".into(),
            now_unix() - 1,
        )
        .expect("session"),
    );
    v.session_epoch = v.session_epoch.wrapping_add(1);
}

fn expire(v: &mut Voxhold, window: &mut Window, cx: &mut Context<Voxhold>) {
    assert!(
        !v.session_tick(window, cx),
        "an expired session stops the clock"
    );
}

fn channel_row(id: i64) -> Channel {
    Channel {
        id,
        name: format!("channel-{id}"),
        kind: "text".into(),
        permissions: 3,
        ..Default::default()
    }
}

/// A writable channel workspace without background requests of its own.
fn workspace(v: &mut Voxhold) {
    v.server = 1;
    v.channel = 7;
    v.channels = vec![channel_row(7), channel_row(8)];
    v.shell_initialized = true;
    v.show_members = false;
    v.search = None;
}

fn root_message(id: i64) -> Message {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "channel_id": 7,
        "conversation_id": 0,
        "content": "root",
        "author": {"user_id": 1, "username": "alice"},
        "created_at": 1,
        "revision": 1,
        "reactions": [],
        "attachments": []
    }))
    .expect("root message")
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "voxhold-cf03-{}-{}-{name}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

fn write_temp(name: &str, bytes: usize) -> PathBuf {
    let path = temp_path(name);
    std::fs::write(&path, vec![7u8; bytes]).expect("temporary file");
    path
}

fn pump(visual: &mut VisualTestContext) {
    for _ in 0..40 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The open channel, another channel and an open thread keep their text and
/// attachments through expiry and come back when the same account signs in.
#[gpui::test]
async fn same_owner_restores_channel_other_and_thread_drafts(cx: &mut TestAppContext) {
    let server = TestServer::spawn(Vec::new());
    let base = server.base.clone();
    let (visual, view) = make_app(cx, client_for(&base, "token-1"));
    let key_open = draft::ChatKey::channel(1, 7);
    let key_other = draft::ChatKey::channel(1, 8);
    let key_thread = draft::ChatKey::thread(1, 7, 5);
    let regular = write_temp("regular.bin", 64);
    let clipboard = write_temp("clipboard.png", 16);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            workspace(v);
            v.composer
                .update(cx, |input, cx| input.set_value("черновик 7", window, cx));
            v.drafts.borrow_mut().add_files(
                key_open,
                vec![draft::DraftFile::from_path(regular.clone()).expect("regular attachment")],
            );
            v.drafts.borrow_mut().set_text(key_other, "черновик 8");
            v.drafts.borrow_mut().add_files(
                key_other,
                vec![
                    draft::DraftFile::from_temporary(clipboard.clone(), "clipboard.png")
                        .expect("clipboard attachment"),
                ],
            );
            install_session(v, &base, 1, "alice");
            // A real thread panel: expiry must close it through `close_thread`,
            // which saves the panel composer into its `ChatKey::thread` draft.
            v.open_thread(root_message(5), 0, window, cx);
        });
    });
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let panel = v.thread.clone().expect("thread panel");
            panel.update(cx, |panel, cx| {
                panel.set_composer_for_test("черновик ветки", window, cx)
            });
        });
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| expire(v, window, cx));
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.session.is_none(), "the session is gone");
        assert_eq!(v.auth.screen, crate::auth::AuthScreen::Login);
        assert!(v.thread.is_none(), "the open thread was closed");
        assert!(
            v.drafts.borrow().text(key_open).is_empty(),
            "the live set is emptied by the normal end-of-session path"
        );
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let client = client_for(&base, "token-2");
            let session = session_for(&base, 1, "alice", "token-2");
            v.enter_workspace(client, Vec::new(), session, window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.drafts.borrow().text(key_open),
            "черновик 7",
            "the open channel draft is restored"
        );
        assert_eq!(
            v.drafts.borrow().text(key_other),
            "черновик 8",
            "another channel draft is restored"
        );
        assert_eq!(
            v.drafts.borrow().text(key_thread),
            "черновик ветки",
            "the thread draft is restored"
        );
        assert_eq!(v.drafts.borrow().files(key_open).len(), 1);
        assert_eq!(v.drafts.borrow().files(key_other).len(), 1);
        assert!(v.attachments.is_some(), "the panel is rebuilt after login");
        assert!(regular.exists());
        assert!(
            clipboard.exists(),
            "the clipboard picture is still available"
        );
    });

    // The restored text reaches the composer of its own chat.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.server = 1;
            v.channels = vec![channel_row(7), channel_row(8)];
            v.select_channel(7, window, cx);
        });
    });
    view.read_with(&*visual, |v, cx| {
        assert_eq!(v.chat_key(), key_open);
        assert_eq!(v.composer.read(cx).value(), "черновик 7");
    });
    // The reopened thread reads its own restored draft as well.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.open_thread(root_message(5), 0, window, cx));
    });
    visual.update(|_, app| {
        let panel = view
            .read_with(app, |v, _| v.thread.clone())
            .expect("thread");
        panel.read_with(app, |panel, app| {
            assert_eq!(panel.composer_text(app), "черновик ветки");
        });
    });

    // A normal logout after the restore is still a final clear.
    visual.update(|window, app| {
        view.update(app, |v, cx| v.logout(window, cx));
    });
    pump(visual);
    view.read_with(&*visual, |v, _| {
        assert!(v.drafts.borrow().text(key_open).is_empty());
        assert!(v.drafts.borrow().text(key_other).is_empty());
        assert!(
            !clipboard.exists(),
            "the temporary clipboard file is removed"
        );
        assert!(regular.exists(), "the user's own file is never deleted");
    });
    std::fs::remove_file(regular).ok();
}

/// A different `user_id` on the same origin receives nothing, and the kept set
/// releases its temporary clipboard file.
#[gpui::test]
async fn another_user_does_not_receive_the_kept_drafts(cx: &mut TestAppContext) {
    let server = TestServer::spawn(Vec::new());
    let base = server.base.clone();
    let (visual, view) = make_app(cx, client_for(&base, "token-1"));
    let key = draft::ChatKey::channel(1, 7);
    let clipboard = write_temp("other-user.png", 16);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            workspace(v);
            v.drafts.borrow_mut().set_text(key, "черновик 7");
            v.drafts.borrow_mut().add_files(
                key,
                vec![
                    draft::DraftFile::from_temporary(clipboard.clone(), "clipboard.png")
                        .expect("clipboard attachment"),
                ],
            );
            install_session(v, &base, 1, "alice");
            expire(v, window, cx);
        });
    });

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let client = client_for(&base, "token-2");
            let session = session_for(&base, 2, "bob", "token-2");
            v.enter_workspace(client, Vec::new(), session, window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert!(
            v.drafts.borrow().text(key).is_empty(),
            "another account starts without the previous drafts"
        );
        assert!(v.drafts.borrow().files(key).is_empty());
        assert!(
            !clipboard.exists(),
            "the kept set was cleared with its files"
        );
    });
}

/// The same user id on another origin is a different owner as well.
#[gpui::test]
async fn another_origin_does_not_receive_the_kept_drafts(cx: &mut TestAppContext) {
    let server = TestServer::spawn(Vec::new());
    let base = server.base.clone();
    let (visual, view) = make_app(cx, client_for(&base, "token-1"));
    let key = draft::ChatKey::channel(1, 7);
    let clipboard = write_temp("other-origin.png", 16);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            workspace(v);
            v.drafts.borrow_mut().set_text(key, "черновик 7");
            v.drafts.borrow_mut().add_files(
                key,
                vec![
                    draft::DraftFile::from_temporary(clipboard.clone(), "clipboard.png")
                        .expect("clipboard attachment"),
                ],
            );
            install_session(v, &base, 1, "alice");
            expire(v, window, cx);
        });
    });

    let other = "http://127.0.0.1:1";
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let client = client_for(other, "token-2");
            let session = session_for(other, 1, "alice", "token-2");
            v.enter_workspace(client, Vec::new(), session, window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert!(v.drafts.borrow().text(key).is_empty());
        assert!(v.drafts.borrow().files(key).is_empty());
        assert!(!clipboard.exists());
    });
}

/// An explicit logout clears the drafts of the signed-in account; it must not
/// turn into an expiry restore.
#[gpui::test]
async fn explicit_logout_still_clears_the_drafts(cx: &mut TestAppContext) {
    let server = TestServer::spawn(vec![
        Reply::new(204, "").with_path("/api/v1/auth/logout"),
        Reply::new(500, r#"{"error":"unexpected"}"#),
    ]);
    let base = server.base.clone();
    let (visual, view) = make_app(cx, client_for(&base, "token-1"));
    let key = draft::ChatKey::channel(1, 7);
    let clipboard = write_temp("logout.png", 16);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            workspace(v);
            v.composer
                .update(cx, |input, cx| input.set_value("черновик 7", window, cx));
            v.drafts.borrow_mut().add_files(
                key,
                vec![
                    draft::DraftFile::from_temporary(clipboard.clone(), "clipboard.png")
                        .expect("clipboard attachment"),
                ],
            );
            install_session(v, &base, 1, "alice");
            v.logout(window, cx);
        });
    });
    pump(visual);
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.auth.screen, crate::auth::AuthScreen::Login);
        assert!(v.session.is_none());
        assert!(v.drafts.borrow().text(key).is_empty());
        assert!(v.drafts.borrow().files(key).is_empty());
        assert!(!clipboard.exists());
    });
}

/// A file-dialog answer that arrives after expiry belongs to the dead scope and
/// cannot be appended to the drafts kept for the next login.
#[gpui::test]
async fn a_late_file_pick_cannot_append_to_the_kept_drafts(cx: &mut TestAppContext) {
    let server = TestServer::spawn(Vec::new());
    let base = server.base.clone();
    let (visual, view) = make_app(cx, client_for(&base, "token-1"));
    let key = draft::ChatKey::channel(1, 7);
    let late = write_temp("late.bin", 32);
    let slot: Rc<RefCell<Option<Entity<attachments::Attachments>>>> = Default::default();

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            workspace(v);
            v.composer
                .update(cx, |input, cx| input.set_value("черновик 7", window, cx));
            install_session(v, &base, 1, "alice");
        });
        let (client, runtime) = view.read_with(app, |v, _| {
            (v.client.clone().expect("client"), v.runtime.clone())
        });
        let drafts = view.read_with(app, |v, _| v.drafts.clone());
        let panel =
            app.new(|cx| attachments::Attachments::new(client, runtime, drafts, window, cx));
        panel.update(app, |panel, cx| panel.set_scope(Some(key), true, cx));
        view.update(app, |v, _| v.attachments = Some(panel.clone()));
        *slot.borrow_mut() = Some(panel);
    });
    let panel = slot.borrow().clone().expect("attachments panel");
    let stale_generation = panel.read_with(&*visual, |panel, _| panel.generation());

    visual.update(|window, app| {
        view.update(app, |v, cx| expire(v, window, cx));
    });
    // The dialog was already open when the session ended. Its answer arrives
    // now, with the scope generation that opened it.
    visual.update(|_, app| {
        panel.update(app, |panel, cx| {
            panel.finish_pick(Ok(vec![late.clone()]), Some(key), stale_generation, cx);
        });
    });
    pump(visual);

    visual.update(|window, app| {
        view.update(app, |v, cx| {
            let client = client_for(&base, "token-2");
            let session = session_for(&base, 1, "alice", "token-2");
            v.enter_workspace(client, Vec::new(), session, window, cx);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.drafts.borrow().text(key),
            "черновик 7",
            "the real draft is restored"
        );
        assert!(
            v.drafts.borrow().files(key).is_empty(),
            "the late file answer was discarded, not appended to the kept set"
        );
    });
    std::fs::remove_file(&late).ok();
}
