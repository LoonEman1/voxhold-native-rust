//! O07: one `/thumbnail` download per attachment, reused by every 16 px width
//! bucket. The request count is read from the real `TestServer` log, so the
//! splitter-drag claim is measured and not inferred from cache state.

use super::add_auth_window;
use super::media::png_bytes;
use super::*;
use crate::media::{
    ImageCache, Media, ThumbKey, ThumbSourceKey, THUMB_FAILURES, THUMB_SOURCE_BUDGET,
};
use crate::test_support::{Gate, Reply, TestServer};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Run the GPUI loop until `ready` holds. Fetches and local resizes finish on
/// background threads, so every iteration yields a real 5 ms sleep.
fn wait_for(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    mut ready: impl FnMut(&Voxhold) -> bool,
) -> bool {
    for _ in 0..400 {
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| ready(v)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn thumbnail_requests(server: &TestServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| request.path.ends_with("/thumbnail"))
        .count()
}

fn connect(visual: &mut VisualTestContext, view: &Entity<Voxhold>, server: &TestServer) {
    visual.update(|_, app| {
        view.update(app, |v, _| {
            v.client = Some(Client::new(&server.base).unwrap());
            v.server = 1;
            v.channel = 7;
        });
    });
}

fn request(visual: &mut VisualTestContext, view: &Entity<Voxhold>, key: ThumbKey) {
    visual.update(|window, app| {
        view.update(app, |v, cx| v.request_thumbnail(key, window, cx));
    });
}

fn request_source(
    visual: &mut VisualTestContext,
    view: &Entity<Voxhold>,
    id: &str,
    key: ThumbKey,
) -> bool {
    request(visual, view, key);
    wait_for(visual, view, |v| {
        v.media
            .thumb_sources
            .contains(&ThumbSourceKey(id.to_string()))
    })
}

fn variant_ready(visual: &mut VisualTestContext, view: &Entity<Voxhold>, key: &ThumbKey) -> bool {
    wait_for(visual, view, |v| v.media.thumbs.contains(key))
}

#[gpui::test]
async fn one_thumbnail_request_serves_every_width_bucket(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = TestServer::spawn(vec![Reply::image(200, png_bytes(400, 300))]);
    connect(&mut visual, &view, &server);

    let id = "a".repeat(32);
    let first = ThumbKey::new(&id, 112., 64.);
    let second = ThumbKey::new(&id, 208., 96.);
    let third = ThumbKey::new(&id, 304., 160.);
    assert!(
        request_source(&mut visual, &view, &id, first.clone()),
        "the first bucket downloads one source"
    );
    // This is what the next frame does after the source notification: all
    // visible buckets ask again and are resized locally from the source.
    for key in [first.clone(), second.clone(), third.clone()] {
        request(&mut visual, &view, key);
    }
    assert!(
        variant_ready(&mut visual, &view, &first)
            && variant_ready(&mut visual, &view, &second)
            && variant_ready(&mut visual, &view, &third),
        "every visible width bucket is built from the one source"
    );

    assert_eq!(
        thumbnail_requests(&server),
        1,
        "three buckets, one HTTP request: {:?}",
        server.requests()
    );
    let sizes = visual.update(|_, app| {
        view.update(app, |v, _| {
            [first.clone(), second.clone(), third].map(|key| {
                let image = v.media.thumbs.get(&key).expect("variant");
                (image.size(0).width.0, image.size(0).height.0)
            })
        })
    });
    assert_eq!(sizes, [(112, 64), (208, 96), (304, 160)]);
}

#[gpui::test]
async fn a_cached_source_rebuilds_an_evicted_variant_without_http(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = TestServer::spawn(vec![Reply::image(200, png_bytes(400, 300))]);
    connect(&mut visual, &view, &server);

    let id = "b".repeat(32);
    let key = ThumbKey::new(&id, 112., 64.);
    assert!(request_source(&mut visual, &view, &id, key.clone()));
    assert!(variant_ready(&mut visual, &view, &key));
    assert_eq!(thumbnail_requests(&server), 1);

    // Simulate the variant LRU dropping only the draw key.
    visual.update(|_, app| {
        view.update(app, |v, _| {
            assert!(v.media.thumbs.remove(&key).is_some());
            assert!(!v.media.thumbs.contains(&key));
            assert!(v.media.thumb_sources.contains(&ThumbSourceKey(id.clone())));
        });
    });
    request(&mut visual, &view, key.clone());
    assert!(variant_ready(&mut visual, &view, &key));
    assert_eq!(
        thumbnail_requests(&server),
        1,
        "the source was reused, so the variant came back without HTTP"
    );
}

#[test]
fn the_source_cache_respects_its_byte_budget_and_evicts_the_oldest() {
    let media = Media::default();
    assert_eq!(media.thumb_sources.budget(), THUMB_SOURCE_BUDGET);
    assert_eq!(THUMB_SOURCE_BUDGET, 24 << 20);

    let mut cache: ImageCache<ThumbSourceKey, Arc<image::DynamicImage>> = ImageCache::new(1000);
    let source = |value: u8| {
        Arc::new(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(10, 10, image::Rgba([value, value, value, 255])),
        ))
    };
    let bytes = crate::media::source_bytes(&source(1));
    assert_eq!(bytes, 400, "10×10 RGBA is measured exactly");
    let keys = ["a", "b", "c"].map(|name| ThumbSourceKey(name.into()));
    assert!(cache.put(keys[0].clone(), source(1), bytes).is_empty());
    assert!(cache.put(keys[1].clone(), source(2), bytes).is_empty());
    assert!(cache.get(&keys[0]).is_some(), "touch a, so b is the oldest");
    let evicted = cache.put(keys[2].clone(), source(3), bytes);
    assert_eq!(evicted.len(), 1, "the budget evicts one source");
    assert_eq!(cache.used_bytes(), 800);
    assert!(cache.get(&keys[1]).is_none());
    assert!(cache.get(&keys[0]).is_some());

    // A single source larger than the whole budget is not cached at all.
    assert!(cache.put(keys[1].clone(), source(4), 1200).is_empty());
    assert!(!cache.contains(&keys[1]));
    assert!(cache.used_bytes() <= 1000, "the byte limit still holds");

    // The live Media cache enforces the same limit: the 25th full-size 512 px
    // source (1 MiB each) evicts the oldest one instead of growing to 25 MiB.
    let mut media = Media::default();
    let full = Arc::new(image::DynamicImage::ImageRgba8(image::RgbaImage::new(
        512, 512,
    )));
    let full_bytes = crate::media::source_bytes(&full);
    assert_eq!(full_bytes, 1 << 20);
    for index in 0..25 {
        media.thumb_sources.put(
            ThumbSourceKey(format!("{index:032x}")),
            full.clone(),
            full_bytes,
        );
    }
    assert_eq!(media.thumb_sources.len(), 24);
    assert_eq!(media.thumb_sources.used_bytes(), THUMB_SOURCE_BUDGET);
}

