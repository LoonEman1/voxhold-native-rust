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
    ///
    /// The in-flight latch is deliberately kept: a capture that already runs
    /// must return before the next generation starts one, otherwise two
    /// captures overlap after a DPI or width change. The grid rows are kept as
    /// well, so the same visible cards are requested again at the new width;
    /// [`Self::reset_preview_viewport`] is the separate new-dialog reset.
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
    }

    /// A new picker, a tab switch or a restart dialog starts at the top of its
    /// own list: a scroll offset or a row range of the previous dialog must
    /// never choose the captures of the next one. Budget refusals are about the
    /// previous surface set, so the next layout claims those cards again.
    pub(super) fn reset_preview_viewport(&mut self) {
        self.list_scroll = ScrollHandle::new();
        self.visible_rows = None;
        self.preview_failures
            .retain(|_, reason| reason != PREVIEW_MEMORY_REASON);
        self.preview_unfit.clear();
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
    /// Only the painted rows plus one row of margin are claimed; before the
    /// first layout the snapshot order is used only for the first
    /// [`BOOTSTRAP_PREVIEW_CARDS`] cards. Room is checked before any capture
    /// starts: off-screen pictures are released first and, when nothing can be
    /// evicted, no capturer is started at all. Otherwise a full cache would
    /// answer every visible card with a memory note instead of a picture.
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

    /// The next card in claim order: the painted rows and their margin, else
    /// the bootstrap cards of a category the dialog has not painted yet.
    fn next_preview_source(&self, kind: SourceKind) -> Option<ScreenSource> {
        let sources = self.snapshot(kind);
        let columns = self.preview_columns.max(1);
        let rows = sources.len().div_ceil(columns);
        let (start, end) = capture_row_range(self.visible_rows, columns, rows);
        let start = (start * columns).min(sources.len());
        let end = (end * columns).min(sources.len());
        sources[start..end]
            .iter()
            .find(|source| self.preview_source_ready(source))
            .cloned()
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

    /// Native width to request from the capturer. The first frame of a picker
    /// has not measured its card yet, so it uses the normal two-column width
    /// instead of the cache minimum.
    pub(super) fn thumbnail_width(&self) -> u32 {
        if self.preview_width == 0 {
            SCREEN_THUMBNAIL_DEFAULT_WIDTH
        } else {
            self.preview_width
        }
    }

    /// Row of a source in the current category, used to rank visibility and
    /// cache eviction.
    fn preview_row(&self, identity: &SourceIdentity) -> Option<usize> {
        let index = self
            .snapshot(self.kind)
            .iter()
            .position(|source| source.identity().as_ref() == Some(identity))?;
        Some(index / self.preview_columns.max(1))
    }

    /// Whether the card is inside the last painted grid viewport. Before the
    /// first layout there is no visible row to protect, so a picture of a
    /// category the dialog has not painted yet (a tab switch) may be evicted.
    fn preview_is_visible(&self, identity: &SourceIdentity) -> bool {
        let Some((top, bottom)) = self.visible_rows else {
            return false;
        };
        self.preview_row(identity)
            .is_some_and(|row| row >= top && row <= bottom)
    }

    /// Whether the card moved into the painted rows since the previous frame.
    /// Such a card is retried: a budget refusal from its hidden time is no
    /// longer true.
    fn row_newly_visible(
        &self,
        identity: &SourceIdentity,
        previous: (usize, usize),
        next: (usize, usize),
    ) -> bool {
        self.preview_row(identity).is_some_and(|row| {
            row >= next.0 && row <= next.1 && (row < previous.0 || row > previous.1)
        })
    }

    /// Adopt the rows of the last painted viewport. A card that comes back
    /// into view after a memory refusal is cleared and retried at once: its
    /// earlier failure was about the export budget, not about the surface.
    /// A frame that did not fit (`preview_unfit`) gets the same second chance.
    /// Returns whether the rows changed, so the dialog asks for previews once
    /// and a plain repaint asks nothing.
    pub(super) fn set_visible_rows(&mut self, columns: usize, top: usize, bottom: usize) -> bool {
        self.preview_columns = columns.max(1);
        let next = (top.min(bottom), bottom.max(top));
        let previous = self.visible_rows;
        self.visible_rows = Some(next);
        let Some((old_top, old_bottom)) = previous else {
            // The first layout decides visibility; the bootstrap set already
            // asked for the top rows.
            return false;
        };
        if previous == Some(next) {
            return false;
        }
        let retry: Vec<SourceIdentity> = self
            .preview_failures
            .iter()
            .filter(|(identity, reason)| {
                **reason == PREVIEW_MEMORY_REASON
                    && self.row_newly_visible(identity, (old_top, old_bottom), next)
            })
            .map(|(identity, _)| identity.clone())
            .collect();
        for identity in retry {
            self.preview_failures.remove(&identity);
        }
        let unfit: Vec<SourceIdentity> = self
            .preview_unfit
            .iter()
            .filter(|identity| self.row_newly_visible(identity, (old_top, old_bottom), next))
            .cloned()
            .collect();
        for identity in unfit {
            self.preview_unfit.remove(&identity);
        }
        true
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
    /// picture that is neither visible, selected nor in flight goes first.
    /// Each victim gets the honest memory reason and is retried when it really
    /// returns to the viewport. Returns false when nothing can be evicted, so
    /// the caller never starts a capture whose result could not be kept.
    fn evict_previews(
        &mut self,
        window: &mut Window,
        identity: &SourceIdentity,
        bytes: usize,
    ) -> bool {
        while !self.preview_has_room_for(identity, bytes) {
            let Some(oldest) = self.oldest_evictable_preview() else {
                return false;
            };
            if let Some(thumb) = self.previews.remove(&oldest) {
                self.preview_bytes = self.preview_bytes.saturating_sub(thumb.bytes);
                self.release_image(window, thumb.image);
                // Not a capture failure: an off-screen picture paid for the
                // card the user is looking at. A picture of another category is
                // not shown anywhere now, so only the current tab's victim gets
                // the note; it is dropped again when the card returns.
                if identity_kind(&oldest) == self.kind {
                    self.preview_failures
                        .insert(oldest, PREVIEW_MEMORY_REASON.to_owned());
                }
            }
        }
        true
    }

    /// The oldest cached identity an eviction may take: not selected, not in
    /// flight and, once the rows of this category were painted, not one of its
    /// visible cards. A visibility range of another category protects nothing
    /// here, so a tab switch can free the pictures of the hidden one.
    fn oldest_evictable_preview(&mut self) -> Option<SourceIdentity> {
        let selected = self.selected_identity.clone();
        let mut index = 0;
        while index < self.preview_order.len() {
            let identity = self.preview_order[index].clone();
            if !self.previews.contains_key(&identity) {
                self.preview_order.remove(index);
                continue;
            }
            let protected = selected.as_ref() == Some(&identity)
                || self.preview_pending.contains(&identity)
                || self.preview_is_visible(&identity);
            if protected {
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

    /// Conservative BGRA size of one card picture: the current request width
    /// and the surface's own aspect, never the painted card width. A surface
    /// with no reported size is treated as the whole request rectangle, the
    /// worst reasonable frame that still fits a card.
    fn preview_frame_estimate(&self, source: &ScreenSource) -> usize {
        let (width, height) = preview_frame_size(source, self.thumbnail_width());
        width as usize * height as usize * 4
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
    /// width, height, count or byte bound is counted as rejected and never
    /// queued. Once the grid has painted its rows, a visible card first
    /// releases hidden pictures, exactly like the background path; without a
    /// laid-out grid a full cache is a hard refusal, so a test can observe the
    /// count and byte bounds themselves.
    #[cfg(test)]
    pub(crate) fn insert_preview(
        &mut self,
        window: &mut Window,
        identity: SourceIdentity,
        frame: PreviewFrame,
    ) -> bool {
        if self.visible_rows.is_some() && valid_preview_bytes(&frame).is_some() {
            let bytes = frame.bgra.len();
            if !self.preview_has_room_for(&identity, bytes) {
                self.evict_previews(window, &identity, bytes);
            }
        }
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
        let (width, height) = (frame.width, frame.height);
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
        let previous = self.previews.insert(
            identity.clone(),
            PreviewThumb {
                image,
                width,
                height,
                bytes,
            },
        );
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
/// never reach a card (zero size, over the capture rectangle, malformed
/// buffer).
fn valid_preview_bytes(frame: &PreviewFrame) -> Option<usize> {
    if frame.width == 0
        || frame.height == 0
        || frame.width > SCREEN_THUMBNAIL_WIDTH
        || frame.height > SCREEN_THUMBNAIL_HEIGHT
        || frame.bgra.len() != frame.width as usize * frame.height as usize * 4
    {
        return None;
    }
    Some(frame.bgra.len())
}

/// Cards the first batch may claim before the dialog has painted a visibility
/// range: two rows of the default two-column grid. Everything past them waits
/// for a real scroll instead of walking the whole tab.
pub(super) const BOOTSTRAP_PREVIEW_CARDS: usize = 4;
/// A frame the media layer returned but a card could never paint: an honest
/// note, unlike a full cache, which is not a failure of the surface.
const PREVIEW_UNREADABLE_REASON: &str = "Предпросмотр источника не получен.";

/// Half-open grid rows `[start, end)` the batch may claim: the painted rows
/// plus one row of margin above and below. `None` (no layout yet) claims the
/// first row only, bounded by [`BOOTSTRAP_PREVIEW_CARDS`].
fn capture_row_range(
    visible: Option<(usize, usize)>,
    columns: usize,
    row_count: usize,
) -> (usize, usize) {
    let columns = columns.max(1);
    match visible {
        Some((top, bottom)) => (
            top.saturating_sub(1).min(row_count),
            bottom.saturating_add(2).min(row_count),
        ),
        None => (0, BOOTSTRAP_PREVIEW_CARDS.div_ceil(columns).min(row_count)),
    }
}

/// Size of the BGRA picture the media layer returns for `source`: at most
/// `max_width` wide and [`SCREEN_THUMBNAIL_HEIGHT`] high, the surface's own
/// aspect and even sides, the same rules as `screen_capture::preview_from_bgra`.
/// The cache gate estimates the real frame with this, never the painted card.
fn preview_frame_size(source: &ScreenSource, max_width: u32) -> (u32, u32) {
    let max_width = max_width.clamp(2, SCREEN_THUMBNAIL_WIDTH);
    let (width, height) = (source.width, source.height);
    if width == 0 || height == 0 {
        // No reported size: the whole request rectangle is the worst
        // reasonable frame, not an unbounded portrait guess.
        return (max_width, SCREEN_THUMBNAIL_HEIGHT);
    }
    if width <= max_width && height <= SCREEN_THUMBNAIL_HEIGHT {
        return ((width & !1).max(2), (height & !1).max(2));
    }
    if u64::from(max_width) * u64::from(height)
        <= u64::from(SCREEN_THUMBNAIL_HEIGHT) * u64::from(width)
    {
        let scaled = (u64::from(max_width) * u64::from(height) / u64::from(width)) as u32;
        (max_width, (scaled & !1).max(2))
    } else {
        let scaled =
            (u64::from(SCREEN_THUMBNAIL_HEIGHT) * u64::from(width) / u64::from(height)) as u32;
        ((scaled & !1).max(2), SCREEN_THUMBNAIL_HEIGHT)
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

/// Test seams of the visible-area picker. They expose only what a UI test
/// needs to drive the real render path: the identities the current row range
/// would claim. The scroll itself is a wheel event on `#share-sources`.
#[cfg(test)]
impl VoicePanel {
    /// Identities the batch may claim for the painted rows and their margin,
    /// in visual order. An unmeasured grid returns an empty list.
    pub(crate) fn test_visible_screen_previews(&self) -> Vec<SourceId> {
        let Some((top, bottom)) = self.screen.visible_rows else {
            return Vec::new();
        };
        let sources = self.screen.sources();
        let columns = self.screen.preview_columns.max(1);
        let rows = sources.len().div_ceil(columns);
        let (start, end) = capture_row_range(Some((top, bottom)), columns, rows);
        let start = (start * columns).min(sources.len());
        let end = (end * columns).min(sources.len());
        sources[start..end]
            .iter()
            .map(ScreenSource::source_id)
            .collect()
    }
}

impl VoicePanel {
    /// Native width to request from the capturer, derived from the painted
    /// card and the Windows scale.
    pub(super) fn thumbnail_width(&self) -> u32 {
        self.screen.thumbnail_width()
    }

    /// Adopt the physical card width of the current frame. A larger scale (a
    /// window moved to a 125/150% monitor) starts a new preview generation:
    /// the old pictures are released, the session epoch moves so an in-flight
    /// capture cannot land in the new card, and the visible rows are requested
    /// again at the sharper size. A smaller request keeps the sharper cache.
    pub(in crate::voice_panel) fn set_thumbnail_width(
        &mut self,
        width: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = width.clamp(2, SCREEN_THUMBNAIL_WIDTH);
        if width <= self.screen.preview_width {
            return;
        }
        let first_measure = self.screen.preview_width == 0;
        self.screen.preview_width = width;
        if first_measure {
            // No picture can exist before the first frame, and a test seam may
            // have injected one on purpose: keep it.
            return;
        }
        if !self.screen.open || self.screen.restart {
            return;
        }
        // The generation waits for the in-flight latch: the released pictures
        // belong to the previous epoch, the running capture returns first.
        self.screen.clear_previews(window);
        self.request_screen_previews(window, cx);
    }

    /// Adopt the visible grid rows of the current frame and ask for the cards
    /// that need one. A card that comes back into view after a memory refusal
    /// is retried at once: its failure was about the export budget, not about
    /// the surface. The dialog itself defers the request instead, so this seam
    /// is only driven by tests.
    #[cfg(test)]
    pub(in crate::voice_panel) fn set_preview_viewport(
        &mut self,
        columns: usize,
        top: usize,
        bottom: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.screen.set_visible_rows(columns, top, bottom);
        if changed {
            self.request_screen_previews(window, cx);
        }
        changed
    }

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
        if self.screen.preview_batching || self.screen.preview_in_flight {
            return;
        }
        let kind = self.screen.kind;
        let epoch = self.screen.preview_epoch;
        let width = self.thumbnail_width();
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
                let _ = this.update_in(cx, |panel, _, _| {
                    panel.screen.preview_in_flight = true;
                });
                let task = cx.background_spawn(async move {
                    #[cfg(test)]
                    if let Some(provider) = provider {
                        return provider(&source);
                    }
                    screen_capture::capture_thumbnail(&source, width)
                });
                let result = task.await;
                // Release the capture latch before any epoch check: another
                // generation is allowed to start only after this capture has
                // really returned.
                let _ = this.update_in(cx, |panel, _, _| {
                    panel.screen.preview_in_flight = false;
                });
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
                // Rows added after the last claim, a refresh or a generation
                // that waited for this capture: all of them are picked up
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
                        panel.screen.preview_in_flight = false;
                    }
                });
            }
        })
        .detach();
    }
    /// Take the newest local preview for the on-air card. The previous picture
    /// is released only after the slot already points at the new frame. The
    /// protection does not come from the old `Arc`: `drop_image` removes the
    /// atlas entry regardless of other owners. It comes from the replacement
    /// running in the poll before the next repaint, so the renderer never
    /// paints a picture whose entry was already dropped.
    ///
    /// While no visible card asks for a preview (Idle, a hidden phase, an
    /// unpainted room pane, a minimized window or a card scrolled out of the
    /// viewport) the pending frame is drained without being counted as shown:
    /// a picture the user never saw must not be reported, and a leftover frame
    /// must not reappear when the card returns. The predicate is exactly the
    /// demand's, so no `RenderImage` is created for a hidden card.
    pub(in crate::voice_panel) fn take_screen_preview(&mut self, window: &mut Window) -> bool {
        let Some(handle) = &self.handle else {
            return false;
        };
        if !self.screen_preview_wanted(window) {
            handle.discard_screen_frame();
            return false;
        }
        let Some(frame) = handle.take_screen_frame() else {
            return false;
        };
        let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra) else {
            return false;
        };
        let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
            buffer
        )]));
        self.screen.set_preview(window, image);
        true
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

    /// The claim range covers whole and partial rows plus one row of margin on
    /// each side. The picker must never claim a row's picture just because it
    /// is the next one in the snapshot, and an unmeasured grid may claim only
    /// the bootstrap row.
    #[test]
    fn the_capture_rows_cover_the_viewport_and_one_margin_row() {
        assert_eq!(capture_row_range(Some((0, 1)), 2, 12), (0, 3));
        assert_eq!(capture_row_range(Some((2, 3)), 2, 12), (1, 5));
        assert_eq!(capture_row_range(Some((0, 0)), 2, 1), (0, 1));
        assert_eq!(
            capture_row_range(Some((11, 11)), 2, 12),
            (10, 12),
            "the end of the list stops at the last row instead of running over"
        );
        assert_eq!(
            capture_row_range(Some((0, 0)), 2, 0),
            (0, 0),
            "an empty picker has no rows to prioritise"
        );
    }

    /// No layout means no visibility claim: the batch falls back to the small
    /// bootstrap set, which is the first-frame and restart behaviour. At the
    /// default two columns that is the first row of four cards.
    #[test]
    fn an_unknown_layout_claims_only_the_bootstrap_cards() {
        assert_eq!(capture_row_range(None, 2, 12), (0, 2));
        assert_eq!(capture_row_range(None, 1, 12), (0, 4));
        assert_eq!(capture_row_range(None, 2, 0), (0, 0));
    }

    /// The cache gate must estimate the frame the media layer really returns:
    /// the current request width and the surface aspect, never the painted
    /// card. Unknown sizes get the whole request rectangle.
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
        assert_eq!(
            preview_frame_size(&surface(1920, 1080), SCREEN_THUMBNAIL_WIDTH),
            (480, 270)
        );
        assert_eq!(
            preview_frame_size(&surface(1920, 1080), SCREEN_THUMBNAIL_DEFAULT_WIDTH),
            (288, 162)
        );
        assert_eq!(
            preview_frame_size(&surface(0, 0), SCREEN_THUMBNAIL_DEFAULT_WIDTH),
            (SCREEN_THUMBNAIL_DEFAULT_WIDTH, SCREEN_THUMBNAIL_HEIGHT),
            "an unknown size is not trusted as a small frame"
        );
        assert_eq!(
            preview_frame_size(&surface(100, 50), SCREEN_THUMBNAIL_WIDTH),
            (100, 50),
            "a small surface is never upscaled"
        );
        assert_eq!(
            preview_frame_size(&surface(1080, 1920), SCREEN_THUMBNAIL_WIDTH),
            (152, 270),
            "a portrait surface is bounded by the capture height"
        );
        assert_eq!(
            preview_frame_size(&surface(200, 5000), SCREEN_THUMBNAIL_WIDTH),
            (10, 270),
            "an extreme portrait surface is bounded by the capture height"
        );
        let (width, height) = preview_frame_size(&surface(1920, 1080), SCREEN_THUMBNAIL_WIDTH);
        assert_eq!(
            width as usize * height as usize * 4,
            518_400,
            "a 1080p surface really costs 480×270 BGRA bytes"
        );
    }
}
