//! O05: the history cache and the virtualized list keep their contract with
//! the view: rows are prepared once per input change, a repaint is cheap, and
//! prepending older pages keeps the anchor and the painted read mark.

use super::*;
use gpui::{px, ListOffset};

fn draw(visual: &mut VisualTestContext, view: &Entity<Voxhold>) {
    visual.update(|window, app| {
        view.update(app, |_, cx| cx.notify());
        window.draw(app).clear();
    });
}

/// Like [`draw`], but refreshes the window and settles the frame so the new
/// tree is painted before `debug_bounds` is read.
fn redraw(visual: &mut VisualTestContext, view: &Entity<Voxhold>) {
    visual.update(|window, app| {
        view.update(app, |_, cx| cx.notify());
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

#[gpui::test]
async fn history_rows_rebuild_only_when_their_inputs_change(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|_, app| {
        view.update(app, |v, _| {
            writable_workspace(v);
            for id in 1..=60 {
                v.apply_realtime_event("message.created", &message_json(id, id, "row"));
            }
        });
    });
    draw(&mut visual, &view);
    let built = view.read_with(&*visual, |v, _| v.history_rows.rebuild_count());
    assert!(built > 0);

    draw(&mut visual, &view);
    let repainted = view.read_with(&*visual, |v, _| v.history_rows.rebuild_count());
    assert_eq!(
        repainted, built,
        "a plain repaint must reuse the prepared rows"
    );

    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.apply_realtime_event("message.updated", &message_json(5, 6, "edited"));
        });
    });
    draw(&mut visual, &view);
    let edited = view.read_with(&*visual, |v, _| v.history_rows.rebuild_count());
    assert!(edited > repainted, "an edit must invalidate the rows");
    let content = view.read_with(&*visual, |v, _| {
        v.history_rows.row(4).map(|row| row.message.content.clone())
    });
    assert_eq!(content.as_deref(), Some("edited"));

    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.apply_realtime_event(
                "message.reactions_changed",
                &serde_json::json!({
                    "channel_id": 7,
                    "message_id": 6,
                    "revision": 7,
                    "reactions": [{"emoji": "🔥", "count": 1, "me": true}]
                }),
            );
        });
    });
    draw(&mut visual, &view);
    let reacted = view.read_with(&*visual, |v, _| v.history_rows.rebuild_count());
    assert!(reacted > edited, "a reaction snapshot must invalidate");

    visual.update(|_, app| {
        view.update(app, |v, _| v.unread_after = 10);
    });
    draw(&mut visual, &view);
    let unread = view.read_with(&*visual, |v, _| v.history_rows.rebuild_count());
    assert!(unread > reacted, "a new unread boundary must invalidate");

    visual.update(|_, app| {
        view.update(app, |v, _| v.selected_messages.push(3));
    });
    draw(&mut visual, &view);
    assert!(
        view.read_with(&*visual, |v, _| v.history_rows.rebuild_count()) > unread,
        "selecting a row must invalidate the prepared flags"
    );
}

#[gpui::test]
async fn prepending_older_rows_keeps_the_anchor_and_painted_read(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|_, app| {
        view.update(app, |v, _| {
            writable_workspace(v);
            for id in 31..=110 {
                v.apply_realtime_event("message.created", &message_json(id, id, "row"));
            }
        });
    });
    draw(&mut visual, &view);
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.history_list.scroll_to(ListOffset {
                item_ix: 20,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
    });
    draw(&mut visual, &view);
    let (top_before, painted_before, anchor_id) = view.read_with(&*visual, |v, _| {
        (
            v.history_list.logical_scroll_top(),
            v.painted_read.get(),
            v.messages[20].id,
        )
    });
    assert_eq!(top_before.item_ix, 20, "the test starts scrolled up");
    assert!(painted_before > 0);

    // The older page lands in front of the loaded window; `load_older` keeps
    // `(anchor id, offset)` for the next render.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            let older = (1..=30)
                .map(|id| serde_json::from_value::<Message>(message_json(id, id, "older")).unwrap())
                .collect::<Vec<_>>();
            v.messages.splice(0..0, older);
            v.pending_anchor = Some((anchor_id, px(7.)));
            cx.notify();
        });
    });
    draw(&mut visual, &view);
    let (anchor, top_after, painted_after, keys) = view.read_with(&*visual, |v, _| {
        (
            v.pending_anchor,
            v.history_list.logical_scroll_top(),
            v.painted_read.get(),
            v.history_keys.len(),
        )
    });
    assert!(anchor.is_none(), "the render consumes the pending anchor");
    assert_eq!(keys, 110, "the list count follows the prepended window");
    assert_eq!(
        top_after.item_ix, 50,
        "the anchored row stays under the pointer"
    );
    assert_eq!(top_after.offset_in_item, px(7.));
    assert_eq!(
        painted_after, painted_before,
        "the same visible rows are still painted after the prepend"
    );
}

