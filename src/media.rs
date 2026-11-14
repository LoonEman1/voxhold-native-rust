//! Bounded media state for chat pictures, link previews and the image viewer,
//! ported from Go `internal/workspace/{image_cache,history_media,links,
//! image_viewer}.go`.
//!
//! The budgets keep the decoded working set stable across scroll passes: chat
//! thumbnails 16 MiB, link-preview images 4 MiB, at most 128 preview metadata
//! entries, three simultaneous thumbnail reads and a single viewer surface.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::RenderImage;
use voxhold_api::LinkPreview;

use crate::draft::ChatKey;

/// Go `thumbnailBudget`.
pub const THUMB_BUDGET: usize = 16 << 20;
/// Go `previewImageBudget`.
pub const PREVIEW_IMAGE_BUDGET: usize = 4 << 20;
/// Go `thumbnailBucket`.
pub const THUMB_BUCKET: u32 = 16;
/// Go `thumbnailConcurrency`.
pub const THUMB_CONCURRENCY: usize = 3;
/// Go `previewEntries`.
pub const PREVIEW_ENTRIES: usize = 128;
/// Go `imageViewer` limits. The content request enforces the 48 MiB compressed
/// cap; the viewer checks the edge and pixel limits before building a surface.
#[allow(dead_code)]
pub const VIEWER_MAX_BYTES: usize = 48 << 20;
pub const VIEWER_MAX_EDGE: u32 = 8192;
pub const VIEWER_MAX_PIXELS: u64 = 40_000_000;
/// Go `imageViewerArea`.
pub const VIEWER_AREA: f32 = 0.8;

/// Go `bucketSize`: one-pixel resizes reuse the same decoded variant.
pub fn bucket(value: f32) -> u32 {
    let value = value.max(0.0) as u32;
    (((value + THUMB_BUCKET - 1) / THUMB_BUCKET) * THUMB_BUCKET).max(THUMB_BUCKET)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub id: String,
    pub width: u32,
    pub height: u32,
}

impl ThumbKey {
    /// Go `thumbnailSlot`: the requested box is rounded to 16 px.
    pub fn new(id: &str, width: f32, height: f32) -> Self {
        Self {
            id: id.to_string(),
            width: bucket(width),
            height: bucket(height),
        }
    }
}

/// One metadata entry for a link card. `retry_at` gates a repeat request after
/// a failure so a broken link cannot turn into a request per frame.
#[derive(Clone, Debug, Default)]
pub struct PreviewEntry {
    pub meta: LinkPreview,
    pub retry_at: Option<Instant>,
    pub failed: bool,
}

impl PreviewEntry {
    pub fn available(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|until| now >= until)
    }
}

/// Byte-budgeted LRU for decoded pictures. Evicted images are returned so the
/// caller can release the GPU texture with `Window::drop_image`.
pub struct ImageCache<K: std::hash::Hash + Eq + Clone> {
    budget: usize,
    used: usize,
    clock: u64,
    entries: HashMap<K, (Arc<RenderImage>, usize, u64)>,
}

impl<K: std::hash::Hash + Eq + Clone> ImageCache<K> {
    pub fn new(budget: usize) -> Self {
        Self {
            budget: budget.max(1),
            used: 0,
            clock: 0,
            entries: HashMap::new(),
        }
    }

