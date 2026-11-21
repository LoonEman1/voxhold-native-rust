//! Chat media lifecycle: one bounded thumbnail cache, link previews and the
//! single image viewer. Every background result is checked against the chat,
//! the access generation and the request id before it touches the UI, so a
//! late answer can never reopen a closed viewer or show a channel the user
//! left.

use super::*;
use crate::media::{self, PreviewEntry, ThumbKey, ThumbSourceKey, Viewer, ViewerKey};
use std::sync::Arc;
use std::time::Instant;
use voxhold_api::Attachment;

/// Go `previewConcurrency`; a message with two links never floods the queue.
const PREVIEW_CONCURRENCY: usize = 3;
/// Preview card picture, Go `scaleToFit(img, 98, 76)`.

/// Resize a borrowed server thumbnail into the requested 16 px bucket and
/// convert it to the BGRA surface GPUI uploads. The original is only read, so
/// the caller keeps ownership of its pixels.
pub(crate) fn thumbnail_frame(
    image: &image::DynamicImage,
    width: u32,
    height: u32,
) -> (image::Frame, usize) {
    let (width, height) = (width.max(1), height.max(1));
    // Record the borrowed input with its real size. It is read in place and
    // never materialised as a copy; the probe call documents that and lets the
    // O06 test compare the source against the buffers the pass allocates.
    note_scale_buffer(pixels(image.width(), image.height()), false);
    #[cfg(test)]
    scale_probe::expect_target(pixels(width, height));
    // Scale first: the only pixel buffer this path materialises is the
    // target-sized one. A source-sized `to_rgba8()` copy never exists (O06).
    // `imageops::resize` still allocates its own f32 scratch of
    // source_width × target_height, exactly as the previous path did; what
    // disappears is the source-sized RGBA copy before the resample.
    let resized = image.resize_exact(width, height, image::imageops::FilterType::CatmullRom);
    note_scale_buffer(pixels(resized.width(), resized.height()), true);
    let rgba = match resized {
        // The common RGBA path moves the buffer instead of copying it.
        image::DynamicImage::ImageRgba8(rgba) => rgba,
        other => other.to_rgba8(),
    };
    note_scale_buffer(pixels(rgba.width(), rgba.height()), true);
    let bytes = rgba.width() as usize * rgba.height() as usize * 4;
    (
        members::avatar_frame(image::DynamicImage::ImageRgba8(rgba)),
        bytes,
    )
}

fn pixels(width: u32, height: u32) -> usize {
    width as usize * height as usize
}

/// Test seam for O06: records the pixel buffers the scale path touches on this
/// thread. The borrowed input is reported with `copied = false`; every buffer
/// `thumbnail_frame` allocates is reported with `copied = true`. Any allocated
/// buffer with more pixels than the requested target sets `oversized`, i.e. a
/// decoded-source-sized copy exists again.
///
/// Limitation: this only sees buffers the module itself reports. A copy made
/// inside `image` without passing through `note_scale_buffer` would be
/// invisible without a global allocator; the viewer path therefore keeps the
/// source behind `&DynamicImage` and never clones it.
#[cfg(test)]
pub(crate) mod scale_probe {
    use std::cell::Cell;

    thread_local! {
        static PEAK: Cell<usize> = const { Cell::new(0) };
        static SOURCE: Cell<usize> = const { Cell::new(0) };
        static TARGET: Cell<usize> = const { Cell::new(usize::MAX) };
        static OVERSIZED: Cell<bool> = const { Cell::new(false) };
        static PASSES: Cell<usize> = const { Cell::new(0) };
        static INSTALLED: Cell<usize> = const { Cell::new(0) };
    }

    /// Start a pass for one target surface; allocations are compared against
    /// its pixel count.
    pub(super) fn expect_target(pixels: usize) {
        TARGET.with(|target| target.set(pixels));
        OVERSIZED.with(|flag| flag.set(false));
    }

