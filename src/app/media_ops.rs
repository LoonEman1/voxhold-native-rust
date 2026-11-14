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
const PREVIEW_IMAGE: (u32, u32) = (98, 76);

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
            || self.media.preview_pending.contains(&href)
            || self.media.preview_pending.len() >= PREVIEW_CONCURRENCY
            || self.media.preview_blocked(&href, now)
        {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let access = self.access_generation;
        let requested = href.clone();
        let runtime = self.runtime.clone();
        self.media.preview_pending.insert(href.clone());
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let mut meta = client.link_preview(&requested).await.unwrap_or_default();
                // The server echoes the clicked URL; anything else is stale.
                if meta.url != requested {
                    meta = Default::default();
                }
                let image = if meta.image_path.is_empty() {
                    None
                } else {
                    client
                        .link_preview_image(&meta.image_path)
                        .await
                        .ok()
                        .map(|image| thumbnail_frame(image, PREVIEW_IMAGE.0, PREVIEW_IMAGE.1))
                };
                Ok::<_, voxhold_api::Error>((meta, image))
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.media.preview_pending.remove(&href);
                if s.access_generation != access {
                    return;
                }
                match result {
                    Ok((mut meta, image)) => {
                        let failed = meta.url.is_empty();
                        if failed {
                            meta.url = href.clone();
                            meta.site_name = url::Url::parse(&href)
                                .ok()
                                .and_then(|url| url.host_str().map(str::to_owned))
                                .unwrap_or_default();
                            meta.title = meta.site_name.clone();
                            meta.unavailable = true;
                        }
                        let retry = if failed {
                            media::preview_retry(None, false).map(|delay| Instant::now() + delay)
                        } else {
                            None
                        };
                        let released = s.media.put_preview(
                            &href,
                            PreviewEntry {
                                meta,
                                retry_at: retry,
                                failed,
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
                    Err(_) => {
                        let retry =
                            media::preview_retry(None, true).map(|delay| Instant::now() + delay);
                        s.media.put_preview(
                            &href,
                            PreviewEntry {
                                meta: Default::default(),
                                retry_at: retry,
                                failed: true,
                            },
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
