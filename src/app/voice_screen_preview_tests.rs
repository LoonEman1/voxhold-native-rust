//! Picker grid and preview-cache regressions: layout, epoch, failure and
//! memory-bound cases of the screen-share cards.

use super::voice_screen_tests::{draw, monitor, open_picker_with, wait_for, window};
use gpui::TestAppContext;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use voxhold_media::screen_capture::{PreviewFrame, ScreenSource, SourceKind, ThumbnailError};

/// A deterministic preview provider that records every attempt and the highest
/// number of captures that were ever inside it at once. No capture stack is
/// involved.
#[derive(Clone, Default)]
struct RecordingPreview {
    attempted: Arc<Mutex<Vec<String>>>,
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl RecordingPreview {
    fn provider(
        &self,
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
                width: 4,
                height: 4,
                bgra: vec![0x30; 4 * 4 * 4],
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

/// Two cards per row at the dialog's normal width. The card selectors and the
/// painted-card count must match the snapshot one-to-one.
#[gpui::test]
async fn the_picker_grid_uses_two_columns_at_the_normal_width(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let first = monitor(3, "Экран A", 1920, 1080);
    let second = monitor(5, "Экран B", 2560, 1440);
    let third = monitor(7, "Экран C", 1280, 720);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![first, second, third],
    );
    let a = visual
        .debug_bounds("share-source-card-monitor-3")
        .expect("the first card is painted");
    let b = visual
        .debug_bounds("share-source-card-monitor-5")
        .expect("the second card is painted");
    let c = visual
        .debug_bounds("share-source-card-monitor-7")
        .expect("the third card is painted");
    let (ax, ay) = (f32::from(a.origin.x), f32::from(a.origin.y));
    let (bx, by) = (f32::from(b.origin.x), f32::from(b.origin.y));
    let (cx_, cy) = (f32::from(c.origin.x), f32::from(c.origin.y));
    assert!((ay - by).abs() < 0.5, "the first two cards share a row");
    assert!(bx > ax, "the second card is to the right");
    assert!((ax - cx_).abs() < 0.5, "the third card starts a new row");
    assert!(cy > ay, "the third card is below the first row");
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_preview_cards()),
        3,
        "exactly one card per source"
    );
}

/// A narrow dialog stacks the same cards in one column.
#[gpui::test]
async fn the_picker_grid_uses_one_column_at_a_narrow_width(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(520.), true);
    draw(&mut visual);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![
            monitor(3, "Экран A", 1920, 1080),
            monitor(5, "Экран B", 2560, 1440),
        ],
    );
    let a = visual
        .debug_bounds("share-source-card-monitor-3")
        .expect("the first card is painted");
    let b = visual
        .debug_bounds("share-source-card-monitor-5")
        .expect("the second card is painted");
    let (ax, ay) = (f32::from(a.origin.x), f32::from(a.origin.y));
    let (bx, by) = (f32::from(b.origin.x), f32::from(b.origin.y));
    assert!((ax - bx).abs() < 0.5, "one column shares the left edge");
    assert!(by > ay, "the second card is stacked below the first");
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_preview_cards()),
        2
    );
}

#[gpui::test]
async fn a_card_without_a_thumbnail_paints_the_pending_placeholder(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![monitor(3, "Экран A", 1920, 1080)],
    );
    assert!(
        visual
            .debug_bounds("share-thumb-placeholder-monitor-3")
            .is_some(),
        "a card without a picture says that the preview is pending"
    );
    assert!(
        visual.debug_bounds("share-thumb-monitor-3").is_none(),
        "no picture is invented"
    );
}