#[test]
fn removing_an_attachment_drops_both_caches_and_its_pending_state() {
    let mut media = Media::default();
    let id = "c".repeat(32);
    let key = ThumbKey::new(&id, 16., 16.);
    let frame = image::Frame::new(image::RgbaImage::new(16, 16));
    let render = Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
    media.thumbs.put(key.clone(), render, 1024);
    media.thumb_sources.put(
        ThumbSourceKey(id.clone()),
        Arc::new(image::DynamicImage::ImageRgba8(image::RgbaImage::new(
            16, 16,
        ))),
        1024,
    );
    media.begin_thumb_load(&key);
    media.thumb_waiting.insert(key.clone());
    media.thumb_building.insert(ThumbKey::new(&id, 32., 32.));
    media.thumb_retry(&id, Some(429));

    let (images, _) = media.remove_attachment(&id);
    assert_eq!(images.len(), 1, "the GPU variant is returned for release");
    assert!(media.thumbs.is_empty());
    assert!(media.thumb_sources.is_empty());
    assert!(media.thumb_loads.is_empty());
    assert!(media.thumb_waiting.is_empty());
    assert!(media.thumb_building.is_empty());
    assert!(!media.thumb_blocked(&id, Instant::now()));
    assert!(media.is_purged(&id));
}

