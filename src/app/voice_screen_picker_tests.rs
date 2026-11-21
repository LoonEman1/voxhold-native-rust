//! Picker geometry and thumbnail-session regressions: explicit picture
//! slots, measured text, separate scrolling, DPI generation and budget
//! eviction. The cache/session rules themselves stay in
//! `voice_screen_preview_tests`.

use super::voice_screen_preview_tests::RecordingPreview;
use super::voice_screen_tests::{draw, monitor, open_picker_with, wait_for};
use gpui::TestAppContext;
use std::sync::{atomic::Ordering, Arc};
use voxhold_media::screen_capture::{ScreenSource, SourceKind};

/// P05: when the byte budget is full, a card the user is looking at still wins
/// over pictures scrolled out of view. The released card gets the honest
/// memory reason instead of silently keeping the budget inflated.
#[gpui::test]
async fn a_visible_thumbnail_evicts_hidden_cards_instead_of_missing_the_budget(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..40)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    let visible_id = sources[0].source_id();
    let picker = sources.clone();
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(SourceKind::Monitor, picker, None);
        });
    });
    draw(&mut visual);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            // Fill the cache with off-screen rows first.
            for source in sources.iter().skip(2) {
                panel.test_inject_screen_preview(source, 480, 270);
            }
            let (_, max_bytes) = panel.test_screen_preview_bounds();
            assert!(
                panel.test_screen_preview_counts().1 + 480 * 270 * 4 > max_bytes,
                "the cache is full to within one picture"
            );
            // The card in the visible first row must still be admitted.
            assert!(panel.test_inject_screen_preview(&sources[0], 480, 270));
            let (count, bytes) = panel.test_screen_preview_counts();
            assert!(count <= 64);
            assert!(bytes <= max_bytes, "the byte bound is still enforced");
            assert!(
                panel.test_screen_has_preview(visible_id),
                "the visible card owns its picture"
            );
            let evicted = sources
                .iter()
                .skip(2)
                .filter_map(|source| panel.test_screen_preview_error(source.source_id()))
                .any(|reason| reason.contains("лимит памяти"));
            assert!(evicted, "an off-screen picture was released with a reason");
        });
    });
}

/// P05: the selected card is protected even when it is scrolled far away.
/// Evicting it would leave the dialog it is about to publish with a permanent
/// "превышен лимит памяти" note.
#[gpui::test]
async fn the_selected_card_is_never_evicted_for_a_visible_card(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..40)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    let selected = sources[39].source_id();
    let visible = sources[0].source_id();
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(
                SourceKind::Monitor,
                sources.clone(),
                Some(selected),
            );
            // Both the selected far row and the first row are outside the
            // painted viewport.
            panel.test_set_preview_viewport(2, 0, 0, cx);
            // 31 hidden frames plus the selected one: 32 full 480×270 frames
            // fit the 16 MiB budget, so this is a cache at its count ceiling.
            for source in sources.iter().skip(7).take(31) {
                assert!(panel.test_inject_screen_preview(source, 480, 270));
            }
            assert!(panel.test_inject_screen_preview(&sources[39], 480, 270));
            let (_, max_bytes) = panel.test_screen_preview_bounds();
            assert_eq!(panel.test_screen_preview_counts().0, 32);
            // A visible card must enter by releasing a hidden one, but never
            // the selected surface.
            assert!(panel.test_inject_screen_preview(&sources[0], 480, 270));
            assert!(
                panel.test_screen_has_preview(selected),
                "the selected far card kept its picture"
            );
            assert!(
                panel.test_screen_has_preview(visible),
                "the visible card was admitted"
            );
            assert_eq!(panel.test_screen_preview_counts().0, 32);
            assert!(panel.test_screen_preview_counts().1 <= max_bytes);
            let evicted = sources
                .iter()
                .skip(7)
                .take(31)
                .filter(|source| source.source_id() != selected)
                .any(|source| {
                    panel
                        .test_screen_preview_error(source.source_id())
                        .is_some_and(|reason| reason == panel.test_preview_memory_reason())
                });
            assert!(evicted, "a hidden non-selected frame paid for the room");
        });
    });
}

