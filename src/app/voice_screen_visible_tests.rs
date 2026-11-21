//! Visible-area thumbnail regressions: a scrolled picker must capture the rows
//! it paints first, a full cache must not start an off-screen capture, and a
//! tab switch or a closed picker must not let the old category through.

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
/// the batch must start with the rows the grid paints (plus one margin row),
/// not with the first row of the snapshot, and still reach the rest while the
/// cache has room.
#[gpui::test]
async fn scrolled_rows_are_captured_before_the_rest_of_the_tab(cx: &mut TestAppContext) {
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

    // Two rows down: 2 × (124 px card + 8 px gap).
    wheel(&mut visual, -2.0 * 132.0);
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
        panel.test_screen_preview_counts().0 == sources.len()
    });
    let attempts = seen.attempts();
    assert_eq!(
        &attempts[..visible.len()],
        labels(&visible).as_slice(),
        "the visible cards start the batch in visual order"
    );
    assert_eq!(
        attempts.len(),
        sources.len(),
        "the batch still reaches off-screen rows while the cache has room"
    );
    assert_eq!(seen.peak(), 1, "one capture at a time");
    let mut unique = attempts.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        attempts.len(),
        "a scrolled visibility list never captured a row twice"
    );
}

/// The cache bound is checked before a capture starts: a full cache keeps the
/// visible cards going (their honest memory note) and never spins up a
/// capturer for rows outside the viewport.
#[gpui::test]
async fn a_full_cache_only_starts_the_visible_captures(cx: &mut TestAppContext) {
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
        assert_eq!(
            panel.test_screen_preview_counts().0,
            panel.test_screen_preview_bounds().0,
            "the count bound held"
        );
        assert!(
            panel.test_screen_preview_rejected() >= visible.len() as u32,
            "the visible captures were refused honestly"
        );
    });
}

/// The byte half of the same gate: 41 pictures still fit the count bound but
/// leave less than one estimated card frame. A background card must not start
/// a capturer, while the visible cards are still tried and refused honestly.
#[gpui::test]
async fn a_byte_full_cache_only_starts_the_visible_captures(cx: &mut TestAppContext) {
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
            let frame = 320 * 320 * 4;
            let mut index = 0;
            while panel.test_screen_preview_counts().1 + frame <= max_bytes {
                let filler = monitor(9_100 + index as u64, "Заполнитель", 8, 8);
                assert!(panel.test_inject_screen_preview(window, &filler, 320, 320));
                index += 1;
            }
            assert_eq!(index, 40, "the byte budget fits 40 full frames");
            // One smaller picture still fits and leaves less than the
            // conservative estimate (288² × 9/16 × 4 ≈ 186 KiB).
            let filler = monitor(9_200, "Заполнитель", 8, 8);
            assert!(panel.test_inject_screen_preview(window, &filler, 320, 220));
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
        assert_eq!(
            panel.test_screen_preview_counts().0,
            41,
            "the refused frames did not enter the cache"
        );
        assert_eq!(
            panel.test_screen_preview_rejected(),
            visible.len() as u32,
            "each visible frame was refused for the byte budget"
        );
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
            assert_eq!(
                panel.test_screen_preview_counts(),
                (0, 0),
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
