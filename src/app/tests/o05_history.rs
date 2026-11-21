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
