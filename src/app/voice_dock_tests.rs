//! Focus-ring regressions for the voice dock and the hidden room pane. They
//! share the window/fixture setup with `voice_tests`, but the painted-only
//! assertions are kept separate so the main voice suite stays under the file
//! size limit.

use super::voice_tests::{draw, fixture, fixture_with, participant};
use crate::voice_room::CallPhase;
use gpui::{TestAppContext, VisualTestContext};

#[gpui::test]
async fn the_dock_ring_follows_the_painted_sidebar(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture_with(cx, Some(390.0), true);
    // The window starts narrow: the channel list is a closed overlay, so the
    // dock was never mounted and owns no ring entry.
    draw(&mut visual);
    let dock_ids = |visual: &mut VisualTestContext| {
        voice.read_with(visual, |panel, _| {
            let mut ids: Vec<_> = panel
                .test_focus_ring_ids()
                .into_iter()
                .filter(|id| id.starts_with("voice-dock-action-"))
                .collect();
            ids.sort_unstable();
            ids
        })
    };
    assert!(
        visual.debug_bounds("voice-dock").is_none(),
        "the mobile dock only paints inside the opened channel overlay"
    );
    assert!(
        dock_ids(&mut visual).is_empty(),
        "a closed channel overlay leaves no dock entries"
    );

    // Opening the overlay paints the dock and restores exactly its ids.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.show_channels = true;
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(visual.debug_bounds("voice-dock").is_some());
    assert_eq!(
        dock_ids(&mut visual),
        [
            "voice-dock-action-voice-deaf",
            "voice-dock-action-voice-leave",
            "voice-dock-action-voice-mic"
        ],
        "the painted dock contributes microphone, sound and leave"
    );

    // Closing it again removes the entries even though the call is still on.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.show_channels = false;
            cx.notify();
        });
    });
    draw(&mut visual);
    assert!(
        dock_ids(&mut visual).is_empty(),
        "a closed channel overlay leaves no stale dock entries"
    );

    // The flag itself is authoritative too: false removes every dock entry,
    // true adds exactly the three painted actions back.
    visual.update(|_, app| {
        voice.update(app, |panel, _| panel.set_dock_visible(false));
    });
    assert!(dock_ids(&mut visual).is_empty());
    visual.update(|_, app| {
        voice.update(app, |panel, _| panel.set_dock_visible(true));
    });
    assert_eq!(
        dock_ids(&mut visual),
        [
            "voice-dock-action-voice-deaf",
            "voice-dock-action-voice-leave",
            "voice-dock-action-voice-mic"
        ]
    );
}

#[gpui::test]
async fn tab_skips_room_handles_when_the_room_pane_is_hidden(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture(cx);
    draw(&mut visual);
    // A text channel keeps the call and the dock but paints no room pane.
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            v.channel = 9;
            cx.notify();
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(!v.voice_room_visible(), "the text channel owns the pane")
    });
    assert!(visual.debug_bounds("voice-dock").is_some());
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_focus_ring_ids()),
        [
            "voice-dock-action-voice-mic",
            "voice-dock-action-voice-deaf",
            "voice-dock-action-voice-leave"
        ],
        "only the painted dock is in the ring"
    );
    for id in voice.read_with(&*visual, |panel, _| panel.test_focus_ring_ids()) {
        assert!(
            id.starts_with("voice-dock-action-") || id.starts_with("stream-"),
            "the hidden room contributed {id}"
        );
        assert!(
            visual.debug_bounds(id).is_some(),
            "{id} is in the ring but was not painted"
        );
    }

    // The dock is mounted but the panel root is not, so focus a dock handle
    // directly and walk the ring with the real Tab action.
    for _ in 0..2 {
        visual.update(|window, app| {
            let focus = voice.read(app).dock_focus(0);
            window.focus(&focus);
            window.draw(app).clear();
        });
        visual.run_until_parked();
    }
    let focused = |visual: &mut VisualTestContext| {
        visual.update(|window, app| voice.read(app).focused_action(window))
    };
    assert_eq!(focused(&mut visual), Some("voice-dock-action-voice-mic"));
    for expected in [
        "voice-dock-action-voice-deaf",
        "voice-dock-action-voice-leave",
        "voice-dock-action-voice-mic",
    ] {
        visual.simulate_keystrokes("tab");
        assert_eq!(
            focused(&mut visual),
            Some(expected),
            "Tab stays inside the painted dock"
        );
    }
    visual.simulate_keystrokes("shift-tab");
    assert_eq!(
        focused(&mut visual),
        Some("voice-dock-action-voice-leave"),
        "backward Tab wraps inside the dock too"
    );
    for _ in 0..3 {
        visual.simulate_keystrokes("shift-tab");
        let id = focused(&mut visual).expect("the dock keeps focus");
        assert!(
            id.starts_with("voice-dock-action-"),
            "the hidden room's {id} must not receive focus"
        );
        assert!(!matches!(id, "voice-local" | "voice-mic"));
    }

    // With no room, no dock and no player the ring is empty and cycling is
    // safe: it must not panic or steal focus.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.set_dock_visible(false);
            assert!(
                !panel.cycle_focus(false, window, cx),
                "an empty ring reports that it moved nowhere"
            );
            assert!(!panel.cycle_focus(true, window, cx));
        });
    });
}

#[gpui::test]
async fn the_dock_disappears_when_the_call_ends(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = fixture_with(cx, None, false);
    // The call was idle before the first frame, so a fresh paint builds no
    // dock at all.
    draw(&mut visual);
    view.read_with(&*visual, |v, app| {
        assert!(
            !v.voice_dock_visible(app),
            "an idle call has no dock to show"
        );
    });
    assert!(
        visual.debug_bounds("voice-dock").is_none(),
        "an idle call paints no dock"
    );
    assert!(
        voice
            .read_with(&*visual, |panel, _| panel.test_focus_ring_ids())
            .iter()
            .all(|id| !id.starts_with("voice-dock-action-")),
        "an idle call leaves no dock handles behind"
    );

    // Rejoining paints the dock; ending the call again drops its ring entries
    // and the visibility flag in the same frame that stops the dock branch.
    visual.update(|_, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_call(
                7,
                CallPhase::Connected,
                vec![participant("alice", 1)],
                vec!["alice".into()],
            );
            cx.notify();
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, app| assert!(v.voice_dock_visible(app)));
    assert!(visual.debug_bounds("voice-dock").is_some());
    visual.update(|_, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_call(0, CallPhase::Idle, Vec::new(), Vec::new());
            cx.notify();
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, app| {
        assert!(!v.voice_dock_visible(app), "the call is over");
    });
    assert!(
        voice
            .read_with(&*visual, |panel, _| panel.test_focus_ring_ids())
            .iter()
            .all(|id| !id.starts_with("voice-dock-action-")),
        "the ended call leaves no dock handles behind"
    );
}