/// One selected source paints the thumbnail, the badge and the size, and the
/// stable id still drives the publish request.
#[gpui::test]
async fn a_selected_card_paints_its_thumbnail_and_badge(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let selected = monitor(3, "Экран A", 1920, 1080);
    let id = selected.source_id();
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![selected.clone()],
                Some(id),
            );
            assert!(panel.test_inject_screen_preview(window, &selected, 32, 18));
        });
    });
    draw(&mut visual);
    draw(&mut visual);
    assert!(
        visual.debug_bounds("share-thumb-monitor-3").is_some(),
        "the accepted picture is painted"
    );
    assert!(
        visual.debug_bounds("share-selected-monitor-3").is_some(),
        "the selected card carries the badge"
    );
    assert!(
        visual
            .debug_bounds("share-thumb-placeholder-monitor-3")
            .is_none(),
        "a picture replaces the placeholder"
    );
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_selected_source_id()),
        Some(id),
        "the card still publishes the stable kind+id"
    );
}

/// A failed preview is a card-local error: the selection and the request keep
/// the same source id, and an empty refresh still invalidates that selection.
#[gpui::test]
async fn a_failed_preview_does_not_change_the_selected_id_or_startable_source(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let selected = monitor(3, "Экран A", 1920, 1080);
    let id = selected.source_id();
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![selected.clone()],
                Some(id),
            );
            panel.test_inject_screen_preview_error(&selected, "Источник захвата закрыт.");
            assert_eq!(panel.test_selected_source_id(), Some(id));
            assert!(panel.test_can_start());
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("share-thumb-error-monitor-3").is_some(),
        "the card says the preview is unavailable"
    );
    assert!(
        visual.debug_bounds("share-thumb-monitor-3").is_none(),
        "a failed capture paints no picture"
    );
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_screen_preview_error(id).as_deref(),
            Some("Источник захвата закрыт.")
        );
        assert_eq!(panel.test_selected_source_id(), Some(id));
    });

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_start_screen_share(window, cx);
            assert_eq!(panel.test_take_screen_commands(), vec![("publish", 7)]);
            assert_eq!(panel.test_screen_request().source, selected);
        });
    });

    // The window closes before the next refresh: the card's failure message is
    // irrelevant now, and the absent id invalidates the selection.
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_apply_screen_sources(window, SourceKind::Monitor, Vec::new());
            assert_eq!(panel.test_selected_source_id(), None);
            assert!(!panel.test_can_start());
            assert!(
                panel.test_screen_error().contains("больше недоступен"),
                "the refresh explains the loss"
            );
        });
    });
}

/// Closing the picker moves the epoch; the late answer of the old session is
/// ignored and a new picker starts empty.
#[gpui::test]
async fn a_late_preview_from_a_closed_picker_is_ignored(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![source.clone()],
    );
    let old_epoch = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            assert_eq!(panel.test_screen_preview_counts().0, 1);
        });
    });
    visual.update(|window, app| {
        voice.update(app, |panel, cx| panel.test_close_screen_share(window, cx));
    });
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.screen_share_open());
        assert_ne!(panel.test_preview_epoch(), old_epoch, "the epoch moved");
        assert_eq!(panel.test_screen_preview_counts(), (0, 0));
        assert_eq!(panel.test_screen_preview_pending(), 0);
    });
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            assert!(
                !panel.test_deliver_screen_preview(window, old_epoch, &source, true),
                "a result of the closed picker is ignored"
            );
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
        });
    });

    // A new picker has its own epoch and starts with no pictures.
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![source.clone()],
    );
    let new_epoch = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    assert_ne!(new_epoch, old_epoch);
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            assert!(!panel.test_deliver_screen_preview(window, old_epoch, &source, true));
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            assert!(panel.test_deliver_screen_preview(window, new_epoch, &source, true));
            assert_eq!(panel.test_screen_preview_counts().0, 1);
        });
    });
}

/// A result for an id that the current snapshot no longer describes is
/// ignored, exactly like a closed window during capture.
#[gpui::test]
async fn a_preview_for_an_id_outside_the_snapshot_is_ignored(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let old = monitor(3, "Экран A", 1920, 1080);
    let new = monitor(5, "Экран B", 2560, 1440);
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, vec![old.clone()]);
    let epoch = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_apply_screen_sources(window, SourceKind::Monitor, vec![new.clone()]);
            assert!(!panel.test_deliver_screen_preview(window, epoch, &old, true));
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            assert!(panel.test_deliver_screen_preview(window, epoch, &new, true));
            assert_eq!(panel.test_screen_preview_counts().0, 1);
        });
    });
}