    pub fn get(&mut self, key: &K) -> Option<Arc<RenderImage>> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.entries.get_mut(key)?;
        entry.2 = clock;
        Some(entry.0.clone())
    }

    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    pub fn put(&mut self, key: K, image: Arc<RenderImage>, bytes: usize) -> Vec<Arc<RenderImage>> {
        let mut evicted = Vec::new();
        if bytes == 0 || bytes > self.budget {
            return evicted;
        }
        self.clock += 1;
        if let Some((previous, size, clock)) = self.entries.get_mut(&key) {
            self.used = self.used.saturating_sub(*size).saturating_add(bytes);
            evicted.push(std::mem::replace(previous, image));
            *size = bytes;
            *clock = self.clock;
        } else {
            self.clock += 1;
            self.entries.insert(key, (image, bytes, self.clock));
            self.used += bytes;
        }
        self.trim(&mut evicted);
        evicted
    }

    fn trim(&mut self, evicted: &mut Vec<Arc<RenderImage>>) {
        while self.used > self.budget && !self.entries.is_empty() {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, _, clock))| *clock)
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else { break };
            if let Some((image, size, _)) = self.entries.remove(&key) {
                self.used = self.used.saturating_sub(size);
                evicted.push(image);
            }
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<Arc<RenderImage>> {
        let (image, size, _) = self.entries.remove(key)?;
        self.used = self.used.saturating_sub(size);
        Some(image)
    }

    pub fn clear(&mut self) -> Vec<Arc<RenderImage>> {
        self.used = 0;
        self.entries
            .drain()
            .map(|(_, (image, _, _))| image)
            .collect()
    }

    #[allow(dead_code)] // used by tests and by the soak diagnostics
    pub fn used_bytes(&self) -> usize {
        self.used
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The single image viewer instance, keyed like Go's `(message, attachment,
/// chat, access generation, open id)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerKey {
    pub chat: ChatKey,
    pub access: u64,
    pub message_id: i64,
    pub attachment_id: String,
    pub open_id: u64,
}

pub struct Viewer {
    pub key: ViewerKey,
    pub loading: bool,
    pub error: String,
    pub image: Option<Arc<RenderImage>>,
    /// Decoded original kept to rescale the single surface on resize.
    pub source: Option<Arc<image::DynamicImage>>,
    /// Size in logical pixels of the current GPU surface.
    pub render_box: (u32, u32),
    pub rescaling: bool,
    pub task: Option<gpui::Task<()>>,
}

impl Viewer {
    pub fn new(key: ViewerKey) -> Self {
        Self {
            key,
            loading: true,
            error: String::new(),
            image: None,
            source: None,
            render_box: (0, 0),
            rescaling: false,
            task: None,
        }
    }
}

pub struct Media {
    pub thumbs: ImageCache<ThumbKey>,
    pub thumb_pending: HashSet<ThumbKey>,
    /// `(retry at, already failed once)`; a repeated failure doubles the gate.
    pub thumb_failures: HashMap<ThumbKey, (Instant, bool)>,
    pub previews: HashMap<String, PreviewEntry>,
    pub preview_order: VecDeque<String>,
    pub preview_images: ImageCache<String>,
    pub preview_pending: HashSet<String>,
    pub preview_failures: HashMap<String, (Instant, bool)>,
    pub viewer: Option<Viewer>,
    pub viewer_seq: u64,
}

impl Default for Media {
    fn default() -> Self {
        Self {
            thumbs: ImageCache::new(THUMB_BUDGET),
            thumb_pending: HashSet::new(),
            thumb_failures: HashMap::new(),
            previews: HashMap::new(),
            preview_order: VecDeque::new(),
            preview_images: ImageCache::new(PREVIEW_IMAGE_BUDGET),
            preview_pending: HashSet::new(),
            preview_failures: HashMap::new(),
            viewer: None,
            viewer_seq: 0,
        }
    }
}

impl Media {
    /// Drop every private picture when the access scope changes. Returns the
    /// GPU images the caller must release.
    pub fn clear(&mut self) -> Vec<Arc<RenderImage>> {
        let mut images = self.thumbs.clear();
        images.extend(self.preview_images.clear());
        if let Some(viewer) = self.viewer.take() {
            if let Some(image) = viewer.image {
                images.push(image);
            }
        }
        self.thumb_pending.clear();
        self.thumb_failures.clear();
        self.previews.clear();
        self.preview_order.clear();
        self.preview_pending.clear();
        self.preview_failures.clear();
        images
    }

    /// Go `thumbnailRetryDelay`: 15 s after `429`, 5 s otherwise, doubled after
    /// a second failure.
    pub fn thumb_retry(&mut self, key: &ThumbKey, status: Option<u16>) {
        let doubled = self
            .thumb_failures
            .get(key)
            .is_some_and(|(_, doubled)| *doubled);
        let delay = retry_delay(status, doubled);
        self.thumb_failures
            .insert(key.clone(), (Instant::now() + delay, true));
    }

    pub fn thumb_ready(&mut self, key: &ThumbKey) -> bool {
        self.thumbs.contains(key)
    }

    pub fn thumb_blocked(&self, key: &ThumbKey, now: Instant) -> bool {
        self.thumb_failures
            .get(key)
            .is_some_and(|(until, _)| now < *until)
    }

