//! Bounded picker preview cache and background thumbnail generation.
//!
//! Kept apart from the dialog painter so the session epoch, the memory bound,
//! the one-batch rule and the one-source-at-a-time rule are reviewed together.

use super::selection::identity_kind;
use super::*;

impl ScreenShareUi {
    /// The stable card/focus identity as a short debug suffix.
    pub(super) fn source_kind_name(id: SourceId) -> &'static str {
        match id.kind {
            SourceKind::Monitor => "monitor",
            SourceKind::Window => "window",
        }
    }

    /// Start a new preview session: the epoch moves, so every capture still in
    /// flight is ignored, and no picture of the previous picker survives. The
    /// batch guard is released here as well: the epoch check in the old batch
    /// stops it from touching a newer session's flag. Cached pictures are
    /// removed from the sprite atlas, never just forgotten.
    pub(in crate::voice_panel) fn clear_previews(&mut self, window: &mut Window) {
        self.preview_epoch = self.preview_epoch.wrapping_add(1);
        let images: Vec<Arc<RenderImage>> = self
            .previews
            .drain()
            .map(|(_, thumb)| thumb.image)
            .collect();
        self.release_images(window, images);
        self.preview_failures.clear();
        self.preview_pending.clear();
        self.preview_unfit.clear();
        self.preview_batching = false;
        self.preview_bytes = 0;
        self.preview_order.clear();
        self.preview_rejected = 0;
        self.preview_cards = 0;
        // The next picker starts at the top of its own list: a scroll offset
        // or visibility list of the previous dialog must not choose its
        // captures.
        self.visible_previews.clear();
        self.visible_scratch.clear();
        self.visible_scroll = 0.0;
        self.scroll = ScrollHandle::new();
    }

    /// Release one GPUI image from the sprite atlas. Every replaced or removed
    /// preview passes through here, so the test-only counter proves that the
    /// texture was really released and not merely dropped from a collection.
    pub(super) fn release_image(&mut self, window: &mut Window, image: Arc<RenderImage>) {
        let _ = window.drop_image(image);
        #[cfg(test)]
        {
            // Read at the exact moment of the drop, so a test observes the
            // slot the renderer would see, not just the order of two calls.
            self.last_release_slot_image = self.preview.as_ref().map(|image| image.id.0 as u64);
            self.released_images = self.released_images.saturating_add(1);
        }
    }

    fn release_images(
        &mut self,
        window: &mut Window,
        images: impl IntoIterator<Item = Arc<RenderImage>>,
    ) {
        for image in images {
            self.release_image(window, image);
        }
    }

    /// Release every picture this picker owns: the on-air preview card and all
    /// cached thumbnails. Used when the whole share state is reset.
    pub(super) fn release_all_images(&mut self, window: &mut Window) {
        let card = self.preview.take();
        let previews: Vec<Arc<RenderImage>> = self
            .previews
            .drain()
            .map(|(_, thumb)| thumb.image)
            .collect();
        if let Some(card) = card {
            self.release_image(window, card);
        }
        self.release_images(window, previews);
        self.preview_bytes = 0;
        self.preview_order.clear();
    }

    /// Replace the on-air card picture, releasing the previous one only after
    /// the slot already points at the new image.
    pub(in crate::voice_panel) fn set_preview(
        &mut self,
        window: &mut Window,
        image: Arc<RenderImage>,
    ) {
        let previous = self.preview.replace(image);
        if let Some(previous) = previous {
            self.release_image(window, previous);
        }
    }

    /// Drop the on-air card picture, if any.
    pub(in crate::voice_panel) fn clear_preview(&mut self, window: &mut Window) {
        if let Some(previous) = self.preview.take() {
            self.release_image(window, previous);
        }
    }

    /// Claim the next source that still needs a picture, marking it pending.
    /// `None` means the batch has no work left for this epoch and category.
    ///
    /// Only the painted rows of this category are claimed; the snapshot order
    /// is used only before the first visibility list exists, and then only for
    /// the first [`BOOTSTRAP_PREVIEW_CARDS`] cards. Room is checked before any
    /// capture starts: off-screen pictures are released first and, when
    /// nothing can be evicted, no capturer is started at all. Otherwise a full
    /// cache would answer every visible card with a memory note instead of a
    /// picture.
    ///
    /// Called before every capture, so a row that became visible while the
    /// batch waits on a capture is picked up without starting a second batch.
    pub(super) fn take_next_preview_source(
        &mut self,
        window: &mut Window,
        epoch: u64,
        kind: SourceKind,
    ) -> Option<ScreenSource> {
        if !self.open || self.restart || self.preview_epoch != epoch || self.kind != kind {
            return None;
        }
        let source = self.next_preview_source(kind)?;
        if !self.reserve_preview_room(window, &source) {
            return None;
        }
        self.preview_pending.insert(source.identity()?);
        Some(source)
    }

    /// The next card in claim order: the painted visible rows first, else the
    /// bootstrap cards of a category the dialog has not painted yet.
    fn next_preview_source(&self, kind: SourceKind) -> Option<ScreenSource> {
        if self.visible_kind == kind && !self.visible_previews.is_empty() {
            return self.visible_preview_source(kind);
        }
        self.snapshot(kind)
            .iter()
            .take(BOOTSTRAP_PREVIEW_CARDS)
            .find(|source| self.preview_source_ready(source))
            .cloned()
    }

    /// One visible, uncached source, in the order the grid paints it. A
    /// duplicated identity is ambiguous and can never be accepted by
    /// `finish_preview`, so it is skipped instead of captured.
    fn visible_preview_source(&self, kind: SourceKind) -> Option<ScreenSource> {
        if self.visible_kind != kind {
            return None;
        }
        for identity in &self.visible_previews {
            let mut matches = self
                .snapshot(kind)
                .iter()
                .filter(|source| source.identity().as_ref() == Some(identity));
            let Some(source) = matches.next() else {
                continue;
            };
            if matches.next().is_some() {
                continue;
            }
            if self.preview_source_ready(source) {
                return Some(source.clone());
            }
        }
        None
    }

    /// Whether one capture of this surface can still be claimed: not cached,
    /// not failed, not refused for lack of room, not in flight and
    /// unambiguous in the current snapshot.
    fn preview_source_ready(&self, source: &ScreenSource) -> bool {
        let Some(identity) = source.identity() else {
            return false;
        };
        !self.previews.contains_key(&identity)
            && !self.preview_failures.contains_key(&identity)
            && !self.preview_unfit.contains(&identity)
            && !self.preview_pending.contains(&identity)
            && self.source_for_identity(&identity).is_some()
    }

    /// Whether the cache can keep a picture of `bytes` without passing the
    /// count or byte bound. A cached identity only replaces its own bytes.
    fn preview_has_room_for(&self, identity: &SourceIdentity, bytes: usize) -> bool {
        let existing = self
            .previews
            .get(identity)
            .map(|thumb| thumb.bytes)
            .unwrap_or(0);
        let grows_count = usize::from(existing == 0);
        self.previews.len() + grows_count <= MAX_SCREEN_PREVIEWS
            && self.preview_bytes.saturating_sub(existing) + bytes <= MAX_SCREEN_PREVIEW_BYTES
    }

    /// Free cached pictures until one frame of `bytes` fits again. The oldest
    /// picture that is neither visible in this category nor in flight goes
    /// first. Returns false when nothing can be evicted, so the caller never
    /// starts a capture whose result could not be kept.
    fn evict_previews(
        &mut self,
        window: &mut Window,
        identity: &SourceIdentity,
        bytes: usize,
    ) -> bool {
        while !self.preview_has_room_for(identity, bytes) {
            let kind = identity_kind(identity);
            let Some(oldest) = self.oldest_evictable_preview(kind) else {
                return false;
            };
            if let Some(thumb) = self.previews.remove(&oldest) {
                self.preview_bytes = self.preview_bytes.saturating_sub(thumb.bytes);
                self.release_image(window, thumb.image);
            }
        }
        true
    }

    /// The oldest cached identity an eviction may take: not in flight and,
    /// once the rows of this category were painted, not one of its visible
    /// cards. A visibility list of another category protects nothing here, so
    /// a tab switch can free the pictures of the hidden one.
    fn oldest_evictable_preview(&mut self, kind: SourceKind) -> Option<SourceIdentity> {
        let mut index = 0;
        while index < self.preview_order.len() {
            let identity = self.preview_order[index].clone();
            if !self.previews.contains_key(&identity) {
                self.preview_order.remove(index);
                continue;
            }
            let visible = self.visible_kind == kind && self.visible_previews.contains(&identity);
            if visible || self.preview_pending.contains(&identity) {
                index += 1;
                continue;
            }
            self.preview_order.remove(index);
            return Some(identity);
        }
        None
    }

    /// Make room for the estimated frame of `source`, evicting off-screen
    /// pictures if needed. False means no capture may start.
    fn reserve_preview_room(&mut self, window: &mut Window, source: &ScreenSource) -> bool {
        let Some(identity) = source.identity() else {
            return false;
        };
        let estimate = self.preview_frame_estimate(source);
        self.evict_previews(window, &identity, estimate)
    }

    /// Conservative BGRA size of one card picture: the fixed capture width and
    /// the surface's own aspect, never the painted card width. A surface with
    /// no reported size is treated as a square at the capture width, the worst
    /// reasonable frame that still fits a card.
    fn preview_frame_estimate(&self, source: &ScreenSource) -> usize {
        let (width, height) = preview_frame_size(source);
        width as usize * height as usize * 4
    }

    /// Recompute the visible rows from the scroll handle of the last painted
    /// frame. The identity list is swapped into place through a reused buffer,
    /// so a repaint does not allocate a new `Vec`; every identity is still
    /// cloned because a monitor identity owns its device string. Returns
    /// whether the rows or the scroll changed since the previous frame; the
    /// dialog then asks for previews once, and the batch guard keeps that from
    /// starting a second capture beside a running one.
    pub(super) fn sync_visible_previews(
        &mut self,
        sources: &[ScreenSource],
        columns: usize,
    ) -> bool {
        // A scroll offset is zero at the top and negative further down.
        let scroll_top = (-f32::from(self.scroll.offset().y)).max(0.0);
        let scrolled = (scroll_top - self.visible_scroll).abs() > 0.5;
        self.visible_scroll = scroll_top;
        self.visible_scratch.clear();
        if self.restart || columns == 0 || sources.is_empty() {
            let changed = self.visible_kind != self.kind || !self.visible_previews.is_empty();
            self.visible_previews.clear();
            self.visible_kind = self.kind;
            return scrolled || changed;
        }
        let rows = sources.len().div_ceil(columns);
        let Some((start, end)) =
            visible_row_range(scroll_top, SOURCE_LIST_HEIGHT, SOURCE_ROW_HEIGHT, rows)
        else {
            let changed = self.visible_kind != self.kind || !self.visible_previews.is_empty();
            self.visible_previews.clear();
            self.visible_kind = self.kind;
            return scrolled || changed;
        };
        for row in start..end {
            for column in 0..columns {
                if let Some(identity) = sources
                    .get(row * columns + column)
                    .and_then(ScreenSource::identity)
                {
                    self.visible_scratch.push(identity);
                }
            }
        }
        let changed =
            self.visible_kind != self.kind || self.visible_scratch != self.visible_previews;
        if changed {
            self.visible_kind = self.kind;
            std::mem::swap(&mut self.visible_previews, &mut self.visible_scratch);
        }
        self.visible_scratch.clear();
        scrolled || changed
    }

    /// Release the batch guard at the end of a batch. A stale batch (its epoch
    /// was superseded by a closed picker, a stop or a newer session) must not
    /// clear a flag that now belongs to a newer batch. Returns whether the
    /// caller owns the guard and should re-request pending work.
    pub(crate) fn end_preview_batch(&mut self, epoch: u64) -> bool {
        if self.preview_epoch != epoch {
            return false;
        }
        self.preview_batching = false;
        true
    }

    /// Add one picture to the bounded cache for a test seam. A picture over the
    /// width, count or byte bound is counted as rejected and never queued. The
    /// background path reserves room first, so a `Full` answer here is a caller
    /// mistake, not a card failure.
    #[cfg(test)]
    pub(crate) fn insert_preview(
        &mut self,
        window: &mut Window,
        identity: SourceIdentity,
        frame: PreviewFrame,
    ) -> bool {
        let stored = self.store_preview(window, identity, frame);
        if !stored {
            self.preview_rejected = self.preview_rejected.saturating_add(1);
        }
        stored
    }

    /// Put an already validated picture into the cache, replacing the previous
    /// one of the same identity. The caller has reserved room for `bytes`.
    fn store_preview(
        &mut self,
        window: &mut Window,
        identity: SourceIdentity,
        frame: PreviewFrame,
    ) -> bool {
        let Some(bytes) = valid_preview_bytes(&frame) else {
            return false;
        };
        if !self.preview_has_room_for(&identity, bytes) {
            return false;
        }
        let Some(image) = preview_render_image(frame) else {
            return false;
        };
        let existing = self
            .previews
            .get(&identity)
            .map(|thumb| thumb.bytes)
            .unwrap_or(0);
        self.preview_bytes = self.preview_bytes.saturating_sub(existing) + bytes;
        let previous = self
            .previews
            .insert(identity.clone(), PreviewThumb { image, bytes });
        if previous.is_none() {
            self.preview_order.push_back(identity.clone());
        }
        if let Some(previous) = previous {
            self.release_image(window, previous.image);
        }
        self.preview_failures.remove(&identity);
        self.preview_pending.remove(&identity);
        self.preview_unfit.remove(&identity);
        true
    }

    pub(crate) fn insert_preview_failure(&mut self, identity: SourceIdentity, message: String) {
        self.preview_pending.remove(&identity);
        self.preview_failures.insert(identity, message);
    }

    /// Apply a background result. A different epoch (closed/old picker), a
    /// non-pending id (cancelled batch) or an id absent from the current
    /// snapshot is ignored and never inserted.
    ///
    /// A cache that is still full after evicting everything it may is not a
    /// failure: the picture is dropped, nothing is painted on the card and the
    /// surface is retried when a later snapshot or tab describes it again.
    pub(crate) fn finish_preview(
        &mut self,
        window: &mut Window,
        epoch: u64,
        identity: SourceIdentity,
        result: Result<PreviewFrame, ThumbnailError>,
    ) -> bool {
        if self.preview_epoch != epoch {
            return false;
        }
        if !self.preview_pending.remove(&identity) {
            return false;
        }
        // A result of another category can only be a late frame of a tab the
        // user already left: the current batch captures `kind` only.
        if identity_kind(&identity) != self.kind {
            return false;
        }
        // The result belongs to the surface, not to the number its capture
        // started with: after a reorder the same id can name another display.
        if self.source_for_identity(&identity).is_none() {
            return false;
        }
        match result {
            Ok(frame) => {
                let Some(bytes) = valid_preview_bytes(&frame) else {
                    // The card could never paint this frame: an honest note.
                    self.preview_rejected = self.preview_rejected.saturating_add(1);
                    self.insert_preview_failure(identity, PREVIEW_UNREADABLE_REASON.to_owned());
                    return true;
                };
                // The real frame may be larger than the reservation (a window
                // resized between the enumeration and the capture): release
                // the pictures an eviction may take for its exact size.
                if !self.preview_has_room_for(&identity, bytes) {
                    self.evict_previews(window, &identity, bytes);
                }
                if !self.store_preview(window, identity.clone(), frame) {
                    self.preview_rejected = self.preview_rejected.saturating_add(1);
                    self.preview_unfit.insert(identity);
                }
            }
            Err(error) => self.insert_preview_failure(identity, error.message().to_owned()),
        }
        true
    }

    /// Drop cached previews/failures/pending identities that the newest
    /// snapshot of a kind no longer offers. A duplicated identity is
    /// ambiguous, so it is dropped as well; the next answer may describe it
    /// unambiguously again.
    pub(super) fn retain_preview_snapshot(
        &mut self,
        window: &mut Window,
        kind: SourceKind,
        sources: &[ScreenSource],
    ) {
        let present = |identity: &SourceIdentity| {
            identity_kind(identity) == kind
                && sources
                    .iter()
                    .filter(|source| source.identity().as_ref() == Some(identity))
                    .count()
                    == 1
        };
        let gone: Vec<SourceIdentity> = self
            .previews
            .keys()
            .filter(|identity| identity_kind(identity) == kind && !present(identity))
            .cloned()
            .collect();
        let mut removed = Vec::new();
        for identity in gone {
            if let Some(thumb) = self.previews.remove(&identity) {
                self.preview_bytes = self.preview_bytes.saturating_sub(thumb.bytes);
                removed.push(thumb.image);
            }
        }
        self.release_images(window, removed);
        self.preview_order
            .retain(|identity| self.previews.contains_key(identity));
        self.preview_failures
            .retain(|identity, _| identity_kind(identity) != kind || present(identity));
        self.preview_pending
            .retain(|identity| identity_kind(identity) != kind || present(identity));
        // A fresh answer describes the surface again, so a frame that did not
        // fit before gets another chance.
        self.preview_unfit
            .retain(|identity| identity_kind(identity) != kind);
    }
}