/// The cache enforces both hard bounds: 64 pictures and 16 MiB of BGRA.
#[gpui::test]
async fn the_preview_cache_enforces_its_count_and_byte_bounds(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_open_screen_picker_with(window, SourceKind::Monitor, Vec::new(), None);
            let (max_count, max_bytes) = panel.test_screen_preview_bounds();
            for index in 0..(max_count + 5) {
                let source = monitor(10_000 + index as u64, "Экран", 8, 8);
                panel.test_inject_screen_preview(window, &source, 8, 8);
            }
            let (count, bytes) = panel.test_screen_preview_counts();
            assert_eq!(count, max_count, "the count bound is exact");
            assert_eq!(panel.test_screen_preview_rejected(), 5);
            assert!(bytes <= max_bytes);

            // A fresh picker starts at zero and the byte bound rejects before
            // the count bound for large thumbnails.
            panel.test_open_screen_picker_with(window, SourceKind::Monitor, Vec::new(), None);
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            for index in 0..(max_count + 5) {
                let source = monitor(20_000 + index as u64, "Экран", 320, 320);
                panel.test_inject_screen_preview(window, &source, 320, 320);
            }
            let (count, bytes) = panel.test_screen_preview_counts();
            assert!(count < max_count, "the byte bound bound the cache first");
            assert!(bytes <= max_bytes, "{bytes} > {max_bytes}");
            assert!(panel.test_screen_preview_rejected() > 0);
        });
    });
}

/// Every session end moves the epoch and releases the cached bytes and pending
/// ids: a stop, the live restart dialog and a Failed/Publishing answer all
/// leave the bounded store empty, so the next picker starts from zero.
#[gpui::test]
async fn a_session_end_clears_the_preview_cache_and_moves_the_epoch(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    let bytes = 8 * 8 * 4;

    // A stop is an intent of the on-air card and ends the picker session.
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![source.clone()],
    );
    let before_stop = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            assert_eq!(panel.test_screen_preview_counts(), (1, bytes));
            panel.test_stop_screen_share(window, cx);
            assert_ne!(
                panel.test_preview_epoch(),
                before_stop,
                "a stop moves the preview epoch"
            );
            assert_eq!(
                panel.test_screen_preview_counts(),
                (0, 0),
                "a stop drops the cached bytes"
            );
            assert_eq!(panel.test_screen_preview_pending(), 0);
        });
    });

    // The live restart dialog has no picker grid: opening it must drop the
    // captured cards and reject their late answers.
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![source.clone()],
    );
    let before_restart = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                ..Sharing::default()
            });
            panel.test_set_screen_confirmed_source(source.clone());
            panel.test_open_live_screen_settings(window, cx);
            assert!(panel.test_screen_restart(), "the restart dialog opened");
            assert_ne!(
                panel.test_preview_epoch(),
                before_restart,
                "the restart dialog moves the preview epoch"
            );
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            assert_eq!(panel.test_screen_preview_pending(), 0);
        });
    });

    // A Failed answer closes the attempt and clears whatever the dialog held.
    let before_failed = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            assert!(panel.test_apply_screen_sharing(
                window,
                Sharing {
                    phase: Phase::Failed,
                    error: Some("Захват запрещён.".into()),
                    ..Sharing::default()
                }
            ));
            assert_ne!(
                panel.test_preview_epoch(),
                before_failed,
                "a failed attempt moves the preview epoch"
            );
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            assert_eq!(panel.test_screen_preview_pending(), 0);
        });
    });

    // A Publishing answer is the other phase transition that closes the
    // picker before Live arrives.
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![source.clone()],
    );
    let before_publishing = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            assert!(panel.test_apply_screen_sharing(
                window,
                Sharing {
                    phase: Phase::Publishing,
                    ..Sharing::default()
                }
            ));
            assert!(!panel.screen_share_open());
            assert_ne!(
                panel.test_preview_epoch(),
                before_publishing,
                "a publishing phase moves the preview epoch"
            );
            assert_eq!(panel.test_screen_preview_counts(), (0, 0));
            assert_eq!(panel.test_screen_preview_pending(), 0);
        });
    });
}