    pub(super) fn observe(pixels: usize, copied: bool) {
        if copied {
            PEAK.with(|peak| peak.set(peak.get().max(pixels)));
            if pixels > TARGET.with(Cell::get) {
                OVERSIZED.with(|flag| flag.set(true));
            }
        } else {
            SOURCE.with(|source| source.set(source.get().max(pixels)));
        }
    }

    /// Count one started background rescale pass on this thread, so a test can
    /// prove a drag queues at most one pass behind the running one.
    pub(super) fn pass() {
        PASSES.with(|passes| passes.set(passes.get() + 1));
    }

    /// Count a pass that reached its UI half. A pass whose handle was dropped
    /// is cancelled and never installs a frame, so this counter stays put.
    pub(super) fn install() {
        INSTALLED.with(|installed| installed.set(installed.get() + 1));
    }

    /// Largest allocated buffer since the last `reset`, in pixels.
    pub(crate) fn peak_pixels() -> usize {
        PEAK.with(Cell::get)
    }

    /// Largest borrowed input since the last `reset`, in pixels.
    pub(crate) fn source_pixels() -> usize {
        SOURCE.with(Cell::get)
    }

    pub(crate) fn oversized() -> bool {
        OVERSIZED.with(Cell::get)
    }

    pub(crate) fn passes() -> usize {
        PASSES.with(Cell::get)
    }

    pub(crate) fn installed() -> usize {
        INSTALLED.with(Cell::get)
    }

    pub(crate) fn reset() {
        PEAK.with(|peak| peak.set(0));
        SOURCE.with(|source| source.set(0));
        TARGET.with(|target| target.set(usize::MAX));
        OVERSIZED.with(|flag| flag.set(false));
        PASSES.with(|passes| passes.set(0));
        INSTALLED.with(|installed| installed.set(0));
    }
}

/// Test seam for O06: counts the GPU images the viewer path released through
/// `Window::drop_image`. Dropping our `Arc` alone would make a close test pass
/// even if the renderer never learned about the surface, so the close test
/// asserts this counter in addition to the refcount.
#[cfg(test)]
pub(crate) mod image_probe {
    use std::cell::Cell;

    thread_local! {
        static DROPS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn dropped() {
        DROPS.with(|drops| drops.set(drops.get() + 1));
    }

    pub(crate) fn drops() -> usize {
        DROPS.with(Cell::get)
    }

    pub(crate) fn reset() {
        DROPS.with(|drops| drops.set(0));
    }
}

#[cfg(test)]
fn note_scale_buffer(pixels: usize, copied: bool) {
    scale_probe::observe(pixels, copied);
}

#[cfg(not(test))]
fn note_scale_buffer(_pixels: usize, _copied: bool) {}

#[cfg(test)]
fn note_scale_installed() {
    scale_probe::install();
}

#[cfg(not(test))]
fn note_scale_installed() {}

#[cfg(test)]
fn note_dropped_image() {
    image_probe::dropped();
}

#[cfg(not(test))]
fn note_dropped_image() {}

impl Voxhold {
    /// Whether a thumbnail may still be installed. The access scope, the
    /// deletion tombstone and the row's own `deleted` flag all win over a
    /// background answer, so a late picture is never resurrected.
    fn thumbnail_stale(&self, access: u64, id: &str) -> bool {
        self.access_generation != access
            || self.media.is_purged(id)
            || self.messages.iter().any(|message| {
                message
                    .attachments
                    .iter()
                    .any(|file| file.id == id && file.deleted)
            })
    }