#[gpui::test]
async fn a_late_thumbnail_answer_after_a_purge_is_dropped(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let gate = Gate::new();
    gate.arm();
    let server = TestServer::spawn(vec![
        Reply::image(200, png_bytes(64, 48)).with_gate(gate.clone())
    ]);
    connect(&mut visual, &view, &server);

    let id = "d".repeat(32);
    let key = ThumbKey::new(&id, 112., 64.);
    request(&mut visual, &view, key);
    // Keep polling the deterministic executor while the background task
    // reaches the gated reply; a bare wait on the test thread would not let
    // the spawned fetch start inside the test context.
    let mut arrived = false;
    for _ in 0..100 {
        visual.run_until_parked();
        if gate.wait_arrived(Duration::from_millis(50)) {
            arrived = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(arrived, "the thumbnail request is in flight");
    let purge_id = id.clone();
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            v.purge_attachments(&[purge_id.clone()], window, cx);
        });
    });
    assert!(view.read_with(&*visual, |v, _| {
        v.media.thumbs.is_empty()
            && v.media.thumb_sources.is_empty()
            && v.media.thumb_loads.is_empty()
    }));

    gate.release();
    for _ in 0..40 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    view.read_with(&*visual, |v, _| {
        assert!(v.media.thumbs.is_empty(), "a late variant is not kept");
        assert!(
            v.media.thumb_sources.is_empty(),
            "a late source is not kept"
        );
        assert!(v.media.is_purged(&id));
    });
    assert_eq!(thumbnail_requests(&server), 1);
}

#[gpui::test]
async fn a_429_gate_blocks_every_bucket_of_the_same_id(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = TestServer::spawn(vec![Reply::image(429, png_bytes(8, 8))]);
    connect(&mut visual, &view, &server);

    let id = "e".repeat(32);
    let first = ThumbKey::new(&id, 112., 64.);
    let second = ThumbKey::new(&id, 208., 96.);
    request(&mut visual, &view, first);
    assert!(
        wait_for(&mut visual, &view, |v| v
            .media
            .thumb_failures
            .contains_key(&ThumbSourceKey(id.clone()))),
        "the 429 is recorded"
    );
    request(&mut visual, &view, second);
    assert_eq!(
        thumbnail_requests(&server),
        1,
        "the second bucket shares the retry gate instead of retrying"
    );
    assert!(view.read_with(&*visual, |v, _| v.media.thumb_blocked(&id, Instant::now())));
}

#[gpui::test]
async fn an_evicted_source_is_downloaded_again(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = TestServer::spawn(vec![
        Reply::image(200, png_bytes(400, 300)),
        Reply::image(200, png_bytes(400, 300)),
    ]);
    connect(&mut visual, &view, &server);

    let id = "f".repeat(32);
    let key = ThumbKey::new(&id, 112., 64.);
    assert!(request_source(&mut visual, &view, &id, key.clone()));
    assert!(variant_ready(&mut visual, &view, &key));
    assert_eq!(thumbnail_requests(&server), 1);

    // Simulate the byte-budget LRU dropping the decoded source together with
    // its draw variant. O07 promises reuse only "while the source is cached",
    // so the next request must download the picture again instead of showing
    // an empty cell.
    visual.update(|_, app| {
        view.update(app, |v, _| {
            assert!(v.media.thumbs.remove(&key).is_some());
            assert!(v
                .media
                .thumb_sources
                .remove(&ThumbSourceKey(id.clone()))
                .is_some());
        });
    });
    assert!(request_source(&mut visual, &view, &id, key.clone()));
    assert!(variant_ready(&mut visual, &view, &key));
    assert_eq!(
        thumbnail_requests(&server),
        2,
        "an evicted source has to be fetched again: {:?}",
        server.requests()
    );
}