/// Replacing one card's picture must replace its byte accounting, not add to
/// it: the cache keeps only the latest frame per id, so the freed budget stays
/// available for the next card.
#[gpui::test]
async fn replacing_a_preview_does_not_inflate_the_byte_budget(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_open_screen_picker_with(window, SourceKind::Monitor, Vec::new(), None);
            let source = monitor(3, "Экран A", 1920, 1080);
            assert!(panel.test_inject_screen_preview(window, &source, 8, 8));
            assert_eq!(
                panel.test_screen_preview_counts(),
                (1, 8 * 8 * 4),
                "the first frame is counted once"
            );
            // The same id with a bigger frame replaces the old entry.
            assert!(panel.test_inject_screen_preview(window, &source, 16, 16));
            assert_eq!(
                panel.test_screen_preview_counts(),
                (1, 16 * 16 * 4),
                "only the latest frame of a replaced id is counted"
            );
            // A full-size frame for another card is still accepted: the
            // replacement did not leave a stale copy in the byte budget.
            let other = monitor(5, "Экран B", 2560, 1440);
            assert!(
                panel.test_inject_screen_preview(window, &other, 320, 320),
                "the budget was not inflated by the replacement"
            );
            assert_eq!(
                panel.test_screen_preview_counts(),
                (2, 16 * 16 * 4 + 320 * 320 * 4),
                "the two live frames are the only bytes counted"
            );
        });
    });
}

/// Refresh clears a failed card and retries only that source; the cached card
/// is not captured again.
#[gpui::test]
async fn refresh_retries_a_failed_preview_without_repeating_a_cached_card(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let cached = monitor(3, "Экран A", 1920, 1080);
    let failed = monitor(5, "Экран B", 2560, 1440);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![cached.clone(), failed.clone()],
    );
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_inject_screen_preview(window, &cached, 8, 8);
            panel.test_inject_screen_preview_error(&failed, "Предпросмотр не получен.");
            assert!(panel
                .test_screen_preview_error(failed.source_id())
                .is_some());
            panel.test_set_preview_provider(Arc::new(|_source| {
                Ok(PreviewFrame {
                    width: 4,
                    height: 4,
                    bgra: vec![0x10; 4 * 4 * 4],
                })
            }));
        });
    });
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_refresh_screen_sources(window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == 2
            && panel
                .test_screen_preview_error(failed.source_id())
                .is_none()
    });
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_preview_pending(), 0);
        assert_eq!(panel.test_screen_preview_counts().0, 2);
    });
}

/// Only the visible tab is captured; switching back queues the other kind,
/// never both at once.
#[gpui::test]
async fn only_the_visible_category_is_queued_for_previews(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let visible = monitor(3, "Экран A", 1920, 1080);
    let hidden = window(11, "Документ");
    visual.update(|window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(
                window,
                SourceKind::Monitor,
                vec![visible.clone()],
                None,
            );
            panel
                .test_replace_screen_list_without_refresh(SourceKind::Window, vec![hidden.clone()]);
        });
    });
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = seen.clone();
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(Arc::new(move |source| {
                if let Ok(mut list) = recorded.lock() {
                    list.push(format!("{:?}", source.source_id()));
                }
                Ok(PreviewFrame {
                    width: 4,
                    height: 4,
                    bgra: vec![0x20; 4 * 4 * 4],
                })
            }));
        });
    });
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_refresh_screen_sources(window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == 1
    });
    assert_eq!(
        seen.lock().expect("provider log").as_slice(),
        ["monitor:3".to_owned()],
        "the hidden window category was not captured"
    );

    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_switch_share_kind(SourceKind::Window, window, cx)
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == 2
    });
    assert_eq!(
        seen.lock().expect("provider log").as_slice(),
        ["monitor:3".to_owned(), "window:11".to_owned()],
        "the second batch captured only the visible window"
    );
    assert_eq!(
        voice.read_with(&*visual, |panel, _| panel.test_screen_preview_pending()),
        0
    );
}