/// P05: a card that comes back into the viewport drops only the memory
/// refusal and is captured again; its earlier failure was about the budget.
#[gpui::test]
async fn returning_to_the_viewport_retries_a_memory_refusal(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..40)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    let far = sources[39].source_id();
    let seen = RecordingPreview::default();
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(SourceKind::Monitor, sources.clone(), Some(far));
            panel.test_set_preview_provider(seen.provider());
            panel.test_set_preview_viewport(2, 0, 0, cx);
            let reason = panel.test_preview_memory_reason();
            panel.test_inject_screen_preview_error(&sources[39], reason);
            assert_eq!(
                panel.test_screen_preview_error(far).as_deref(),
                Some(reason)
            );
            // The selected row 19 becomes visible: the memory refusal must be
            // cleared and its capture requested again.
            panel.test_set_preview_viewport(2, 19, 20, cx);
            assert!(
                panel.test_screen_preview_error(far).is_none(),
                "a memory refusal does not survive the return to the viewport"
            );
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_has_preview(far)
    });
    assert!(
        seen.attempts().contains(&"monitor:81".to_owned()),
        "the returned card was captured again: {:?}",
        seen.attempts()
    );
}

#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn right(self) -> f32 {
        self.x + self.w
    }

    fn bottom(self) -> f32 {
        self.y + self.h
    }
}

fn bounds(visual: &mut gpui::VisualTestContext, selector: &'static str) -> Rect {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"));
    Rect {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        w: f32::from(bounds.size.width),
        h: f32::from(bounds.size.height),
    }
}

/// One picture inside its slot: same scale on both axes, fully inside the
/// mask, margins equal on the constrained axis.
fn assert_frame_fits_slot(
    visual: &mut gpui::VisualTestContext,
    key: &str,
    id: u64,
    frame_w: f32,
    frame_h: f32,
) {
    let slot = bounds(
        visual,
        Box::leak(format!("share-slot-{key}-{id}").into_boxed_str()),
    );
    let image = bounds(
        visual,
        Box::leak(format!("share-thumb-{key}-{id}").into_boxed_str()),
    );
    let scale = (slot.w / frame_w).min(slot.h / frame_h);
    assert!(
        (image.w - frame_w * scale).abs() < 0.75 && (image.h - frame_h * scale).abs() < 0.75,
        "{key}-{id}: image {}×{}, expected {}×{}",
        image.w,
        image.h,
        frame_w * scale,
        frame_h * scale
    );
    assert!(image.x >= slot.x - 0.25);
    assert!(image.y >= slot.y - 0.25);
    assert!(image.right() <= slot.right() + 0.25);
    assert!(image.bottom() <= slot.bottom() + 0.25);
    let left = image.x - slot.x;
    let right = slot.w - left - image.w;
    let top = image.y - slot.y;
    let bottom = slot.h - top - image.h;
    if left + right > 1. {
        assert!(
            (left - right).abs() < 0.75,
            "horizontal margins {left}/{right}"
        );
    }
    if top + bottom > 1. {
        assert!(
            (top - bottom).abs() < 0.75,
            "vertical margins {top}/{bottom}"
        );
    }
}

/// P02: a 16:9, portrait, 4:3 and ultrawide frame are each fully visible
/// inside an explicit slot at both the normal two-column and the narrow
/// one-column width. The checked bounds are the painted image bounds, not the
/// source aspect. Measured fact from the base layout: a `Contain` image does
/// not crop its texture, the old picture slot was simply small (about 124×70
/// inside a ~286 px card) and the 236 px list viewport hid the second row.
#[gpui::test]
async fn every_preview_aspect_keeps_all_edges_at_normal_and_narrow_widths(cx: &mut TestAppContext) {
    let frames = [(320_u32, 180_u32), (90, 160), (240, 180), (420, 180)];
    for width in [1400., 520.] {
        let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(width), true);
        draw(&mut visual);
        let sources = vec![
            monitor(3, "Экран A", 1920, 1080),
            monitor(5, "Экран B", 1080, 1920),
            monitor(7, "Экран C", 1024, 768),
            monitor(9, "Экран D", 3440, 1440),
        ];
        visual.update(|_window, app| {
            voice.update(app, |panel, _| {
                panel.test_attach_detached_handle();
                panel.test_open_screen_picker_with(SourceKind::Monitor, sources.clone(), None);
                for (source, (w, h)) in sources.iter().zip(frames) {
                    assert!(panel.test_inject_screen_preview(source, w, h));
                }
            });
        });
        draw(&mut visual);
        for (source, (w, h)) in sources.iter().zip(frames) {
            assert_frame_fits_slot(&mut visual, "monitor", source.id, w as f32, h as f32);
        }
    }
}

