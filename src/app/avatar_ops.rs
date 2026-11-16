//! Avatar loading, cropping and upload operations.

use super::*;
use crate::settings;

impl Voxhold {
    pub(super) fn load_personal_avatar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(panel) = &mut self.personal else {
            return;
        };
        let Some(version) = panel.avatar_version.clone() else {
            if let Some(image) = panel.avatar.take() {
                let _ = window.drop_image(image);
            }
            panel.avatar_missing = true;
            return;
        };
        if panel.avatar_loading || self.self_user_id == 0 {
            return;
        }
        let token = panel.dialog.begin();
        panel.avatar_loading = true;
        let user = self.self_user_id;
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.avatar(user, &version))
                .map_err(|error| error)
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ =
                this.update_in(window, |s, window, cx| {
                    if s.session_generation != session || !s.personal_settle(token) {
                        return;
                    }
                    let mut dropped = None;
                    if let Some(panel) = &mut s.personal {
                        panel.avatar_loading = false;
                        match result {
                            Ok(image) => {
                                let frame = crate::members::avatar_frame(image);
                                dropped = panel.avatar.replace(std::sync::Arc::new(
                                    gpui::RenderImage::new(smallvec::smallvec![frame]),
                                ));
                                panel.avatar_missing = false;
                            }
                            Err(voxhold_api::Error::Http(404)) => {
                                dropped = panel.avatar.take();
                                panel.avatar_missing = true;
                            }
                            Err(error) => panel.local_error = error.to_string(),
                        }
                    }
                    if let Some(image) = dropped {
                        let _ = window.drop_image(image);
                    }
                    cx.notify();
                });
        })
        .detach();
        cx.notify();
    }
}

