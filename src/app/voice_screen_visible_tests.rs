//! Visible-area thumbnail regressions: only the painted rows plus their margin
//! are captured, a full cache evicts off-screen pictures instead of failing the
//! visible cards, and a tab switch or a scroll never captures twice.

use super::voice_screen_tests::{draw, monitor, open_picker_with, wait_for, window};
use gpui::TestAppContext;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use voxhold_media::screen_capture::{
    PreviewFrame, ScreenSource, SourceId, SourceKind, ThumbnailError,
};

/// Records every capture attempt and the peak number of captures inside the
/// provider at once. No Windows capturer is involved.
#[derive(Clone, Default)]
struct Recording {
    attempted: Arc<Mutex<Vec<String>>>,
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Recording {
    fn provider(
        &self,
    ) -> Arc<dyn Fn(&ScreenSource) -> Result<PreviewFrame, ThumbnailError> + Send + Sync> {
        self.provider_with(4, 4)
    }

    /// A provider whose frames are the size the media layer really returns, so
    /// the byte half of the cache bound is exercised.
    fn big_provider(
        &self,
    ) -> Arc<dyn Fn(&ScreenSource) -> Result<PreviewFrame, ThumbnailError> + Send + Sync> {
        self.provider_with(320, 180)
    }

    fn provider_with(
        &self,
        width: u32,
        height: u32,
    ) -> Arc<dyn Fn(&ScreenSource) -> Result<PreviewFrame, ThumbnailError> + Send + Sync> {
        let attempted = self.attempted.clone();
        let in_flight = self.in_flight.clone();
        let peak = self.peak.clone();
        Arc::new(move |source: &ScreenSource| {
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            if let Ok(mut list) = attempted.lock() {
                list.push(format!("{:?}", source.source_id()));
            }
            in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(PreviewFrame {
                width,
                height,
                bgra: vec![0x30; width as usize * height as usize * 4],
            })
        })
    }

    fn attempts(&self) -> Vec<String> {
        self.attempted.lock().expect("provider log").clone()
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

fn ids(sources: &[ScreenSource]) -> Vec<SourceId> {
    sources.iter().map(ScreenSource::source_id).collect()
}

fn labels(ids: &[SourceId]) -> Vec<String> {
    ids.iter().map(|id| format!("{id:?}")).collect()
}

/// Scroll the picker list by `delta_y` pixels through a real wheel event at
/// its centre, exactly like the user.
fn wheel(visual: &mut gpui::VisualTestContext, delta_y: f32) {
    let list = visual
        .debug_bounds("share-sources")
        .expect("the picker list is painted");
    let at = gpui::point(list.center().x, list.center().y);
    visual.simulate_mouse_move(at, None, Default::default());
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(delta_y))),
        ..Default::default()
    });
}

