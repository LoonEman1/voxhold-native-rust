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
    /// stops it from touching a newer session's flag.
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
        let source = self
            .snapshot(kind)
            .iter()
            .filter(|source| {
                let Some(identity) = source.identity() else {
                    return false;
                };
                !self.previews.contains_key(&identity)
                    && !self.preview_failures.contains_key(&identity)
                    && !self.preview_pending.contains(&identity)
            })
            // A duplicated identity is ambiguous and can never be accepted by
            // `finish_preview`, so it is not captured at all.
            .find(|source| {
                source
                    .identity()
                    .is_some_and(|identity| self.source_for_identity(&identity).is_some())
            })?
            .clone();
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

    /// Add one picture to the bounded cache. A picture over the width, count
    /// or byte bound is counted as rejected and never queued.
    pub(crate) fn insert_preview(&mut self, identity: SourceIdentity, frame: PreviewFrame) -> bool {
        if frame.width == 0
            || frame.height == 0
            || frame.width > SCREEN_THUMBNAIL_WIDTH
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
        let grows_count = usize::from(existing == 0);
        if self.previews.len() + grows_count > MAX_SCREEN_PREVIEWS
            || self.preview_bytes.saturating_sub(existing) + bytes > MAX_SCREEN_PREVIEW_BYTES
        {
            self.preview_rejected = self.preview_rejected.saturating_add(1);
            return false;
        }
        let Some(image) = preview_render_image(frame) else {
            self.preview_rejected = self.preview_rejected.saturating_add(1);
            return false;
        };
        self.preview_bytes = self.preview_bytes.saturating_sub(existing) + bytes;
        self.previews
            .insert(identity.clone(), PreviewThumb { image, bytes });
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
        if self.screen.preview_batching {
            return;
        }
        let kind = self.screen.kind;
        let epoch = self.screen.preview_epoch;
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
                let task = cx.background_spawn(async move {
                    #[cfg(test)]
                    if let Some(provider) = provider {
                        return provider(&source);
                    }
                    screen_capture::capture_thumbnail(&source, SCREEN_THUMBNAIL_WIDTH)
                });
                let result = task.await;
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
