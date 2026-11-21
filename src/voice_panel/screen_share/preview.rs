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
    /// stops it from touching a newer session's flag. The in-flight latch is
    /// deliberately kept: a capture that already runs must return before the
    /// next generation starts one, otherwise two captures overlap after a DPI
    /// or width change.
    pub(super) fn clear_previews(&mut self) {
        self.preview_epoch = self.preview_epoch.wrapping_add(1);
        self.previews.clear();
        self.preview_failures.clear();
        self.preview_pending.clear();
        self.preview_batching = false;
        self.preview_bytes = 0;
        self.preview_rejected = 0;
        self.preview_cards = 0;
    }

    /// Claim the next visible source that still needs a picture, marking it
    /// pending. `None` means the batch has no work left for this epoch and
    /// category. Called before every capture, so a row added while the batch
    /// waits on a capture is picked up without starting a second batch.
    pub(super) fn take_next_preview_source(
        &mut self,
        epoch: u64,
        kind: SourceKind,
    ) -> Option<ScreenSource> {
        if !self.open || self.restart || self.preview_epoch != epoch || self.kind != kind {
            return None;
        }
        let columns = self.preview_columns.max(1);
        let visible = self.visible_rows;
        let mut candidates: Vec<(usize, ScreenSource)> = self
            .snapshot(kind)
            .iter()
            .enumerate()
            .filter(|(_, source)| {
                let Some(identity) = source.identity() else {
                    return false;
                };
                !self.previews.contains_key(&identity)
                    && !self.preview_failures.contains_key(&identity)
                    && !self.preview_pending.contains(&identity)
            })
            // A duplicated identity is ambiguous and can never be accepted by
            // `finish_preview`, so it is not captured at all.
            .filter(|(_, source)| {
                source
                    .identity()
                    .is_some_and(|identity| self.source_for_identity(&identity).is_some())
            })
            .map(|(index, source)| (index, source.clone()))
            .collect();
        // Cards inside the last painted viewport are captured first; the rest
        // follow in list order. Before the first layout, everything counts as
        // visible so the original order is kept.
        candidates.sort_by_key(|(index, _)| {
            let row = index / columns;
            let hidden = visible.is_some_and(|(top, bottom)| row < top || row > bottom);
            (u8::from(hidden), *index)
        });
        let source = candidates.into_iter().next()?.1;
        self.preview_pending.insert(source.identity()?);
        Some(source)
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

    /// Row of a source in the current category, used to rank cache eviction.
    fn preview_row(&self, identity: &SourceIdentity) -> Option<usize> {
        let index = self
            .snapshot(self.kind)
            .iter()
            .position(|source| source.identity().as_ref() == Some(identity))?;
        Some(index / self.preview_columns.max(1))
    }

    /// Whether the card is inside the last painted grid viewport. Before the
    /// first layout everything counts as visible, so insertion order is kept.
    fn preview_is_visible(&self, identity: &SourceIdentity) -> bool {
        let Some((top, bottom)) = self.visible_rows else {
            return true;
        };
        self.preview_row(identity)
            .is_some_and(|row| row >= top && row <= bottom)
    }

    /// Adopt the rows of the last painted viewport and retry the cards that
    /// just came back on screen with only a memory refusal. Such a card had no
    /// chance to load while it was hidden; once it is visible again the next
    /// capture may win the budget. Returns whether any refusal was cleared.
    pub(super) fn set_visible_rows(&mut self, columns: usize, top: usize, bottom: usize) -> bool {
        self.preview_columns = columns.max(1);
        let next = (top.min(bottom), bottom.max(top));
        let previous = self.visible_rows;
        self.visible_rows = Some(next);
        let Some((old_top, old_bottom)) = previous else {
            // The first layout decides visibility; no card was hidden before.
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
                    && self.preview_row(identity).is_some_and(|row| {
                        let newly_visible = row >= next.0 && row <= next.1;
                        let was_visible = row >= old_top && row <= old_bottom;
                        newly_visible && !was_visible
                    })
            })
            .map(|(identity, _)| identity.clone())
            .collect();
        let retried = !retry.is_empty();
        for identity in retry {
            self.preview_failures.remove(&identity);
        }
        retried
    }

    /// Make room for a visible card by releasing cached pictures that are
    /// scrolled out of view, farthest first. The released card gets the honest
    /// memory reason and is retried by the next refresh. Only a visible card
    /// may evict anything; a hidden frame that does not fit is rejected
    /// without disturbing what the user is looking at. The selected source is
    /// never a victim: losing the chosen card would leave the dialog showing
    /// "превышен лимит памяти" for the surface it is about to publish.
    fn evict_hidden_previews(&mut self, identity: &SourceIdentity, existing: usize, bytes: usize) {
        if !self.preview_is_visible(identity) {
            return;
        }
        let selected = self.selected_identity.clone();
        let grows_count = usize::from(existing == 0);
        let over = |previews: &HashMap<SourceIdentity, PreviewThumb>, total: usize| -> bool {
            previews.len() + grows_count > MAX_SCREEN_PREVIEWS
                || total.saturating_sub(existing) + bytes > MAX_SCREEN_PREVIEW_BYTES
        };
        if !over(&self.previews, self.preview_bytes) {
            return;
        }
        let mut victims: Vec<(usize, SourceIdentity, usize)> = self
            .previews
            .iter()
            .filter(|(candidate, _)| *candidate != identity)
            .filter(|(candidate, _)| selected.as_ref() != Some(*candidate))
            .filter(|(candidate, _)| !self.preview_is_visible(candidate))
            .map(|(candidate, thumb)| {
                (
                    self.preview_row(candidate).unwrap_or(usize::MAX),
                    candidate.clone(),
                    thumb.bytes,
                )
            })
            .collect();
        // Farthest row first: the cards the user is least likely to scroll to
        // are released before a nearer one.
        victims.sort_by(|left, right| right.0.cmp(&left.0));
        for (_, victim, size) in victims {
            if !over(&self.previews, self.preview_bytes) {
                break;
            }
            if self.previews.remove(&victim).is_some() {
                self.preview_bytes = self.preview_bytes.saturating_sub(size);
                self.preview_pending.remove(&victim);
                self.preview_failures
                    .insert(victim, PREVIEW_MEMORY_REASON.to_owned());
            }
        }
    }

    /// Add one picture to the bounded cache. A picture over the width, count
    /// or byte bound is counted as rejected and never queued; a visible card
    /// first releases hidden pictures that do not fit the budget.
    pub(crate) fn insert_preview(&mut self, identity: SourceIdentity, frame: PreviewFrame) -> bool {
        if frame.width == 0
            || frame.height == 0
            || frame.width > SCREEN_THUMBNAIL_WIDTH
            || frame.height > SCREEN_THUMBNAIL_HEIGHT
            || frame.bgra.len() != frame.width as usize * frame.height as usize * 4
        {
            self.preview_rejected = self.preview_rejected.saturating_add(1);
            return false;
        }
        let bytes = frame.bgra.len();
        let existing = self
            .previews
            .get(&identity)
            .map(|thumb| thumb.bytes)
            .unwrap_or(0);
        self.evict_hidden_previews(&identity, existing, bytes);
        let grows_count = usize::from(existing == 0);
        if self.previews.len() + grows_count > MAX_SCREEN_PREVIEWS
            || self.preview_bytes.saturating_sub(existing) + bytes > MAX_SCREEN_PREVIEW_BYTES
        {
            self.preview_rejected = self.preview_rejected.saturating_add(1);
            return false;
        }
        let (width, height) = (frame.width, frame.height);
        let Some(image) = preview_render_image(frame) else {
            self.preview_rejected = self.preview_rejected.saturating_add(1);
            return false;
        };
        self.preview_bytes = self.preview_bytes.saturating_sub(existing) + bytes;
        self.previews.insert(
            identity.clone(),
            PreviewThumb {
                image,
                width,
                height,
                bytes,
            },
        );
        self.preview_failures.remove(&identity);
        self.preview_pending.remove(&identity);
        true
    }

    pub(crate) fn insert_preview_failure(&mut self, identity: SourceIdentity, message: String) {
        self.preview_pending.remove(&identity);
        self.preview_failures.insert(identity, message);
    }

    /// Apply a background result. A different epoch (closed/old picker), a
    /// non-pending id (cancelled batch) or an id absent from the current
    /// snapshot is ignored and never inserted.
    pub(crate) fn finish_preview(
        &mut self,
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
        // The result belongs to the surface, not to the number its capture
        // started with: after a reorder the same id can name another display.
        if self.source_for_identity(&identity).is_none() {
            return false;
        }
        match result {
            Ok(frame) => {
                if !self.insert_preview(identity.clone(), frame) {
                    self.insert_preview_failure(identity, PREVIEW_MEMORY_REASON.to_owned());
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
    pub(super) fn retain_preview_snapshot(&mut self, kind: SourceKind, sources: &[ScreenSource]) {
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
        for identity in gone {
            if let Some(thumb) = self.previews.remove(&identity) {
                self.preview_bytes = self.preview_bytes.saturating_sub(thumb.bytes);
            }
        }
        self.preview_failures
            .retain(|identity, _| identity_kind(identity) != kind || present(identity));
        self.preview_pending
            .retain(|identity| identity_kind(identity) != kind || present(identity));
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

impl VoicePanel {
    /// Native width to request from the capturer. The first frame of a picker
    /// has not measured its card yet, so it uses the normal two-column width
    /// instead of the cache minimum.
    pub(super) fn thumbnail_width(&self) -> u32 {
        if self.screen.preview_width == 0 {
            SCREEN_THUMBNAIL_DEFAULT_WIDTH
        } else {
            self.screen.preview_width
        }
    }

    /// Adopt the physical card width of the current frame. A larger scale (a
    /// window moved to a 125/150% monitor) starts a new preview generation:
    /// the old pictures are released, the session epoch moves so an in-flight
    /// capture cannot land in the new card, and the visible rows are requested
    /// again at the sharper size. A smaller request keeps the sharper cache.
    pub(in crate::voice_panel) fn set_thumbnail_width(
        &mut self,
        width: u32,
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
        self.screen.clear_previews();
        self.request_screen_previews(cx);
    }

    /// Adopt the visible grid rows of the current frame. A card that comes
    /// back into view after a memory refusal is retried at once: its failure
    /// was about the export budget, not about the surface.
    pub(in crate::voice_panel) fn set_preview_viewport(
        &mut self,
        columns: usize,
        top: usize,
        bottom: usize,
        cx: &mut Context<Self>,
    ) {
        if self.screen.set_visible_rows(columns, top, bottom) {
            self.request_screen_previews(cx);
        }
    }

    /// Start background thumbnail generation for the visible category.
    ///
    /// Cached and pending ids are skipped. At most one batch, and inside it at
    /// most one capture, runs at a time. A request made while the batch is
    /// active only returns: the running batch re-reads the visible snapshot
    /// before every capture and re-requests pending work when it ends, so a
    /// source that appeared mid-batch is still captured, just not concurrently.
    /// Before every source the batch re-checks that the same picker, epoch and
    /// category are still current, so a closed dialog or a tab switch never
    /// starts another capture. The capture itself is bounded by the media layer
    /// and runs on the background executor, never on the UI thread or the voice
    /// worker.
    pub(crate) fn request_screen_previews(&mut self, cx: &mut Context<Self>) {
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
        let Some(first) = self.screen.take_next_preview_source(epoch, kind) else {
            return;
        };
        self.screen.preview_batching = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut next = Some(first);
            while let Some(source) = next {
                let Some(identity) = source.identity() else {
                    break;
                };
                #[cfg(test)]
                let provider = provider.clone();
                let keep = this
                    .update(cx, |panel, _| {
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
                let _ = this.update(cx, |panel, _| {
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
                let _ = this.update(cx, |panel, _| {
                    panel.screen.preview_in_flight = false;
                });
                let alive = this
                    .update(cx, |panel, _| {
                        panel.screen.open
                            && !panel.screen.restart
                            && panel.screen.preview_epoch == epoch
                            && panel.screen.kind == kind
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
                let _ = this.update(cx, |panel, cx| {
                    // A vanished id is ignored but does not stop the rest of
                    // the batch; only an epoch/category change above does.
                    if panel.screen.finish_preview(epoch, identity, result) {
                        cx.notify();
                    }
                });
                // The next claim re-checks the session, so a row added while
                // this capture ran joins the same batch.
                next = this
                    .update(cx, |panel, _| {
                        panel.screen.take_next_preview_source(epoch, kind)
                    })
                    .ok()
                    .flatten();
            }
            let _ = this.update(cx, |panel, cx| {
                // Only this batch's epoch may release the batch flag: a newer
                // picker or dialog already reset it, and a late end must not
                // clear a batch that session started. The in-flight latch is
                // already released above, so a newer generation can still start
                // its own batch even when this epoch is stale.
                panel.screen.end_preview_batch(epoch);
                // Rows added after the last claim, a refresh, a tab switch or a
                // generation that waited for this capture: all of them are
                // picked up here, on the current category.
                if panel.screen.open && !panel.screen.restart {
                    panel.request_screen_previews(cx);
                }
            });
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
        let mut screen = ScreenShareUi::default();
        let id = SourceIdentity::Window(7);
        let frame = PreviewFrame {
            width: 2,
            height: 1,
            bgra: [color, color].concat(),
        };
        assert!(
            screen.insert_preview(id.clone(), frame),
            "the picture fits the cache"
        );
        let thumb = screen.previews.get(&id).expect("the picture is cached");
        thumb.image.as_bytes(0).expect("one frame").to_vec()
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
}