/// With 24 sources the tab takes many captures. After scrolling two rows down
/// the batch must start with the rows the grid paints (plus one margin row)
/// and stop there: the rest of the tab waits for a real scroll.
#[gpui::test]
async fn scrolled_rows_are_captured_instead_of_the_whole_tab(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..24)
        .map(|index| monitor(100 + index, &format!("Экран {index}"), 1920, 1080))
        .collect();
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources.clone());
    let seen = Recording::default();
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(seen.provider())
        });
    });

    // The wheel must drive the layout the dialog really painted. If the card
    // height or the row gap drifts, this fails instead of scrolling a stale
    // number of pixels.
    let first = visual
        .debug_bounds("share-source-card-monitor-100")
        .expect("the first card is painted");
    let second_row = visual
        .debug_bounds("share-source-card-monitor-102")
        .expect("the first card of the second row is painted");
    let card_height = f32::from(first.size.height);
    let step = f32::from(second_row.origin.y - first.origin.y);
    assert!(
        step > card_height + 1.,
        "the painted row step {step} is one card plus the row gap"
    );

    // Two rows down: 2 × the painted row step.
    wheel(&mut visual, -2.0 * step);
    draw(&mut visual);

    let visible = voice.read_with(&*visual, |panel, _| panel.test_visible_screen_previews());
    // Rows 1..5: one margin row above, rows 2 and 3 on screen and their
    // margin below; two cards per row at the normal dialog width.
    assert_eq!(
        visible,
        ids(&sources[2..10]),
        "the visible set is the painted rows plus one margin row"
    );
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == visible.len()
    });
    let attempts = seen.attempts();
    assert_eq!(
        attempts,
        labels(&visible),
        "only the visible rows and their margin were captured"
    );
    assert!(
        attempts.len() <= 12,
        "24 open sources must not become 24 captures: {}",
        attempts.len()
    );
    assert_eq!(seen.peak(), 1, "one capture at a time");

    // One scroll asks once; a repaint without a scroll asks nothing.
    let deferrals = voice.read_with(&*visual, |panel, _| panel.test_screen_preview_deferrals());
    draw(&mut visual);
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_preview_deferrals()),
        deferrals,
        "a repaint with unchanged rows must not request previews"
    );
    wheel(&mut visual, -2.0 * step);
    draw(&mut visual);
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_preview_deferrals()),
        deferrals + 1,
        "one scroll produces exactly one deferred request"
    );
    draw(&mut visual);
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_preview_deferrals()),
        deferrals + 1,
        "the paint that follows a scroll does not request again"
    );

    let next = voice.read_with(&*visual, |panel, _| panel.test_visible_screen_previews());
    assert_eq!(
        next,
        ids(&sources[6..14]),
        "the next scroll moves the visible window"
    );
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == visible.len() + 4
    });
    let attempts = seen.attempts();
    assert_eq!(
        &attempts[visible.len()..],
        labels(&next[4..]).as_slice(),
        "the rows that just scrolled into view were captured next"
    );
    let mut unique = attempts.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        attempts.len(),
        "a scrolled visibility list never captured a row twice"
    );
}

/// The count bound is checked before a capture starts: a full cache releases
/// the oldest off-screen pictures so the visible cards still get theirs, and a
/// refused picture never becomes a sticky memory note.
#[gpui::test]
async fn a_full_cache_evicts_off_screen_pictures_for_the_visible_cards(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..12)
        .map(|index| monitor(300 + index, &format!("Окно {index}"), 1920, 1080))
        .collect();
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources.clone());
    let seen = Recording::default();
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(seen.provider());
            let (max_count, _) = panel.test_screen_preview_bounds();
            for index in 0..max_count {
                let filler = monitor(9_000 + index as u64, "Заполнитель", 8, 8);
                assert!(
                    panel.test_inject_screen_preview(window, &filler, 8, 8),
                    "the filler fits the cache"
                );
            }
            assert_eq!(panel.test_screen_preview_counts().0, max_count);
        });
    });
    let released_before = voice.read_with(&*visual, |panel, _| panel.test_screen_images_released());
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        !panel.test_preview_batch_active() && panel.test_screen_preview_pending() == 0
    });

    let visible = voice.read_with(&*visual, |panel, _| panel.test_visible_screen_previews());
    assert!(!visible.is_empty(), "the top rows are visible at offset 0");
    assert_eq!(
        seen.attempts(),
        labels(&visible),
        "only the visible cards were handed to the capturer"
    );
    voice.read_with(&*visual, |panel, _| {
        let (count, bytes) = panel.test_screen_preview_counts();
        let (max_count, max_bytes) = panel.test_screen_preview_bounds();
        assert!(count <= max_count, "{count} > {max_count}");
        assert!(bytes <= max_bytes, "{bytes} > {max_bytes}");
        assert_eq!(
            panel.test_screen_preview_rejected(),
            0,
            "eviction must make room instead of refusing the visible frames"
        );
        for id in &visible {
            assert!(
                panel.test_screen_has_preview(*id),
                "the visible card kept a placeholder"
            );
            assert_eq!(
                panel.test_screen_preview_error(*id),
                None,
                "a full cache is not a card failure"
            );
        }
        assert!(
            panel.test_screen_images_released() > released_before,
            "the evicted pictures left the sprite atlas"
        );
    });

    // A later request finds the visible rows cached and starts no capturer.
    let attempts = seen.attempts();
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        !panel.test_preview_batch_active() && panel.test_screen_preview_pending() == 0
    });
    assert_eq!(seen.attempts(), attempts, "nothing new needed a capture");
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_preview_rejected(), 0);
        for id in &visible {
            assert_eq!(panel.test_screen_preview_error(*id), None, "no sticky note");
        }
    });
}