/// P04: the title and size get the measured card width minus the footer
/// padding. A short label is not ellipsized by a zero-width text block, the
/// long title still truncates, and the error phrase has room for two lines.
#[gpui::test]
async fn the_card_text_block_is_measured_from_the_card_width(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let short = monitor(3, "Экран 1", 1920, 1080);
    let long = monitor(
        5,
        "Очень длинное кириллическое название окна с документом и вкладками",
        2560,
        1440,
    );
    let unknown = monitor(7, "Экран 3", 0, 0);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(
                SourceKind::Monitor,
                vec![short.clone(), long.clone(), unknown.clone()],
                None,
            );
            panel.test_inject_screen_preview_error(
                &long,
                "Предпросмотр недоступен: превышен лимит памяти.",
            );
        });
    });
    draw(&mut visual);
    for source in [&short, &long, &unknown] {
        let card = bounds(
            &mut visual,
            Box::leak(format!("share-source-card-monitor-{}", source.id).into_boxed_str()),
        );
        let title = bounds(
            &mut visual,
            Box::leak(format!("share-title-monitor-{}", source.id).into_boxed_str()),
        );
        let size = bounds(
            &mut visual,
            Box::leak(format!("share-size-monitor-{}", source.id).into_boxed_str()),
        );
        let inner = card.w - 2.;
        assert!(
            (title.w - (inner - 12.)).abs() < 0.75,
            "title of {} uses the footer content width: {} vs {}",
            source.id,
            title.w,
            inner - 12.
        );
        assert!((size.w - (inner - 12.)).abs() < 0.75);
        assert!(size.y >= title.y);
        assert!(size.bottom() <= card.bottom() + 0.25);
    }

    // The failed card paints a two-line reason inside the reserved footer.
    let card = bounds(&mut visual, "share-source-card-monitor-5");
    let reason = bounds(&mut visual, "share-thumb-error-line-monitor-5");
    assert!(reason.y >= card.y);
    assert!(reason.bottom() <= card.bottom() + 0.25);

    // The full title is reachable by hover. The tooltip is a window-level view,
    // so neither the card mask nor the scroll viewport clips it.
    let title = bounds(&mut visual, "share-title-monitor-5");
    let position = gpui::point(
        gpui::px(title.x + title.w / 2.),
        gpui::px(title.y + title.h / 2.),
    );
    visual.simulate_mouse_move(position, None, gpui::Modifiers::default());
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(650));
    visual.run_until_parked();
    draw(&mut visual);
    assert!(
        visual.debug_bounds("share-title-tooltip").is_some(),
        "hover reveals the full source title"
    );
}