/// O05 review: the prepared rows hold `Arc<Message>` bodies, so a logout must
/// drop them together with `messages`/`history_keys` instead of waiting for a
/// history render that never runs. A later render must rebuild from scratch.
#[gpui::test]
async fn logout_releases_the_prepared_rows_and_the_next_render_rebuilds(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|_, app| {
        view.update(app, |v, _| {
            writable_workspace(v);
            for id in 1..=20 {
                v.apply_realtime_event("message.created", &message_json(id, id, "private text"));
            }
        });
    });
    draw(&mut visual, &view);
    assert_eq!(view.read_with(&*visual, |v, _| v.history_rows.len()), 20);

    // One cached row keeps its body in an `Arc`. The view alone owns it, so a
    // test clone raises the count to two until the cache is dropped.
    let held = view.read_with(&*visual, |v, _| {
        v.history_rows
            .row(0)
            .map(|row| row.message.clone())
            .expect("the first row is cached")
    });
    assert_eq!(std::sync::Arc::strong_count(&held), 2);

    visual.update(|window, app| {
        view.update(app, |v, cx| v.logout(window, cx));
    });
    visual.run_until_parked();

    view.read_with(&*visual, |v, _| {
        assert!(v.messages.is_empty());
        assert!(v.history_keys.is_empty());
        assert_eq!(v.history_rows.len(), 0, "logout drops every prepared row");
    });
    assert_eq!(
        std::sync::Arc::strong_count(&held),
        1,
        "the cached message body is released by logout"
    );
    drop(held);

    // Re-entering a workspace must refill the cache, not leave it reset.
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            writable_workspace(v);
            v.apply_realtime_event("message.created", &message_json(1, 1, "after logout"));
            let chat = crate::layout::Rect::new(0., 0., 800., 600.);
            let _ = v.render_history(window, cx, chat, false, false);
        });
    });
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.history_rows.len(), 1);
        assert_eq!(
            v.history_rows
                .row(0)
                .map(|row| row.message.content.as_str()),
            Some("after logout")
        );
    });
}

/// O05 review: the attachment tombstone must reach the next drawn row, not
/// only the message vector. `attachment_epoch` forces the cache rebuild and
/// the deleted picture falls back to the "Вложение удалено" row.
#[gpui::test]
async fn an_attachment_tombstone_reaches_the_drawn_row(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let id = "b".repeat(32);
    visual.update(|_, app| {
        view.update(app, |v, _| {
            writable_workspace(v);
            let mut dto = message_json(1, 1, "файл");
            dto["attachments"] = serde_json::json!([{
                "id": id.clone(),
                "filename": "photo.png",
                "mime_type": "image/png",
                "size_bytes": 12,
                "width": 10,
                "height": 10,
                "deleted": false
            }]);
            v.apply_realtime_event("message.created", &dto);
        });
    });
    redraw(&mut visual, &view);
    assert!(
        visual.debug_bounds("attachment-1-0-live").is_some(),
        "the live picture is drawn before the tombstone"
    );
    let built = view.read_with(&*visual, |v, _| v.history_rows.rebuild_count());

    visual.update(|_, app| {
        view.update(app, |v, _| {
            let effect = v.apply_realtime_event(
                "attachments.changed",
                &serde_json::json!({"channel_id": 7, "attachment_ids": [id.clone()]}),
            );
            assert_eq!(effect.attachments, vec![id.clone()]);
        });
    });
    redraw(&mut visual, &view);

    let rebuilt = view.read_with(&*visual, |v, _| v.history_rows.rebuild_count());
    assert!(
        rebuilt > built,
        "the attachment epoch must rebuild the prepared row"
    );
    let (deleted, label) = view.read_with(&*visual, |v, _| {
        let row = v.history_rows.row(0).expect("the row is still cached");
        let file = &row.message.attachments[0];
        (file.deleted, crate::history::attachment_label(file))
    });
    assert!(deleted);
    assert_eq!(label, "Вложение удалено");
    assert!(
        visual.debug_bounds("attachment-1-0-deleted").is_some(),
        "the next frame draws the tombstoned file row instead of the picture"
    );
}