/// The byte half of the same gate: 41 pictures still fit the count bound but
/// leave less than one estimated frame. The oldest off-screen frames are
/// evicted for the visible cards; the byte bound itself still holds.
#[gpui::test]
async fn a_byte_full_cache_evicts_off_screen_pictures_too(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..12)
        .map(|index| monitor(400 + index, &format!("Окно {index}"), 1920, 1080))
        .collect();
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources.clone());
    let seen = Recording::default();
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(seen.big_provider());
            let (_, max_bytes) = panel.test_screen_preview_bounds();
            let frame = 480 * 270 * 4;
            let mut index = 0;
            while panel.test_screen_preview_counts().1 + frame <= max_bytes {
                let filler = monitor(9_100 + index as u64, "Заполнитель", 8, 8);
                assert!(panel.test_inject_screen_preview(window, &filler, 480, 270));
                index += 1;
            }
            assert_eq!(index, 32, "the byte budget fits 32 full frames");
            // One tiny picture still fits and leaves less than one estimated
            // 480×270 frame (518 400 bytes).
            let filler = monitor(9_200, "Заполнитель", 8, 8);
            assert!(panel.test_inject_screen_preview(window, &filler, 8, 8));
            let count = panel.test_screen_preview_counts().0;
            assert!(
                count < panel.test_screen_preview_bounds().0,
                "the byte budget fills before the count bound"
            );
        });
    });
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        !panel.test_preview_batch_active() && panel.test_screen_preview_pending() == 0
    });

    let visible = voice.read_with(&*visual, |panel, _| panel.test_visible_screen_previews());
    assert_eq!(
        seen.attempts(),
        labels(&visible),
        "only the visible cards were handed to the capturer"
    );
    voice.read_with(&*visual, |panel, _| {
        let (count, bytes) = panel.test_screen_preview_counts();
        let (max_count, max_bytes) = panel.test_screen_preview_bounds();
        assert!(count <= max_count, "{count} > {max_count}");
        assert!(bytes <= max_bytes, "{bytes} > {max_bytes}");
        assert_eq!(
            panel.test_screen_preview_rejected(),
            0,
            "the visible frames were kept, not refused"
        );
        for id in &visible {
            assert!(panel.test_screen_has_preview(*id));
            assert_eq!(panel.test_screen_preview_error(*id), None);
        }
    });
}

/// Switching the tab while a batch is queued must not capture the hidden
/// category: the batch re-checks the category before every capture, and the
/// category switch drops the pending ids of the old one.
#[gpui::test]
async fn switching_the_tab_during_a_batch_never_captures_the_old_category(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let screen = monitor(3, "Экран A", 1920, 1080);
    let document = window(11, "Документ");
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![screen.clone()],
    );
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_replace_screen_list_without_refresh(SourceKind::Window, vec![document]);
        });
    });
    let seen = Recording::default();
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(seen.provider())
        });
    });
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx);
            assert!(panel.test_preview_batch_active());
            assert_eq!(panel.test_screen_preview_pending(), 1);
        });
    });
    assert!(
        seen.attempts().is_empty(),
        "the batch is queued, not started, so the switch races its first check"
    );

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_switch_share_kind(SourceKind::Window, window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == 1
    });
    assert_eq!(
        seen.attempts(),
        ["window:11".to_owned()],
        "the old category never reached the capturer"
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_preview_pending(), 0);
        assert!(!panel.test_preview_batch_active());
    });
}