/// BGRA size of a frame the picker may paint, or `None` for a picture that can
/// never reach a card (zero size, over the capture width, malformed buffer).
fn valid_preview_bytes(frame: &PreviewFrame) -> Option<usize> {
    if frame.width == 0
        || frame.height == 0
        || frame.width > SCREEN_THUMBNAIL_WIDTH
        || frame.bgra.len() != frame.width as usize * frame.height as usize * 4
    {
        return None;
    }
    Some(frame.bgra.len())
}

/// Cards the first batch may claim before the dialog has painted a visibility
/// list of its category: two rows of the default two-column grid. Everything
/// past them waits for a real scroll instead of walking the whole tab.
pub(super) const BOOTSTRAP_PREVIEW_CARDS: usize = 4;
/// A frame the media layer returned but a card could never paint: an honest
/// note, unlike a full cache, which is not a failure of the surface.
const PREVIEW_UNREADABLE_REASON: &str = "Предпросмотр источника не получен.";
/// Largest side the media layer keeps for a thumbnail, mirroring
/// `screen_capture::MAX_CAPTURE_SIDE`. It bounds the height of a portrait
/// surface captured at the fixed width.
const PREVIEW_MAX_SIDE: u32 = 4096;

/// Size of the BGRA picture the media layer returns for `source`: at most
/// `SCREEN_THUMBNAIL_WIDTH` wide, the surface's own aspect and even sides, the
/// same rules as `screen_capture::preview_from_bgra`. The cache gate estimates
/// the real frame with this, never the painted card.
fn preview_frame_size(source: &ScreenSource) -> (u32, u32) {
    let (width, height) = (source.width, source.height);
    if width == 0 || height == 0 {
        // No reported size: a square at the capture width is the worst
        // reasonable frame, not an unbounded portrait guess.
        return (SCREEN_THUMBNAIL_WIDTH, SCREEN_THUMBNAIL_WIDTH);
    }
    if width <= SCREEN_THUMBNAIL_WIDTH && height <= PREVIEW_MAX_SIDE {
        return ((width & !1).max(2), (height & !1).max(2));
    }
    if u64::from(SCREEN_THUMBNAIL_WIDTH) * u64::from(height)
        <= u64::from(PREVIEW_MAX_SIDE) * u64::from(width)
    {
        let scaled =
            (u64::from(SCREEN_THUMBNAIL_WIDTH) * u64::from(height) / u64::from(width)) as u32;
        (SCREEN_THUMBNAIL_WIDTH, (scaled & !1).max(2))
    } else {
        let scaled = (u64::from(PREVIEW_MAX_SIDE) * u64::from(width) / u64::from(height)) as u32;
        ((scaled & !1).max(2), PREVIEW_MAX_SIDE)
    }
}