impl Voxhold {
    pub(super) fn pick_avatar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(generation) = self.personal.as_ref().map(|panel| panel.dialog.generation) else {
            return;
        };
        let session = self.session_generation;
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Выбрать аватар".into()),
        });
        cx.spawn_in(window, async move |this, window| {
            let path = match prompt.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = path else { return };
            let _ = this.update_in(window, move |s, window, cx| {
                let Some(panel) = &mut s.personal else { return };
                if s.session_generation != session
                    || panel.dialog.generation != generation
                    || s.active_settings != Some(SettingsKind::Personal)
                    || panel.tab() != 0
                {
                    return;
                }
                panel.avatar_pick = panel.avatar_pick.wrapping_add(1);
                let selection = panel.avatar_pick;
                let path = path.clone();
                // Reading, EXIF rotation and downscaling happen off the UI
                // thread, exactly like the crop preview.
                let task =
                    cx.background_spawn(async move { super::avatar_ops::decode_avatar(&path) });
                cx.spawn_in(window, async move |this, window| {
                    let result = task.await;
                    let _ = this.update_in(window, |s, window, cx| {
                        s.apply_avatar_decoded(session, generation, selection, result, window, cx);
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    /// Apply a finished decode only when it still belongs to the current
    /// session, dialog and file choice.
    pub(super) fn apply_avatar_decoded(
        &mut self,
        session: u64,
        generation: u64,
        selection: u64,
        result: Result<
            (
                std::sync::Arc<Vec<u8>>,
                std::sync::Arc<image::DynamicImage>,
                u32,
                u32,
            ),
            String,
        >,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session_generation != session
            || self.active_settings != Some(SettingsKind::Personal)
        {
            return;
        }
        let Some(panel) = &mut self.personal else {
            return;
        };
        if panel.dialog.generation != generation
            || panel.tab() != 0
            || panel.avatar_pick != selection
        {
            return;
        }
        let mut refresh = false;
        match result {
            Ok((data, source, width, height)) => {
                panel.crop = Some(crate::personal_settings::CropDraft {
                    data,
                    source,
                    pick: selection,
                    width,
                    height,
                    crop: settings::Crop::default(),
                    preview: None,
                    preview_token: 1,
                });
                panel.local_error.clear();
                refresh = true;
            }
            Err(error) => panel.local_error = error,
        }
        if refresh {
            self.refresh_crop_preview(window, cx);
        }
        cx.notify();
    }

    fn refresh_crop_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.personal else { return };
        let Some(draft) = &panel.crop else { return };
        let token = draft.preview_token;
        let source = draft.source.clone();
        let crop = draft.crop;
        let (width, height) = (draft.width, draft.height);
        let generation = panel.dialog.generation;
        // The pixels and the file id must come from the same draft. Reading the
        // panel-wide `avatar_pick` here would pair draft A's pixels with a newer
        // selection B when the slider moves before B is decoded.
        let pick = draft.pick;
        let task = cx.background_spawn(async move {
            let (x, y) = crop.origin(width, height);
            let side = crop.size(width, height).max(1);
            let source_width = source.width().max(1);
            let source_height = source.height().max(1);
            let left = (u64::from(x) * u64::from(source_width) / u64::from(width.max(1))) as u32;
            let top = (u64::from(y) * u64::from(source_height) / u64::from(height.max(1))) as u32;
            let right = ((u64::from(x + side) * u64::from(source_width) / u64::from(width.max(1)))
                as u32)
                .max(left + 1)
                .min(source_width);
            let bottom = ((u64::from(y + side) * u64::from(source_height)
                / u64::from(height.max(1))) as u32)
                .max(top + 1)
                .min(source_height);
            let image = source
                .crop_imm(
                    left,
                    top,
                    right.saturating_sub(left),
                    bottom.saturating_sub(top),
                )
                .resize_exact(120, 120, image::imageops::FilterType::CatmullRom);
            std::sync::Arc::new(gpui::RenderImage::new(smallvec::smallvec![
                crate::members::avatar_frame(image)
            ]))
        });
        cx.spawn_in(window, async move |this, window| {
            let image = task.await;
            let _ = this.update_in(window, |s, _window, cx| {
                s.apply_crop_preview(generation, pick, token, image, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Apply a finished crop preview only to the draft that requested it. The
    /// file choice is checked as well as the dialog generation and the slider
    /// token, because a new draft starts again at `preview_token = 1`.
    pub(super) fn apply_crop_preview(
        &mut self,
        generation: u64,
        pick: u64,
        token: u64,
        image: std::sync::Arc<gpui::RenderImage>,
        cx: &mut Context<Self>,
    ) {
        if self.active_settings != Some(SettingsKind::Personal) {
            return;
        }
        let Some(panel) = &mut self.personal else {
            return;
        };
        if panel.dialog.generation != generation || panel.avatar_pick != pick {
            return;
        }
        let Some(draft) = &mut panel.crop else {
            return;
        };
        if draft.preview_token != token {
            return;
        }
        draft.preview = Some(image);
        cx.notify();
    }

    pub(super) fn edit_crop(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        axis: usize,
        value: i32,
    ) {
        if let Some(panel) = &mut self.personal {
            if let Some(draft) = &mut panel.crop {
                match axis {
                    0 => draft.crop.zoom = value.clamp(0, 100),
                    1 => draft.crop.x = value.clamp(0, 100),
                    _ => draft.crop.y = value.clamp(0, 100),
                }
                draft.preview_token = draft.preview_token.wrapping_add(1);
            }
        }
        self.refresh_crop_preview(window, cx);
    }

    pub(super) fn save_avatar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.personal else { return };
        let Some(draft) = &panel.crop else { return };
        let data = draft.data.clone();
        let crop = draft.upload_crop();
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.upload_avatar((*data).clone(), Some(crop)))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.personal_settle(token) {
                    return;
                }
                let mut refresh = false;
                if let Some(panel) = &mut s.personal {
                    match result {
                        Ok(etag) => {
                            panel.crop = None;
                            panel.avatar_version = Some(etag);
                            panel.notice = "Аватар обновлён.".into();
                            panel.local_error.clear();
                            refresh = true;
                        }
                        Err(error) => panel.local_error = error,
                    }
                }
                if refresh {
                    s.load_personal_avatar(window, cx);
                }
                s.refresh_members(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn remove_avatar(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.remove_avatar())
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.personal_settle(token) {
                        return;
                    }
                    if let Some(panel) = &mut s.personal {
                        match result {
                            Ok(()) => {
                                panel.clear_avatar();
                                panel.avatar_missing = true;
                                panel.notice = "Аватар удалён.".into();
                            }
                            Err(error) => panel.local_error = error,
                        }
                    }
                    s.refresh_members(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
}

pub(super) fn decode_avatar(
    path: &std::path::Path,
) -> Result<
    (
        std::sync::Arc<Vec<u8>>,
        std::sync::Arc<image::DynamicImage>,
        u32,
        u32,
    ),
    String,
> {
    const MAX_BYTES: u64 = 5 << 20;
    const MAX_PIXELS: u64 = 16_000_000;
    let invalid = || "Выберите JPEG, PNG, WebP или GIF до 5 МБ и 16 мегапикселей.".to_owned();
    let meta = std::fs::metadata(path).map_err(|_| invalid())?;
    if meta.len() == 0 || meta.len() > MAX_BYTES {
        return Err(invalid());
    }
    let data = std::fs::read(path).map_err(|_| invalid())?;
    let reader = image::ImageReader::new(std::io::Cursor::new(&data))
        .with_guessed_format()
        .map_err(|_| invalid())?;
    let format = reader.format().ok_or_else(invalid)?;
    let (raw_width, raw_height) = reader.into_dimensions().map_err(|_| invalid())?;
    if raw_width == 0
        || raw_height == 0
        || u64::from(raw_width) * u64::from(raw_height) > MAX_PIXELS
    {
        return Err(invalid());
    }
    let image = image::load_from_memory_with_format(&data, format).map_err(|_| invalid())?;
    let orientation = settings::image_orientation(&data);
    let image = settings::orient_image(image, orientation);
    let (width, height) = (image.width(), image.height());
    let longest = width.max(height);
    let source = if longest > 640 {
        image.resize(640, 640, image::imageops::FilterType::CatmullRom)
    } else {
        image
    };
    Ok((
        std::sync::Arc::new(data),
        std::sync::Arc::new(source),
        width,
        height,
    ))
}
