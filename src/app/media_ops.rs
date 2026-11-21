//! Chat media lifecycle: one bounded thumbnail cache, link previews and the
//! single image viewer. Every background result is checked against the chat,
//! the access generation and the request id before it touches the UI, so a
//! late answer can never reopen a closed viewer or show a channel the user
//! left.

use super::*;
use crate::media::{self, PreviewEntry, ThumbKey, Viewer, ViewerKey};
use std::sync::Arc;
use std::time::Instant;
use voxhold_api::Attachment;

/// Go `previewConcurrency`; a message with two links never floods the queue.
const PREVIEW_CONCURRENCY: usize = 3;
/// Preview card picture, Go `scaleToFit(img, 98, 76)`.

/// Resize a server thumbnail into the requested 16 px bucket and convert it to
/// the BGRA surface GPUI uploads.
pub(crate) fn thumbnail_frame(
    image: image::DynamicImage,
    width: u32,
    height: u32,
) -> (image::Frame, usize) {
    let rgba = image::imageops::resize(
        &image.to_rgba8(),
        width.max(1),
        height.max(1),
        image::imageops::FilterType::CatmullRom,
    );
    let bytes = rgba.width() as usize * rgba.height() as usize * 4;
    (
        members::avatar_frame(image::DynamicImage::ImageRgba8(rgba)),
        bytes,
    )
}

impl Voxhold {
    /// Request one chat thumbnail. Three reads at a time, a failed key waits
    /// 15 s after `429` and 5 s otherwise (doubled after a second failure).
    pub(super) fn request_thumbnail(
        &mut self,
        key: ThumbKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        if self.media.thumb_ready(&key)
            || self.media.thumb_pending.contains(&key)
            || self.media.thumb_pending.len() >= media::THUMB_CONCURRENCY
            || self.media.thumb_blocked(&key, now)
            || self.media.is_purged(&key.id)
        {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let access = self.access_generation;
        let id = key.id.clone();
        let (width, height) = (key.width, key.height);
        let runtime = self.runtime.clone();
        self.media.thumb_pending.insert(key.clone());
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let image = client.attachment_thumbnail(&id).await?;
                Ok::<_, voxhold_api::Error>(thumbnail_frame(image, width, height))
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.media.thumb_pending.remove(&key);
                if s.access_generation != access {
                    return;
                }
                // The attachment may have been deleted while the thumbnail was
                // in flight; do not resurrect it after the purge.
                if s.media.is_purged(&key.id)
                    || s.messages.iter().any(|message| {
                        message
                            .attachments
                            .iter()
                            .any(|file| file.id == key.id && file.deleted)
                    })
                {
                    return;
                }
                match result {
                    Ok((frame, bytes)) => {
                        let render = Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
                        for evicted in s.media.thumbs.put(key.clone(), render, bytes) {
                            let _ = window.drop_image(evicted);
                        }
                        s.media.thumb_succeeded(&key);
                    }
                    Err(error) => s.media.thumb_retry(&key, error.status()),
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
                            thumbnail_frame(image, media::PREVIEW_IMAGE.0, media::PREVIEW_IMAGE.1)
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
                let (frame, bytes) = thumbnail_frame(source.clone(), target.0, target.1);
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
                        viewer.source = Some(Arc::new(source));
                        viewer.render_box = target;
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

    /// Close the viewer and release its GPU surface. Idempotent; also aborts
    /// the pending content request.
    pub(super) fn close_viewer(&mut self, window: &mut Window) {
        let Some(mut viewer) = self.media.viewer.take() else {
            return;
        };
        viewer.task.take();
        if let Some(image) = viewer.image.take() {
            let _ = window.drop_image(image);
        }
        viewer.source = None;
    }

    /// Rescale the single viewer surface when the window changes, keeping the
    /// decoded source and the 80% fit.
    pub(super) fn ensure_viewer_scale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = self.media.viewer.as_ref() else {
            return;
        };
        let Some(source) = viewer.source.clone() else {
            return;
        };
        if viewer.rescaling {
            return;
        }
        let target = media::fit_size(
            (source.width(), source.height()),
            media::viewer_area(self.viewport),
        );
        let current = viewer.render_box;
        if (i64::from(target.0) - i64::from(current.0)).abs() <= 16
            && (i64::from(target.1) - i64::from(current.1)).abs() <= 16
        {
            return;
        }
        let open_id = viewer.key.open_id;
        if let Some(viewer) = self.media.viewer.as_mut() {
            viewer.rescaling = true;
        }
        let task =
            cx.background_spawn(
                async move { thumbnail_frame((*source).clone(), target.0, target.1) },
            );
        cx.spawn_in(window, async move |this, window| {
            let (frame, _bytes) = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                let Some(viewer) = s.media.viewer.as_mut() else {
                    return;
                };
                if viewer.key.open_id != open_id {
                    return;
                }
                viewer.rescaling = false;
                let next = Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
                if let Some(previous) = std::mem::replace(&mut viewer.image, Some(next)) {
                    let _ = window.drop_image(previous);
                }
                viewer.render_box = target;
                cx.notify();
            });
        })
        .detach();
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