/// P03/P04: several rows scroll in a separate viewport with a real scroll
/// range; a wheel and the Tab ring both reach the last card, which is fully
/// inside the viewport. The action row stays outside the scroll area.
#[gpui::test]
async fn the_last_card_is_reachable_by_wheel_and_tab_and_actions_stay_visible(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..7)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources.clone());
    let list = bounds(&mut visual, "share-sources");
    let first = bounds(&mut visual, "share-source-card-monitor-3");
    assert!(first.y >= list.y - 0.25);
    assert!(
        first.bottom() <= list.bottom() + 0.25,
        "a normal picker shows one whole card"
    );
    let scrollable = voice.read_with(&*visual, |panel, _| panel.test_screen_list_scrollable());
    assert!(
        scrollable > 0.5,
        "the multi-row list has a real scroll range"
    );

    let center = gpui::point(
        gpui::px(list.x + list.w / 2.),
        gpui::px(list.y + list.h / 2.),
    );
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: center,
        // A negative Y delta moves the list towards later rows.
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-2000.))),
        ..Default::default()
    });
    draw(&mut visual);
    let last = bounds(&mut visual, "share-source-card-monitor-15");
    assert!(last.y >= list.y - 0.25, "last card top");
    assert!(
        last.bottom() <= list.bottom() + 0.25,
        "the last card is fully visible after the wheel"
    );

    // Reopen and walk the Tab ring. Focus must scroll the last row into view.
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources.clone());
    for _ in 0..3 {
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(
        super::voice_screen_tests::focused(&mut visual, &voice),
        Some("Source(monitor:3)".to_owned())
    );
    for _ in 1..sources.len() {
        visual.simulate_keystrokes("tab");
    }
    draw(&mut visual);
    assert_eq!(
        super::voice_screen_tests::focused(&mut visual, &voice),
        Some("Source(monitor:15)".to_owned())
    );
    let list = bounds(&mut visual, "share-sources");
    let last = bounds(&mut visual, "share-source-card-monitor-15");
    assert!(
        last.y >= list.y - 0.25 && last.bottom() <= list.bottom() + 0.25,
        "Tab reveals the whole focused card"
    );

    for _ in 0..24 {
        if super::voice_screen_tests::focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(
        super::voice_screen_tests::focused(&mut visual, &voice),
        Some("Start".to_owned())
    );
    let card = bounds(&mut visual, "screen-share-card");
    let actions = bounds(&mut visual, "share-actions");
    assert!(
        actions.bottom() <= card.bottom() + 0.25,
        "the action row stays inside the modal"
    );
}

#[gpui::test]
async fn tab_reaches_the_actions_after_a_long_source_list(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    let sources = (1..=40)
        .map(|id| monitor(id, "Экран", 1920, 1080))
        .collect();
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources);
    for _ in 0..42 {
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(
        super::voice_screen_tests::focused(&mut visual, &voice).as_deref(),
        Some("Source(monitor:40)")
    );
    for _ in 0..24 {
        visual.simulate_keystrokes("tab");
        if super::voice_screen_tests::focused(&mut visual, &voice).as_deref() == Some("Start") {
            return;
        }
    }
    panic!("Tab never reached the start action");
}

/// P03: a scrolling grid paints a real bar from the scroll handle, inside the
/// viewport. Reserving `scrollbar_width` alone would leave a blank 8 px strip
/// and let the cards run under the painted thumb.
#[gpui::test]
async fn a_scrolling_grid_paints_its_bar_inside_the_viewport(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..10)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources.clone());
    let list = bounds(&mut visual, "share-sources");
    let scrollable = voice.read_with(&*visual, |panel, _| panel.test_screen_list_scrollable());
    assert!(scrollable > 0.5, "the grid really scrolls");
    let bar = bounds(&mut visual, "share-sources-scrollbar");
    let thumb = bounds(&mut visual, "share-sources-scrollbar-thumb");
    assert!(bar.x >= list.x - 0.25, "bar starts inside the viewport");
    assert!(
        bar.right() <= list.right() + 0.25,
        "bar right {} <= list right {}",
        bar.right(),
        list.right()
    );
    assert!(bar.y >= list.y - 0.25 && bar.bottom() <= list.bottom() + 0.25);
    assert!(
        thumb.y >= bar.y - 0.25 && thumb.bottom() <= bar.bottom() + 0.25,
        "the thumb stays inside its track"
    );
    assert!(
        thumb.h > 0. && thumb.h < bar.h,
        "a scrollable list has a thumb"
    );

    // The thumb follows the same scroll handle as the grid, not a static
    // decoration: after a wheel it moves down inside the track.
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: gpui::point(
            gpui::px(list.x + list.w / 2.),
            gpui::px(list.y + list.h / 2.),
        ),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-300.))),
        ..Default::default()
    });
    draw(&mut visual);
    let moved = bounds(&mut visual, "share-sources-scrollbar-thumb");
    assert!(
        moved.y > thumb.y + 0.5,
        "the thumb follows the offset: {} -> {}",
        thumb.y,
        moved.y
    );
    assert!(moved.bottom() <= bar.bottom() + 0.25);

    // Two columns plus their gap and the bar still fit the content width: the
    // right card never extends under the bar.
    let left = bounds(&mut visual, "share-source-card-monitor-3");
    let right = bounds(&mut visual, "share-source-card-monitor-5");
    assert!(
        right.right() <= bar.x + 0.25,
        "the cards end before the bar: list {:?} left {:?} right {:?} bar {:?}",
        (list.x, list.w),
        (left.x, left.w),
        (right.x, right.w),
        (bar.x, bar.w)
    );
    assert!(right.right() <= list.right() + 0.25);
    assert!(left.right() < right.x);
}