/// Wrap a media BGRA picture for GPUI. `RenderImage` keeps BGRA (the Windows
/// atlas uploads it as `B8G8R8A8` verbatim), so the bytes are passed through:
/// a channel swap here would paint every picker card in inverted colours.
fn preview_render_image(frame: PreviewFrame) -> Option<Arc<RenderImage>> {
    let (width, height) = (frame.width, frame.height);
    let buffer = image::RgbaImage::from_raw(width, height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(smallvec::smallvec![
        image::Frame::new(buffer)
    ])))
}

/// Half-open row range `[start, end)` that a viewport covers at `scroll_top`,
/// widened by one row above and below so a small scroll already has its
/// pictures. `None` means the layout is unknown (no rows, no viewport, a zero
/// row height or a non-finite value): the caller then falls back to the small
/// bootstrap set instead of guessing.
///
/// `scroll_top` is the positive distance from the top of the content; the
/// GPUI scroll handle stores the same distance negated.
pub(super) fn visible_row_range(
    scroll_top: f32,
    viewport_height: f32,
    row_height: f32,
    row_count: usize,
) -> Option<(usize, usize)> {
    if row_count == 0 || !(viewport_height > 0.0) || !(row_height > 0.0) {
        return None;
    }
    if !scroll_top.is_finite() || !viewport_height.is_finite() || !row_height.is_finite() {
        return None;
    }
    let top = scroll_top.max(0.0) / row_height;
    let top = top.floor() as usize;
    if top >= row_count {
        // Overscroll: the last row is still the one the user looks at.
        return Some((row_count - 1, row_count));
    }
    let bottom = ((scroll_top.max(0.0) + viewport_height) / row_height).ceil() as usize;
    let bottom = bottom.clamp(top + 1, row_count);
    let start = top.saturating_sub(1);
    let end = (bottom + 1).min(row_count);
    Some((start, end))
}