/// A request that arrives while a batch owns the picker must not start a
/// second batch. The batch task is spawned, not polled immediately, so the
/// second request lands while the first batch is active and before its first
/// capture: it may only note the new row, which the same batch then captures
/// exactly once, after the current capture.
#[gpui::test]
async fn a_second_preview_request_joins_the_running_batch(cx: &mut TestAppContext) {
    let first = monitor(3, "Экран A", 1920, 1080);
    let second = monitor(5, "Экран B", 2560, 1440);
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![first.clone()],
    );

    let seen = RecordingPreview::default();
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_preview_provider(seen.provider())
        });
    });
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx);
            assert!(
                panel.test_preview_batch_active(),
                "the first request claimed the picker"
            );
            assert_eq!(panel.test_screen_preview_pending(), 1);
        });
    });
    assert!(
        seen.attempts().is_empty(),
        "the batch is queued, not started, so the next request races an active batch"
    );

    // A source-list update calls the production request again. It must only
    // record the new row: queueing it here would start a second batch beside
    // the one that already owns the picker.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_screen_sources(
                window,
                SourceKind::Monitor,
                vec![first.clone(), second.clone()],
            );
            panel.test_request_screen_previews(window, cx);
            assert!(
                panel.test_preview_batch_active(),
                "the guard still belongs to the first batch"
            );
            assert_eq!(
                panel.test_screen_preview_pending(),
                1,
                "the second request queued no capture of its own"
            );
            assert!(
                seen.attempts().is_empty(),
                "no capture started beside the running batch"
            );
        });
    });

    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == 2
    });
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_preview_pending(), 0);
        assert!(
            !panel.test_preview_batch_active(),
            "the finished batch released the guard"
        );
    });
    let mut attempts = seen.attempts();
    attempts.sort();
    assert_eq!(
        attempts,
        vec!["monitor:3".to_owned(), "monitor:5".to_owned()],
        "each currently visible source got exactly one attempt"
    );
    assert_eq!(
        seen.peak(),
        1,
        "the picked-up row was captured after the first one, never beside it"
    );
}

/// Closing the picker clears the guard at once, and the late end of that old
/// batch must not clear the flag of a batch started by a newer session.
#[gpui::test]
async fn a_late_preview_batch_cannot_clear_a_newer_batch(cx: &mut TestAppContext) {
    let source = monitor(3, "Экран A", 1920, 1080);
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![source.clone()],
    );

    let seen = RecordingPreview::default();
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
        voice.update(app, |panel, cx| {
            panel.test_close_screen_share(window, cx);
            assert!(!panel.screen_share_open());
            assert!(
                !panel.test_preview_batch_active(),
                "close releases the guard before the old batch answers"
            );
            assert_eq!(panel.test_screen_preview_pending(), 0);
        });
    });

    // A new picker starts its own batch before the old task ever ran.
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![source.clone()],
    );
    let new_epoch = voice.read_with(&*visual, |panel, _| panel.test_preview_epoch());
    assert_ne!(new_epoch, old_epoch, "a new picker has a new epoch");
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_request_screen_previews(window, cx);
            assert!(panel.test_preview_batch_active());
        });
    });

    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            assert!(
                !panel.test_end_preview_batch(old_epoch),
                "a stale epoch may not end the newer batch"
            );
            assert!(
                panel.test_preview_batch_active(),
                "the stale end left the newer guard alone"
            );
        });
    });

    // Let both queued tasks run: the old one must stay inert, the new one
    // fills the picker and releases the guard itself.
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == 1
    });
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_screen_preview_pending(), 0);
        assert!(!panel.test_preview_batch_active());
    });
    assert_eq!(
        seen.attempts(),
        vec!["monitor:3".to_owned()],
        "only the current picker captured the source"
    );
}