/// P03: reopening the picker through the production «Экран» path starts the
/// grid at the top; the previous dialog's scroll handle and visible rows do
/// not survive.
#[gpui::test]
async fn reopening_the_picker_resets_the_grid_scroll(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..10)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_set_share_gate(Ok(()));
            panel.test_open_screen_share(cx);
            panel.test_apply_screen_sources(SourceKind::Monitor, sources.clone());
        });
    });
    draw(&mut visual);
    draw(&mut visual);
    let list = bounds(&mut visual, "share-sources");
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: gpui::point(
            gpui::px(list.x + list.w / 2.),
            gpui::px(list.y + list.h / 2.),
        ),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-2000.))),
        ..Default::default()
    });
    draw(&mut visual);
    let scrolled = voice.read_with(&*visual, |panel, _| panel.test_screen_list_offset());
    assert!(scrolled > 0.5, "the first picker really scrolled");

    visual.update(|_window, app| {
        voice.update(app, |panel, cx| panel.test_close_screen_share(cx));
    });
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_open_screen_share(cx);
            panel.test_apply_screen_sources(SourceKind::Monitor, sources.clone());
        });
    });
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(
            panel.test_screen_list_offset(),
            0.,
            "a reopened picker starts at the top"
        );
    });
}

/// P05: a DPI/width change must not start a second capture while the previous
/// one is still in flight. The generation request waits for the capture latch
/// and captures once after it is released; the provider's concurrency peak
/// stays at one.
#[gpui::test]
async fn a_width_change_never_runs_two_captures_at_once(cx: &mut TestAppContext) {
    use std::sync::atomic::AtomicUsize;
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let source = monitor(3, "Экран A", 1920, 1080);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![source.clone()],
    );
    let seen = RecordingPreview::default();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    {
        let seen = seen.clone();
        let in_flight = in_flight.clone();
        let peak = peak.clone();
        visual.update(|_window, app| {
            voice.update(app, |panel, _| {
                panel.test_set_preview_provider(Arc::new(move |source| {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    let result = seen.provider()(source);
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    result
                }));
            });
        });
    }

    // Model a capture that already reached the provider but has not returned,
    // as during a DPI move. The new width may not start a second one.
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_preview_capture_in_flight(true);
            panel.test_force_preview_width(288);
            panel.test_set_preview_width(432, cx);
            assert!(
                !panel.test_preview_batch_active(),
                "no batch may start beside the in-flight capture"
            );
            assert_eq!(
                panel.test_screen_preview_pending(),
                0,
                "the new generation queued no capture yet"
            );
            assert!(
                seen.attempts().is_empty(),
                "the provider was not entered twice"
            );
        });
    });

    // The old capture returns: its batch releases the latch and the current
    // generation captures at the new width exactly once.
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_preview_capture_in_flight(false);
            panel.test_request_screen_previews(cx);
            assert!(panel.test_preview_batch_active());
            assert_eq!(panel.test_screen_preview_pending(), 1);
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == 1
            && panel.test_screen_preview_pending() == 0
            && !panel.test_preview_batch_active()
    });
    assert_eq!(peak.load(Ordering::SeqCst), 1, "captures never overlapped");
    let final_width = voice.read_with(&*visual, |panel, _| panel.test_screen_preview_width());
    assert!(
        final_width >= 432,
        "the generation kept the sharper request width: {final_width}"
    );
}

/// P03: a small window scrolls the settings and the grid instead of pushing
/// the Cancel/Start row below the modal.
#[gpui::test]
async fn a_compact_window_keeps_the_actions_inside_the_modal(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(900.), true);
    visual.simulate_resize(gpui::size(gpui::px(900.), gpui::px(430.)));
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..6)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources);
    let card = bounds(&mut visual, "screen-share-card");
    let actions = bounds(&mut visual, "share-actions");
    assert!(actions.y >= card.y, "actions stay inside the card top");
    assert!(
        actions.bottom() <= card.bottom() + 0.25,
        "actions stay inside the compact card"
    );
    let list = bounds(&mut visual, "share-sources");
    assert!(
        list.bottom() <= actions.y + 0.25,
        "the grid ends before the fixed action row"
    );
}

/// P03: at the normal 660 px dialog the settings fit without hidden rows, so
/// the codec line the user must read before Start is visible without
/// scrolling the settings block. A compact window may scroll instead.
#[gpui::test]
async fn the_normal_dialog_shows_the_codec_line_inside_the_settings(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    let sources: Vec<ScreenSource> = (0..4)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources);
    let card = bounds(&mut visual, "screen-share-card");
    let settings = bounds(&mut visual, "share-settings");
    let codec = bounds(&mut visual, "share-codec");
    assert!((card.h - 660.).abs() < 0.75, "the normal dialog height");
    assert!(
        codec.y >= settings.y - 0.25,
        "the codec row is below the top"
    );
    assert!(
        codec.bottom() <= settings.bottom() + 0.25,
        "the codec row is not hidden in the settings scroll: codec {} settings {}",
        codec.bottom(),
        settings.bottom()
    );
    assert!(settings.bottom() <= card.bottom() + 0.25);
}