/// Test seams of the visible-area picker. They expose only what a UI test
/// needs to drive the real render path: the identity list the last frame
/// computed. The scroll itself is a wheel event on `#share-sources`.
#[cfg(test)]
impl VoicePanel {
    /// Identities the last painted frame treats as visible, in visual order.
    pub(crate) fn test_visible_screen_previews(&self) -> Vec<SourceId> {
        self.screen
            .visible_previews
            .iter()
            .filter_map(|identity| {
                self.screen
                    .source_for_identity(identity)
                    .map(ScreenSource::source_id)
            })
            .collect()
    }
}

impl VoicePanel {
    /// Start background thumbnail generation for the visible category.
    ///
    /// Cached and pending ids are skipped. At most one batch, and inside it at
    /// most one capture, runs at a time. A request made while the batch is
    /// active only returns: the running batch re-reads the visible rows before
    /// every capture and re-requests pending work when it ends, so a source
    /// that appeared mid-batch (or scrolled into view) is still captured, just
    /// not concurrently. Only the painted rows plus their one-row margin are
    /// captured; the rest of the tab waits for a scroll. Before every source
    /// the cache reserves room, evicting off-screen pictures, and a source with
    /// no room is not captured at all.
    /// Before every source the batch re-checks that the same picker, epoch and
    /// category are still current, so a closed dialog or a tab switch never
    /// starts another capture. The capture itself is bounded by the media layer
    /// and runs on the background executor, never on the UI thread or the voice
    /// worker.
    pub(crate) fn request_screen_previews(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.screen.open || self.screen.restart {
            return;
        }
        // The suite never reaches the real desktop capturer: a test must opt in
        // with an injected provider.
        #[cfg(test)]
        let provider = self.test_preview_provider.clone();
        #[cfg(test)]
        if provider.is_none() {
            return;
        }
        if self.screen.preview_batching {
            return;
        }
        let kind = self.screen.kind;
        let epoch = self.screen.preview_epoch;
        let Some(first) = self.screen.take_next_preview_source(window, epoch, kind) else {
            return;
        };
        self.screen.preview_batching = true;
        cx.notify();
        // `spawn_in` keeps the window available for the whole batch, so both a
        // replaced thumbnail and a re-request at the end can release textures
        // through the window that actually owns the sprite atlas.
        cx.spawn_in(window, async move |this, cx| {
            let mut next = Some(first);
            while let Some(source) = next {
                let Some(identity) = source.identity() else {
                    break;
                };
                #[cfg(test)]
                let provider = provider.clone();
                let keep = this
                    .update_in(cx, |panel, _, _| {
                        panel.screen.open
                            && !panel.screen.restart
                            && panel.screen.preview_epoch == epoch
                            && panel.screen.kind == kind
                            && panel.screen.preview_pending.contains(&identity)
                            && panel.screen.source_for_identity(&identity).is_some()
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
                let task = cx.background_spawn(async move {
                    #[cfg(test)]
                    if let Some(provider) = provider {
                        return provider(&source);
                    }
                    screen_capture::capture_thumbnail(&source, SCREEN_THUMBNAIL_WIDTH)
                });
                let result = task.await;
                let alive = this
                    .update_in(cx, |panel, _, _| {
                        panel.screen.open
                            && !panel.screen.restart
                            && panel.screen.preview_epoch == epoch
                            && panel.screen.kind == kind
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
                let _ = this.update_in(cx, |panel, window, cx| {
                    // A vanished id is ignored but does not stop the rest of
                    // the batch; only an epoch/category change above does.
                    if panel.screen.finish_preview(window, epoch, identity, result) {
                        cx.notify();
                    }
                });
                // The next claim re-checks the session, so a row added while
                // this capture ran joins the same batch.
                next = this
                    .update_in(cx, |panel, window, _| {
                        panel.screen.take_next_preview_source(window, epoch, kind)
                    })
                    .ok()
                    .flatten();
            }
            let applied = this.update_in(cx, |panel, window, cx| {
                // Only this batch's epoch may release the flag: a newer picker
                // or dialog already reset it, and a late end must not clear a
                // batch that session started.
                if !panel.screen.end_preview_batch(epoch) {
                    return;
                }
                // Rows added after the last claim, a refresh or a tab switch
                // that was answered by the guard: all of them are picked up
                // here, on the current category.
                if panel.screen.open && !panel.screen.restart {
                    panel.request_screen_previews(window, cx);
                }
            });
            if applied.is_err() {
                // The window may close between the last capture and this end
                // of the batch. The entity can still be updated without one,
                // so release the guard and the pending ids there: a stuck
                // `preview_batching` would keep this panel from ever capturing
                // again. No picture is touched, so no window is needed.
                let _ = this.update(cx, |panel, _| {
                    if panel.screen.end_preview_batch(epoch) {
                        panel.screen.preview_pending.clear();
                    }
                });
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes GPUI actually receives for one cached card. `RenderImage`
    /// exposes its frame buffer as-is, so this is the atlas input.
    fn cached_bytes(color: [u8; 4]) -> Vec<u8> {
        let frame = PreviewFrame {
            width: 2,
            height: 1,
            bgra: [color, color].concat(),
        };
        let image = preview_render_image(frame).expect("the picture is valid");
        image.as_bytes(0).expect("one frame").to_vec()
    }

    /// A red plate must stay red and a blue plate blue. The media layer hands
    /// over BGRA and GPUI uploads it verbatim, so any channel swap in between
    /// is visible here as an inverted pair.
    #[test]
    fn picker_previews_keep_the_bgra_channel_order() {
        let red = [0x00, 0x00, 0xff, 0xff];
        let blue = [0xff, 0x00, 0x00, 0xff];
        assert_eq!(
            cached_bytes(red),
            [red, red].concat(),
            "pure red must not reach the card as blue"
        );
        assert_eq!(
            cached_bytes(blue),
            [blue, blue].concat(),
            "pure blue must not reach the card as red"
        );
    }

    /// The viewport covers whole and partial rows plus one row of margin on
    /// each side. The picker must never claim a row's picture just because it
    /// is the next one in the snapshot.
    #[test]
    fn the_visible_row_range_covers_the_viewport_and_one_margin_row() {
        // 236 px viewport, 132 px rows: rows 0 and 1 are on screen (row 1 is
        // cut), row 2 is the lower margin.
        assert_eq!(
            visible_row_range(0.0, SOURCE_LIST_HEIGHT, SOURCE_ROW_HEIGHT, 12),
            Some((0, 3))
        );
        // Two rows down: one margin above, rows 2 and 3 on screen, one below.
        assert_eq!(
            visible_row_range(
                2.0 * SOURCE_ROW_HEIGHT,
                SOURCE_LIST_HEIGHT,
                SOURCE_ROW_HEIGHT,
                12
            ),
            Some((1, 5))
        );
        // A single row is both the viewport and its own margin.
        assert_eq!(
            visible_row_range(0.0, SOURCE_LIST_HEIGHT, SOURCE_ROW_HEIGHT, 1),
            Some((0, 1))
        );
        // The end of the list stops at the last row instead of running over.
        let rows = 12_u32;
        let content = (rows - 1) as f32 * SOURCE_ROW_HEIGHT + SOURCE_CARD_HEIGHT;
        let max_offset = content - SOURCE_LIST_HEIGHT;
        assert_eq!(
            visible_row_range(
                max_offset,
                SOURCE_LIST_HEIGHT,
                SOURCE_ROW_HEIGHT,
                rows as usize
            ),
            Some((9, 12))
        );
        // Overscroll past the end still names the last row.
        assert_eq!(
            visible_row_range(9999.0, SOURCE_LIST_HEIGHT, SOURCE_ROW_HEIGHT, 12),
            Some((11, 12))
        );
        // An overscrolled (negative) offset is the top, not a wrapped index.
        assert_eq!(
            visible_row_range(-50.0, SOURCE_LIST_HEIGHT, SOURCE_ROW_HEIGHT, 12),
            Some((0, 3))
        );
    }

    /// The cache gate must estimate the frame the media layer really returns:
    /// the fixed capture width and the surface aspect, not the painted card.
    /// Unknown sizes get a square at that width, the worst reasonable frame.
    #[test]
    fn the_frame_estimate_follows_the_capture_width_and_source_aspect() {
        fn surface(width: u32, height: u32) -> ScreenSource {
            ScreenSource {
                id: 1,
                kind: SourceKind::Window,
                title: "surface".into(),
                display_id: -1,
                width,
                height,
                monitor: None,
            }
        }
        assert_eq!(preview_frame_size(&surface(1920, 1080)), (320, 180));
        assert_eq!(preview_frame_size(&surface(1280, 720)), (320, 180));
        assert_eq!(
            preview_frame_size(&surface(0, 0)),
            (SCREEN_THUMBNAIL_WIDTH, SCREEN_THUMBNAIL_WIDTH),
            "an unknown size is not trusted as a small frame"
        );
        assert_eq!(
            preview_frame_size(&surface(100, 50)),
            (100, 50),
            "a small surface is never upscaled"
        );
        assert_eq!(preview_frame_size(&surface(1080, 1920)), (320, 568));
        assert_eq!(
            preview_frame_size(&surface(200, 5000)),
            (162, PREVIEW_MAX_SIDE),
            "an extreme portrait surface is bounded by the capture side"
        );
        let (width, height) = preview_frame_size(&surface(1920, 1080));
        assert_eq!(
            width as usize * height as usize * 4,
            230_400,
            "a 1080p surface really costs 320×180 BGRA bytes"
        );
    }

    /// No layout means no visibility claim: the batch falls back to the small
    /// bootstrap set, which is the first-frame and restart behaviour.
    #[test]
    fn an_unknown_layout_has_no_visible_range() {
        assert_eq!(
            visible_row_range(0.0, SOURCE_LIST_HEIGHT, SOURCE_ROW_HEIGHT, 0),
            None,
            "an empty picker has no rows to prioritise"
        );
        assert_eq!(
            visible_row_range(0.0, 0.0, SOURCE_ROW_HEIGHT, 12),
            None,
            "a zero-height viewport is unknown"
        );
        assert_eq!(
            visible_row_range(0.0, SOURCE_LIST_HEIGHT, 0.0, 12),
            None,
            "a zero-height row is unknown"
        );
        assert_eq!(
            visible_row_range(f32::NAN, SOURCE_LIST_HEIGHT, SOURCE_ROW_HEIGHT, 12),
            None,
            "a non-finite offset is unknown"
        );
        assert_eq!(
            visible_row_range(0.0, f32::INFINITY, SOURCE_ROW_HEIGHT, 12),
            None,
            "a non-finite viewport is unknown"
        );
    }
}