    /// Request one chat thumbnail. The URL depends only on the attachment id,
    /// so at most one download per id runs; every 16 px bucket is resized
    /// locally from the cached source. Three downloads at a time, and a failed
    /// id waits 15 s after `429` and 5 s otherwise (doubled on a repeat).
    pub(super) fn request_thumbnail(
        &mut self,
        key: ThumbKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        if self.media.thumb_ready(&key)
            || self.media.thumb_building.contains_key(&key)
            || self.media.thumb_waiting.contains(&key)
            || self.media.thumb_blocked(&key.id, now)
            || self.media.is_purged(&key.id)
        {
            return;
        }
        let source_key = ThumbSourceKey(key.id.clone());
        // A decoded source already serves this bucket: resize here, no HTTP.
        if let Some(source) = self.media.thumb_source(&key.id) {
            self.build_thumbnail_variant(key, source, window, cx);
            return;
        }
        if self.media.thumb_loads.contains_key(&source_key) {
            // Same id is downloading. Remember the variant; the completion
            // redraw asks the next frame to build it from the cached source.
            self.media.thumb_waiting.insert(key);
            return;
        }
        if self.media.thumb_loads.len() >= media::THUMB_CONCURRENCY {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let access = self.access_generation;
        let id = key.id.clone();
        let request_id = id.clone();
        let trigger = key.clone();
        let runtime = self.runtime.clone();
        let load = self.media.begin_thumb_load(&key);
        let task = cx.background_spawn(async move {
            runtime.block_on(async move { client.attachment_thumbnail(&request_id).await })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if !s.media.finish_thumb_load(&id, load) || s.thumbnail_stale(access, &id) {
                    return;
                }
                match result {
                    Ok(image) => {
                        let bytes = media::source_bytes(&image);
                        let source = Arc::new(image);
                        // A source has no GPU surface, so evicted pixels are
                        // released by dropping the `Arc` alone.
                        drop(s.media.thumb_sources.put(source_key, source.clone(), bytes));
                        s.media.thumb_succeeded(&id);
                        // Build the variant that started the download now;
                        // the redraw builds the other visible buckets.
                        s.build_thumbnail_variant(trigger, source, window, cx);
                    }
                    Err(error) => s.media.thumb_retry(&id, error.status()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Resize one cached server thumbnail into a draw bucket in the
    /// background. The source is shared by `Arc`, so no decoded copy is made;
    /// only the target-sized BGRA buffer is allocated. Installing checks the
    /// same access and deletion guards as a download answer.
    fn build_thumbnail_variant(
        &mut self,
        key: ThumbKey,
        source: Arc<image::DynamicImage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.media.thumb_ready(&key) || self.media.thumb_building.contains_key(&key) {
            return;
        }
        let access = self.access_generation;
        let id = key.id.clone();
        let (width, height) = (key.width, key.height);
        let build = self.media.begin_thumb_build(&key);
        let task = cx.background_spawn(async move { thumbnail_frame(&source, width, height) });
        cx.spawn_in(window, async move |this, window| {
            let (frame, bytes) = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if !s.media.finish_thumb_build(&key, build) || s.thumbnail_stale(access, &id) {
                    return;
                }
                let render = Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
                for evicted in s.media.thumbs.put(key.clone(), render, bytes) {
                    let _ = window.drop_image(evicted);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Request metadata and, when present, the image of one link card.
    pub(super) fn request_preview(
        &mut self,
        href: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        if self.media.preview_entry(&href).is_some()
            || self.media.preview_pending.contains_key(&href)
            || self.media.preview_pending.len() >= PREVIEW_CONCURRENCY
            || self.media.preview_blocked(&href, now)
            || self.media.preview_permanent_failure(&href)
        {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let access = self.access_generation;
        let requested = href.clone();
        let runtime = self.runtime.clone();
        let request = uuid::Uuid::new_v4();
        self.media.preview_pending.insert(href.clone(), request);
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let meta = super::preview_ops::load_preview(&client, &requested).await?;
                let image = if meta.image_path.is_empty() {
                    None
                } else {
                    client
                        .link_preview_image(&meta.image_path)
                        .await
                        .ok()
                        .map(|image| {
                            thumbnail_frame(&image, media::PREVIEW_IMAGE.0, media::PREVIEW_IMAGE.1)
                        })
                };
                Ok::<_, Option<u16>>((meta, image))
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.media.preview_pending.get(&href) != Some(&request) {
                    return;
                }
                s.media.preview_pending.remove(&href);
                if s.access_generation != access {
                    return;
                }
                match result {
                    Ok((meta, image)) => {
                        let released = s.media.put_preview(
                            &href,
                            PreviewEntry {
                                meta,
                                retry_at: None,
                                failed: false,
                            },
                        );
                        if let Some(released) = released {
                            let _ = window.drop_image(released);
                        }
                        if let Some((frame, bytes)) = image {
                            let render =
                                Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
                            for evicted in s.media.preview_images.put(href.clone(), render, bytes) {
                                let _ = window.drop_image(evicted);
                            }
                        }
                    }
                    Err(status) => {
                        // Keep the HTTP status: a 400 never retries, a
                        // transient failure retries after the Go gate.
                        let already = s.media.preview_failures.contains_key(&href);
                        let retry_at = media::preview_retry(status, already)
                            .map(|delay| Instant::now() + delay);
                        let mut meta = voxhold_api::LinkPreview {
                            url: href.clone(),
                            unavailable: true,
                            ..Default::default()
                        };
                        meta.site_name = url::Url::parse(&href)
                            .ok()
                            .and_then(|url| url.host_str().map(str::to_owned))
                            .unwrap_or_default();
                        meta.title = meta.site_name.clone();
                        let released = s.media.put_preview(
                            &href,
                            PreviewEntry {
                                meta,
                                retry_at,
                                failed: true,
                            },
                        );
                        if let Some(released) = released {
                            let _ = window.drop_image(released);
                        }
                    }
                }
                // The next frame re-evaluates the visible cards and schedules
                // the one retry timer from their remaining gates.
                cx.notify();
            });
        })
        .detach();
    }

    /// Open a link from a card in the system browser; a failure stays in the
    /// chat error line like Go `View.OpenURL`.
    pub(super) fn open_link(&mut self, href: &str, cx: &mut Context<Self>) {
        if crate::open_url::open(href).is_err() {
            self.error = "Не удалось открыть ссылку.".into();
        }
        cx.notify();
    }

    /// Remove every cached picture of a deleted attachment, close the viewer
    /// if it shows that file and drop it from the save panel.
    pub(super) fn purge_attachments(
        &mut self,
        ids: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut history_stale = false;
        for id in ids {
            let (images, evicted) = self.media.remove_attachment(id);
            history_stale |= evicted;
            for image in images {
                let _ = window.drop_image(image);
            }
            if self
                .media
                .viewer
                .as_ref()
                .is_some_and(|viewer| viewer.key.attachment_id == *id)
            {
                self.close_viewer(window);
            }
        }
        let restart_history = history_stale && self.invalidate_history_requests();
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |panel, cx| panel.remove_message_files(ids, cx));
        }
        restart_history
    }

    /// Schedule one wakeup for the nearest future retry gate of the link cards
    /// that are currently visible. When it fires the next frame re-requests
    /// those links without any user input. Expired gates and off-screen cards
    /// are ignored on purpose: the next frame retries the visible one itself,
    /// and a finished request redraws when all preview slots were busy.
    pub(super) fn ensure_preview_timer(
        &mut self,
        visible: &std::collections::HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        let Some(delay) = self.media.next_preview_retry(visible, now) else {
            return;
        };
        let at = now + delay;
        if self
            .preview_wake_at
            .is_some_and(|scheduled| scheduled <= at)
        {
            return;
        }
        let generation = self.access_generation;
        let timer = cx.background_executor().timer(delay);
        let handle = cx.spawn_in(window, async move |this, window| {
            timer.await;
            let _ = this.update_in(window, |s, _window, cx| {
                if s.access_generation != generation {
                    return;
                }
                s.preview_wake_at = None;
                cx.notify();
            });
        });
        self.preview_wakeup = Some(handle);
        self.preview_wake_at = Some(at);
    }

    /// Open the original picture on click. Reuses the single viewer and
    /// cancels the previous request.
    pub(super) fn open_viewer(
        &mut self,
        message_id: i64,
        file: &Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if file.deleted || self.media.is_purged(&file.id) {
            return;
        }
        if let Some(viewer) = &self.media.viewer {
            if viewer.key.attachment_id == file.id && viewer.key.chat == self.chat_key() {
                return;
            }
        }
        self.close_viewer(window);
        let Some(client) = self.client.clone() else {
            return;
        };
        self.media.viewer_seq = self.media.viewer_seq.wrapping_add(1);
        let key = ViewerKey {
            chat: self.chat_key(),
            access: self.access_generation,
            message_id,
            attachment_id: file.id.clone(),
            open_id: self.media.viewer_seq,
        };
        self.media.viewer = Some(Viewer::new(key));
        let id = file.id.clone();
        let runtime = self.runtime.clone();
        let open_id = self.media.viewer_seq;
        let area = media::viewer_area(self.viewport);
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let source = client.attachment_content_image(&id).await?;
                let pixels = u64::from(source.width()) * u64::from(source.height());
                if source.width() > media::VIEWER_MAX_EDGE
                    || source.height() > media::VIEWER_MAX_EDGE
                    || pixels > media::VIEWER_MAX_PIXELS
                {
                    return Err(voxhold_api::Error::InvalidResponse(
                        "Изображение слишком большое для просмотра.",
                    ));
                }
                let target = media::fit_size((source.width(), source.height()), area);
                // The pass borrows the one shared original: opening copies the
                // `Arc` pointer, never the decoded pixels (O06).
                let source = Arc::new(source);
                let (frame, bytes) = thumbnail_frame(&source, target.0, target.1);
                Ok::<_, voxhold_api::Error>((source, frame, bytes, target))
            })
        });
        let handle = cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, _window, cx| {
                let Some(viewer) = s.media.viewer.as_mut() else {
                    return;
                };
                if viewer.key.open_id != open_id {
                    return;
                }
                viewer.loading = false;
                match result {
                    Ok((source, frame, _bytes, target)) => {
                        viewer.image =
                            Some(Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame])));
                        viewer.source = Some(source);
                        viewer.render_box = target;
                        viewer.target_box = target;
                    }
                    Err(error) => viewer.error = draft::describe_error(&error),
                }
                cx.notify();
            });
        });
        if let Some(viewer) = self.media.viewer.as_mut() {
            viewer.task = Some(handle);
        }
        cx.notify();
    }

    /// Close the viewer and release its GPU surface. Idempotent; dropping the
    /// stored handles also aborts the pending content request and a running
    /// rescale pass, so neither can install a frame into the next viewer.
    pub(super) fn close_viewer(&mut self, window: &mut Window) {
        let Some(mut viewer) = self.media.viewer.take() else {
            return;
        };
        viewer.task.take();
        // Dropping the scale handle cancels the background pass; the decoded
        // original it borrowed is released with the cancelled future.
        viewer.scale_task.take();
        if let Some(image) = viewer.image.take() {
            note_dropped_image();
            let _ = window.drop_image(image);
        }
        viewer.source = None;
    }

    /// Rescale the single viewer surface when the window changes, keeping the
    /// decoded source and the 80% fit. Only one background pass runs at a
    /// time; a drag updates the remembered geometry and the running pass
    /// starts exactly one follow-up for the newest box.
    pub(super) fn ensure_viewer_scale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = self.media.viewer.as_ref() else {
            return;
        };
        let Some(source) = viewer.source.clone() else {
            return;
        };
        let target = media::fit_size(
            (source.width(), source.height()),
            media::viewer_area(self.viewport),
        );
        let Some(viewer) = self.media.viewer.as_mut() else {
            return;
        };
        let Some(target) = viewer.request_scale(target) else {
            return;
        };
        self.start_viewer_scale(target, source, window, cx);
    }

    /// One background pass for one requested surface size. It borrows the
    /// shared original and returns only the frame; the caller installs it and
    /// asks the viewer state machine for a single follow-up.
    fn start_viewer_scale(
        &mut self,
        target: (u32, u32),
        source: Arc<image::DynamicImage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open_id) = self.media.viewer.as_ref().map(|viewer| viewer.key.open_id) else {
            return;
        };
        // One pass at a time: dropping the previous handle cancels an older
        // background resize, so the stale pass cannot keep the decoded
        // original alive next to the new one.
        if let Some(viewer) = self.media.viewer.as_mut() {
            viewer.scale_task = None;
        }
        #[cfg(test)]
        scale_probe::pass();
        let task = cx.background_spawn(async move { thumbnail_frame(&source, target.0, target.1) });
        let handle = cx.spawn_in(window, async move |this, window| {
            let (frame, _bytes) = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                // A cancelled pass is dropped before this point and never
                // reaches the UI half.
                note_scale_installed();
                let Some(viewer) = s.media.viewer.as_mut() else {
                    return;
                };
                if viewer.key.open_id != open_id {
                    return;
                }
                let next = Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
                if let Some(previous) = std::mem::replace(&mut viewer.image, Some(next)) {
                    note_dropped_image();
                    let _ = window.drop_image(previous);
                }
                let follow_up = viewer.finish_scale(target);
                let source = follow_up.and_then(|_| viewer.source.clone());
                cx.notify();
                if let (Some(target), Some(source)) = (follow_up, source) {
                    s.start_viewer_scale(target, source, window, cx);
                }
            });
        });
        if let Some(viewer) = self.media.viewer.as_mut() {
            if viewer.key.open_id == open_id {
                viewer.scale_task = Some(handle);
            }
        }
    }

    /// Close the viewer and release every cached private picture. Used when
    /// the access scope changes or the session ends.
    pub(super) fn clear_media(&mut self, window: &mut Window) {
        self.preview_wakeup = None;
        self.preview_wake_at = None;
        for image in self.media.clear() {
            let _ = window.drop_image(image);
        }
    }

    /// The viewer overlay: a `#060606` layer at 0.82 alpha with the picture
    /// centred at 80% of the window. Esc or any click closes it.
    pub(super) fn viewer_layer(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let viewer = self.media.viewer.as_ref()?;
        let p = self.palette;
        let area = media::viewer_area(self.viewport);
        let picture = viewer.image.clone();
        let (target_w, target_h) = viewer.render_box;
        let loading = viewer.loading;
        let error = viewer.error.clone();
        let mut content = div()
            .w(px(area.0))
            .h(px(area.1))
            .flex()
            .items_center()
            .justify_center()
            .flex_col()
            .gap_2();
        if let Some(image) = picture {
            content = content.child(
                div().w(px(target_w as f32)).h(px(target_h as f32)).child(
                    gpui::img(image)
                        .size_full()
                        .object_fit(gpui::ObjectFit::Contain),
                ),
            );
        }
        if loading {
            content = content.child(
                div()
                    .text_size(px(14.))
                    .text_color(ink(p.muted))
                    .child("Загрузка изображения…"),
            );
        }
        if !error.is_empty() {
            content = content.child(
                div()
                    .max_w(px(area.0 - 80.))
                    .text_center()
                    .text_size(px(14.))
                    .text_color(ink(p.muted))
                    .child(error),
            );
            content = content.child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Нажмите Esc, чтобы вернуться в чат"),
            );
        }
        Some(
            div()
                .id("image-viewer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x060606d1))
                .on_click(cx.listener(|s, _, window, cx| {
                    s.close_viewer(window);
                    cx.notify();
                }))
                .child(content)
                .into_any_element(),
        )
    }
}