/// P03: an empty category paints a message, not a grid with the height of a
/// row it does not have, and the actions remain available.
#[gpui::test]
async fn an_empty_picker_does_not_reserve_the_grid_height(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, Vec::new());
    let list = bounds(&mut visual, "share-sources");
    assert!(
        list.h < 80.,
        "an empty category has no grid row: {}",
        list.h
    );
    assert!(visual.debug_bounds("share-sources-empty").is_some());
    assert!(visual.debug_bounds("share-actions").is_some());
}

/// P05: a larger card on a higher-DPI monitor starts a new preview
/// generation. The old epoch is ignored, the cache is released and the next
/// request carries the new width.
#[gpui::test]
async fn a_larger_thumbnail_request_starts_a_new_preview_generation(cx: &mut TestAppContext) {
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
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            assert!(panel.test_inject_screen_preview(&source, 8, 8));
            assert_eq!(panel.test_screen_preview_counts(), (1, 8 * 8 * 4));
        });
    });
    let first_width = voice.read_with(&*visual, |panel, _| panel.test_screen_preview_width());
    assert!(first_width >= 286, "the first frame measured the card");
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_force_preview_width(288);
            panel.test_set_preview_width(432, cx);
            assert_eq!(panel.test_screen_preview_width(), 432);
            assert_ne!(
                panel.test_preview_epoch(),
                old_epoch,
                "a new request moves the epoch"
            );
            assert_eq!(
                panel.test_screen_preview_counts(),
                (0, 0),
                "the smaller cache is released"
            );
            assert!(
                !panel.test_deliver_screen_preview(old_epoch, &source, true),
                "a late frame of the old generation cannot fill the new card"
            );
        });
    });
}

/// P05: the capture batch serves rows inside the last painted viewport
/// before rows scrolled out of it. The list is really scrolled to the bottom,
/// so its order differs from insertion order; only a visible-first ranking
/// captures those rows first.
#[gpui::test]
async fn visible_rows_are_captured_before_hidden_rows(cx: &mut TestAppContext) {
    // Six cards at two columns make three rows; after scrolling, the viewport
    // stands on a later row while the first row is off screen.
    let sources: Vec<ScreenSource> = (0..6)
        .map(|index| monitor(3 + index * 2, "Экран", 1920, 1080))
        .collect();
    let (mut visual, _view, voice) = super::voice_tests::fixture_with(cx, Some(1400.), true);
    draw(&mut visual);
    open_picker_with(&mut visual, &voice, SourceKind::Monitor, sources.clone());
    let list = bounds(&mut visual, "share-sources");
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: gpui::point(
            gpui::px(list.x + list.w / 2.),
            gpui::px(list.y + list.h / 2.),
        ),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-2000.))),
        ..Default::default()
    });
    draw(&mut visual);
    let (top, bottom) = voice
        .read_with(&*visual, |panel, _| panel.test_screen_visible_rows())
        .expect("the scrolled list measured its rows");
    assert!(
        top > 0,
        "the first row is really off screen: {top}..{bottom}"
    );

    let seen = RecordingPreview::default();
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_preview_provider(seen.provider());
            panel.test_request_screen_previews(cx);
        });
    });
    wait_for(&mut visual, &voice, |panel| {
        panel.test_screen_preview_counts().0 == sources.len()
    });
    let attempts = seen.attempts();
    let visible_indexes: Vec<usize> = (top * 2..=(bottom * 2 + 1))
        .filter(|index| *index < sources.len())
        .collect();
    let visible_count = visible_indexes.len();
    let visible: std::collections::HashSet<_> =
        attempts.iter().take(visible_count).cloned().collect();
    let expected: std::collections::HashSet<_> = visible_indexes
        .iter()
        .map(|index| format!("{:?}", sources[*index].source_id()))
        .collect();
    assert_eq!(
        visible, expected,
        "rows {top}..{bottom} on screen are captured first: {attempts:?}"
    );
    let hidden: std::collections::HashSet<_> =
        attempts.iter().skip(visible_count).cloned().collect();
    let expected_hidden: std::collections::HashSet<_> = sources
        .iter()
        .enumerate()
        .filter(|(index, _)| !visible_indexes.contains(index))
        .map(|(_, source)| format!("{:?}", source.source_id()))
        .collect();
    assert_eq!(
        hidden, expected_hidden,
        "the off-screen rows follow: {attempts:?}"
    );
}