/// A full cache must not lock the other tab out: the visible rows of the new
/// category evict the pictures of the hidden one and get their own.
#[gpui::test]
async fn a_tab_switch_with_a_full_cache_evicts_the_old_tab_and_gets_pictures(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let screen = monitor(300, "Экран A", 1920, 1080);
    let document = window(11, "Документ");
    let document_id = document.source_id();
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![screen.clone()],
    );
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_replace_screen_list_without_refresh(SourceKind::Window, vec![document]);
        });
    });
    let seen = Recording::default();
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(seen.provider());
            assert!(
                panel.test_inject_screen_preview(window, &screen, 8, 8),
                "the old tab picture is cached"
            );
            let (max_count, _) = panel.test_screen_preview_bounds();
            for index in 1..max_count {
                let filler = monitor(9_300 + index as u64, "Заполнитель", 8, 8);
                assert!(panel.test_inject_screen_preview(window, &filler, 8, 8));
            }
            assert_eq!(panel.test_screen_preview_counts().0, max_count);
        });
    });
    let released_before = voice.read_with(&*visual, |panel, _| panel.test_screen_images_released());

    // The new tab is painted only after the switch, so its first claim must
    // free the room itself instead of refusing the visible card.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_switch_share_kind(SourceKind::Window, window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_has_preview(document_id)
    });

    assert_eq!(
        seen.attempts(),
        ["window:11".to_owned()],
        "only the new tab's visible card was captured"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            !panel.test_screen_has_preview(screen.source_id()),
            "the old tab picture was evicted"
        );
        let (count, bytes) = panel.test_screen_preview_counts();
        let (max_count, max_bytes) = panel.test_screen_preview_bounds();
        assert!(count <= max_count, "{count} > {max_count}");
        assert!(bytes <= max_bytes, "{bytes} > {max_bytes}");
        assert_eq!(
            panel.test_screen_preview_rejected(),
            0,
            "a full cache is not a memory failure"
        );
        assert_eq!(panel.test_screen_preview_error(screen.source_id()), None);
        assert_eq!(panel.test_screen_preview_error(document_id), None);
        assert!(
            panel.test_screen_images_released() > released_before,
            "the evicted pictures left the sprite atlas"
        );
    });
}

/// A capture that is already in flight when the user switches tabs must not
/// land in the new picker, even with the same epoch.
#[gpui::test]
async fn a_capture_in_flight_during_a_tab_switch_never_lands(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let screen = monitor(300, "Экран A", 1920, 1080);
    let document = window(11, "Документ");
    let document_id = document.source_id();
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![screen.clone()],
    );
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_replace_screen_list_without_refresh(SourceKind::Window, vec![document]);
        });
    });
    let seen = Recording::default();
    let epoch = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(seen.provider())
        });
    });

    // The uncached visible card is claimed but the capture has not run yet: it
    // is in flight when the switch happens.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx);
            assert!(panel.test_preview_batch_active());
            assert_eq!(panel.test_screen_preview_pending(), 1);
        });
    });
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_switch_share_kind(SourceKind::Window, window, cx)
        });
    });
    // A result of that capture arrives after the switch: it belongs to the tab
    // the user left and must not enter the window picker.
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            assert!(
                !panel.test_deliver_screen_preview(window, epoch, &screen, true),
                "a late frame of the old tab is dropped"
            );
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_has_preview(document_id)
    });

    assert_eq!(
        seen.attempts(),
        ["window:11".to_owned()],
        "the old tab never reached the capturer"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            !panel.test_screen_has_preview(screen.source_id()),
            "the late frame never entered the new tab"
        );
        assert_eq!(
            panel.test_screen_preview_error(screen.source_id()),
            None,
            "a cancelled capture is not a card failure"
        );
        assert_eq!(panel.test_screen_preview_pending(), 0);
        assert_eq!(panel.test_screen_preview_rejected(), 0);
    });
}

/// A result of the previous picker carries its epoch; a new picker (and its
/// category) must reject it even when the old source is still cached.
#[gpui::test]
async fn a_late_frame_of_the_previous_session_does_not_land_in_the_new_picker(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let screen = monitor(3, "Экран A", 1920, 1080);
    let document = window(11, "Документ");
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![screen.clone()],
    );
    let seen = Recording::default();
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(seen.provider())
        });
    });
    let old_epoch = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx);
            assert!(panel.test_preview_batch_active());
        });
    });

    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.test_close_screen_share(window, cx));
    });
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Window,
        vec![document.clone()],
    );
    let new_epoch = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    assert_ne!(new_epoch, old_epoch, "a new picker moves the epoch");
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            assert!(
                !panel.test_deliver_screen_preview(window, old_epoch, &screen, true),
                "a frame of the closed picker is ignored"
            );
            assert!(
                !panel.test_screen_has_preview(screen.source_id()),
                "the old monitor picture did not enter the window picker"
            );
        });
    });

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == 1
    });
    assert_eq!(
        seen.attempts(),
        ["window:11".to_owned()],
        "the closed monitor batch never captured after the reopen"
    );
}