#[gpui::test]
async fn a_late_answer_does_not_release_the_newer_load_marker(cx: &mut TestAppContext) {
    cx.update(|cx| gpui_component::init(cx));
    let (window, view) = add_auth_window(cx);
    let mut visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    let server = TestServer::spawn(vec![Reply::image(200, png_bytes(64, 48))]);
    connect(&mut visual, &view, &server);

    let id = "9".repeat(32);
    let source_key = ThumbSourceKey(id.clone());
    let key = ThumbKey::new(&id, 112., 64.);
    // The download starts, but its answer is not polled yet: the test thread
    // owns the executor, so the fetch only runs below.
    request(&mut visual, &view, key.clone());
    let stale = view
        .read_with(&*visual, |v, _| {
            v.media.thumb_loads.get(&source_key).copied()
        })
        .expect("the first load owns the marker");

    // What `invalidate_access` does to media state: a new generation and an
    // empty cache. A newer load of the same id then takes the slot. Its marker
    // is installed directly because a second gated fetch would block this
    // test executor inside `runtime.block_on` until the client timeout; the
    // completion path under test is the real one from `request_thumbnail`.
    visual.update(|window, app| {
        view.update(app, |v, _| {
            v.access_generation = v.access_generation.wrapping_add(1);
            v.clear_media(window);
            let newer = v.media.begin_thumb_load(&key);
            assert!(newer > stale, "tokens stay monotonic across a reset");
            v.media.thumb_waiting.insert(key.clone());
            assert_eq!(v.media.thumb_loads.get(&source_key), Some(&newer));
        });
    });

    for _ in 0..40 {
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }

    view.read_with(&*visual, |v, _| {
        let newer = v
            .media
            .thumb_loads
            .get(&source_key)
            .copied()
            .expect("the late answer leaves the newer marker in place");
        assert!(newer > stale);
        assert!(
            v.media.thumb_waiting.contains(&key),
            "the newer waiting bucket survives the late answer"
        );
        assert!(
            v.media.thumb_sources.is_empty(),
            "the stale payload is still dropped by the access guard"
        );
        assert!(
            v.media.thumb_failures.is_empty(),
            "the late answer does not install a retry gate for the newer load"
        );
    });
    assert_eq!(thumbnail_requests(&server), 1);
}

#[test]
fn a_superseded_load_keeps_the_newer_marker_and_waiting_buckets() {
    let mut media = Media::default();
    let id = "7".repeat(32);
    let first = ThumbKey::new(&id, 112., 64.);
    let second = ThumbKey::new(&id, 208., 96.);
    let stale = media.begin_thumb_load(&first);
    assert_eq!(media.thumb_loads.len(), 1);

    // An access reset drops every marker, but the token keeps counting, so a
    // later load of the same id can never reuse the old answer's token.
    drop(media.clear());
    let current = media.begin_thumb_load(&second);
    assert!(
        current > stale,
        "tokens stay monotonic across an access reset"
    );
    media.thumb_waiting.insert(second.clone());

    assert!(
        !media.finish_thumb_load(&id, stale),
        "a superseded load owns nothing"
    );
    assert_eq!(
        media.thumb_loads.get(&ThumbSourceKey(id.clone())),
        Some(&current)
    );
    assert!(
        media.thumb_waiting.contains(&second),
        "the newer waiting bucket survives the late answer"
    );

    assert!(media.finish_thumb_load(&id, current));
    assert!(media.thumb_loads.is_empty());
    assert!(media.thumb_waiting.is_empty());
}

#[test]
fn the_retry_gate_map_stays_bounded() {
    let mut media = Media::default();
    let total = THUMB_FAILURES + 20;
    for index in 0..total {
        media.thumb_retry(&format!("{index:032x}"), Some(500));
    }
    assert_eq!(media.thumb_failures.len(), THUMB_FAILURES);
    // The newest ids keep their gate, so a picture still on screen is not
    // retried on every frame after a long scroll through failures.
    let now = Instant::now();
    for index in (total - 20)..total {
        assert!(media.thumb_blocked(&format!("{index:032x}"), now));
    }
    // Overflow evicted one of the oldest gates, not a recent one.
    assert!(
        (0..20).any(|index| {
            !media
                .thumb_failures
                .contains_key(&ThumbSourceKey(format!("{index:032x}")))
        }),
        "the oldest gate is the one that leaves a full map"
    );
}