    pub fn thumb_succeeded(&mut self, key: &ThumbKey) {
        self.thumb_failures.remove(key);
    }

    /// Go `loadPreview`: an entry inside its gate is reused, otherwise the
    /// caller starts one request. A failed entry without a gate is retried.
    pub fn preview_entry(&mut self, href: &str) -> Option<PreviewEntry> {
        let now = Instant::now();
        match self.previews.get(href) {
            Some(entry) if entry.available(now) => Some(entry.clone()),
            _ => None,
        }
    }

    pub fn preview_blocked(&self, href: &str, now: Instant) -> bool {
        self.preview_failures
            .get(href)
            .is_some_and(|(until, _)| now < *until)
    }

    /// Store metadata, bounded to 128 entries with FIFO eviction like Go
    /// `previewOrder`. Returns an evicted preview picture for GPU release.
    pub fn put_preview(&mut self, href: &str, entry: PreviewEntry) -> Option<Arc<RenderImage>> {
        let mut released = None;
        if !self.previews.contains_key(href) {
            if self.previews.len() >= PREVIEW_ENTRIES {
                if let Some(oldest) = self.preview_order.pop_front() {
                    self.previews.remove(&oldest);
                    released = self.preview_images.remove(&oldest);
                }
            }
            self.preview_order.push_back(href.to_string());
        }
        match &entry.retry_at {
            Some(until) => {
                let doubled = self
                    .preview_failures
                    .get(href)
                    .is_some_and(|(_, doubled)| *doubled);
                let delay = until.saturating_duration_since(Instant::now());
                self.preview_failures.insert(
                    href.to_string(),
                    (Instant::now() + delay, doubled || entry.failed),
                );
            }
            None => {
                self.preview_failures.remove(href);
            }
        }
        self.previews.insert(href.to_string(), entry);
        released
    }

    /// Go `previewLabels`: site, title, description and host, with the host as
    /// the fallback for an empty site/title.
    pub fn preview_labels(href: &str, preview: &LinkPreview) -> [String; 4] {
        let host = url::Url::parse(href)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        let site = if preview.site_name.is_empty() {
            host.clone()
        } else {
            preview.site_name.clone()
        };
        let title = if preview.title.is_empty() {
            host.clone()
        } else {
            preview.title.clone()
        };
        [site, title, preview.description.clone(), host]
    }
}

/// Go `thumbnailRetryDelay`.
pub fn retry_delay(status: Option<u16>, already_failed: bool) -> Duration {
    let base = if status == Some(429) {
        Duration::from_secs(15)
    } else {
        Duration::from_secs(5)
    };
    if already_failed {
        base * 2
    } else {
        base
    }
}

/// Go `loadPreview` retry gate: a `400` means the URL will never preview.
pub fn preview_retry(status: Option<u16>, already_failed: bool) -> Option<Duration> {
    if status == Some(400) {
        return None;
    }
    let base = Duration::from_secs(5);
    Some(if already_failed { base * 2 } else { base })
}

/// Go viewer area: 80% of the window, never below 200×160.
pub fn viewer_area(viewport: (f32, f32)) -> (f32, f32) {
    (
        (viewport.0 * VIEWER_AREA).max(200.),
        (viewport.1 * VIEWER_AREA).max(160.),
    )
}

/// Scale the source down into the area, keeping the aspect ratio and never
/// enlarging it (Go `imageViewer.Draw`).
pub fn fit_size(source: (u32, u32), area: (f32, f32)) -> (u32, u32) {
    if source.0 == 0 || source.1 == 0 {
        return (1, 1);
    }
    let ratio = (area.0 / source.0 as f32)
        .min(area.1 / source.1 as f32)
        .min(1.0);
    (
        ((source.0 as f32 * ratio) as u32).max(1),
        ((source.1 as f32 * ratio) as u32).max(1),
    )
}

/// Bytes of an RGBA surface; exact for our decoded thumbnails.
#[allow(dead_code)] // referenced by the budget tests and the memory soak
pub fn rgba_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32) -> Arc<RenderImage> {
        let frame = image::Frame::new(image::RgbaImage::new(width, height));
        Arc::new(RenderImage::new(smallvec::smallvec![frame]))
    }

    #[test]
    fn thumbnail_keys_round_to_sixteen_pixels() {
        assert_eq!(bucket(1.0), 16);
        assert_eq!(bucket(16.0), 16);
        assert_eq!(bucket(17.0), 32);
        let first = ThumbKey::new("a", 100.0, 60.0);
        let second = ThumbKey::new("a", 101.0, 61.0);
        assert_eq!((first.width, first.height), (112, 64));
        assert_eq!(first, second, "a one-pixel resize reuses the variant");
    }

    #[test]
    fn retry_gates_match_go() {
        assert_eq!(retry_delay(Some(429), false), Duration::from_secs(15));
        assert_eq!(retry_delay(Some(500), false), Duration::from_secs(5));
        assert_eq!(retry_delay(None, true), Duration::from_secs(10));
        assert_eq!(retry_delay(Some(429), true), Duration::from_secs(30));
        assert_eq!(preview_retry(Some(400), false), None);
        assert_eq!(
            preview_retry(Some(404), false),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            preview_retry(Some(500), true),
            Some(Duration::from_secs(10))
        );
    }

    #[test]
    fn lru_evicts_by_byte_budget_and_returns_gpu_images() {
        let mut cache: ImageCache<i64> = ImageCache::new(1000);
        let first = image(10, 10);
        assert!(cache.put(1, first, 400).is_empty());
        assert!(cache.put(2, image(10, 10), 400).is_empty());
        assert_eq!(cache.used_bytes(), 800);
        // Touch 1 so 2 becomes the oldest.
        assert!(cache.get(&1).is_some());
        let evicted = cache.put(3, image(10, 10), 400);
        assert_eq!(evicted.len(), 1, "one picture leaves the budget");
        assert_eq!(cache.used_bytes(), 800);
        assert!(cache.get(&2).is_none());
        assert!(cache.get(&1).is_some());
        let released = cache.clear();
        assert_eq!(released.len(), 2);
        assert_eq!(cache.used_bytes(), 0);
    }

    #[test]
    fn one_picture_larger_than_the_budget_is_not_cached() {
        let mut cache: ImageCache<i64> = ImageCache::new(100);
        assert!(cache.put(1, image(10, 10), 400).is_empty());
        assert!(cache.get(&1).is_none());
    }

    #[test]
    fn preview_metadata_is_bounded() {
        let mut media = Media::default();
        for index in 0..(PREVIEW_ENTRIES + 5) {
            media.put_preview(
                &format!("https://site{index}.example.com/"),
                PreviewEntry::default(),
            );
        }
        assert_eq!(media.previews.len(), PREVIEW_ENTRIES);
        assert!(media.previews.contains_key("https://site5.example.com/"));
        assert!(!media.previews.contains_key("https://site0.example.com/"));
    }

    #[test]
    fn failing_preview_is_gated() {
        let mut media = Media::default();
        let entry = PreviewEntry {
            retry_at: Some(Instant::now() + Duration::from_secs(5)),
            failed: true,
            ..Default::default()
        };
        media.put_preview("https://broken.example.com/", entry);
        assert!(media.preview_blocked("https://broken.example.com/", Instant::now()));
        assert!(media.preview_entry("https://broken.example.com/").is_none());
    }

    #[test]
    fn viewer_geometry_uses_eighty_percent_and_keeps_the_ratio() {
        assert_eq!(viewer_area((1280., 900.)), (1024., 720.));
        assert_eq!(viewer_area((100., 100.)), (200., 160.));
        assert_eq!(fit_size((1000, 500), (1024., 720.)), (1000, 500));
        assert_eq!(fit_size((4000, 2000), (1024., 720.)), (1024, 512));
        assert_eq!(fit_size((100, 1000), (1024., 720.)), (72, 720));
        assert_eq!(fit_size((0, 0), (10., 10.)), (1, 1));
    }

    #[test]
    fn preview_labels_fall_back_to_the_host() {
        let preview = LinkPreview {
            site_name: String::new(),
            title: String::new(),
            description: "Описание".into(),
            ..Default::default()
        };
        let labels = Media::preview_labels("https://example.com/a", &preview);
        assert_eq!(labels[0], "example.com");
        assert_eq!(labels[1], "example.com");
        assert_eq!(labels[2], "Описание");
        assert_eq!(labels[3], "example.com");
    }
}
