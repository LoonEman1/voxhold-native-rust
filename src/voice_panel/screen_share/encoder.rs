//! Encoder backend selector and the GPU probe of the screen dialog.
//!
//! «Процессор» is the default and needs no probe. «Видеокарта» starts the
//! bounded [`EncoderProbeService`] off the UI thread; until it answers, the
//! explicit codecs are painted as checking and Start is refused. The answer is
//! an immutable snapshot: the request carries its generation and the adapter
//! profile, and the engine revalidates that exact pair on the media worker
//! before `stream.start`.

use super::*;
use std::time::Duration;
use voxhold_media::video_encoder::{EncoderProbeService, EncoderProbeTarget, EncoderRequest};

impl ScreenShareUi {
    pub(super) fn resolved_codec(&self) -> Result<Codec, String> {
        #[cfg(test)]
        if let Some(codec) = self.test_codec {
            return Ok(codec);
        }
        #[cfg(test)]
        if let Some(entries) = &self.test_codec_availability {
            let reason = |codec: Codec| {
                entries
                    .iter()
                    .find(|(entry, _)| *entry == codec)
                    .map(|(_, reason)| reason.clone())
                    .unwrap_or_else(|| Some("Кодек не описан в тестовом наборе.".to_owned()))
            };
            return match self.codec {
                CodecIntent::Auto => entries
                    .iter()
                    .find(|(_, reason)| reason.is_none())
                    .map(|(codec, _)| *codec)
                    .ok_or_else(|| {
                        "В этой сборке нет доступного SDR-кодека для публикации.".to_owned()
                    }),
                CodecIntent::Explicit(codec) => match reason(codec) {
                    None => Ok(codec),
                    Some(reason) => Err(reason),
                },
            };
        }
        if self.encoder_backend == EncoderBackend::Gpu {
            let Some(snapshot) = self.encoder_probe.as_ref() else {
                return Err("Проверяем видеокарту…".to_owned());
            };
            return snapshot
                .resolve(self.codec)
                .map(|resolved| resolved.codec)
                .map_err(|rejection| rejection.to_string());
        }
        CodecSnapshot::current()
            .resolve(self.codec, CodecRole::Sender)
            .map_err(|rejection| rejection.reason().to_owned())
    }

    /// Sender availability of one codec exactly as the selector paints it:
    /// `Ok(())` when this build can encode it, otherwise the probe's reason.
    pub(super) fn codec_availability(&self, codec: Codec) -> Result<(), String> {
        #[cfg(test)]
        if let Some(entries) = &self.test_codec_availability {
            return entries
                .iter()
                .find(|(entry, _)| *entry == codec)
                .map(|(_, reason)| reason.clone().map_or(Ok(()), Err))
                .unwrap_or_else(|| Err("Кодек не описан в тестовом наборе.".to_owned()));
        }
        if self.encoder_backend == EncoderBackend::Gpu {
            let Some(snapshot) = self.encoder_probe.as_ref() else {
                return Err("Проверяем видеокарту…".to_owned());
            };
            let Some(slot) = snapshot.gpu(codec) else {
                return Err("Кодек не описан в снимке видеокарты.".to_owned());
            };
            return match (slot.is_available(), slot.reason()) {
                (true, _) => Ok(()),
                (false, Some(reason)) => Err(reason.to_string()),
                (false, None) => Err("Кодек не подтверждён видеокартой.".to_owned()),
            };
        }
        let availability = CodecSnapshot::current().availability(CodecRole::Sender, codec);
        match availability.reason() {
            None => Ok(()),
            Some(reason) => Err(reason.to_owned()),
        }
    }

    /// The concrete encoder a Start would carry. CPU is the default; an
    /// explicit GPU choice without a ready snapshot is refused instead of
    /// silently publishing on the processor.
    pub(super) fn encoder_plan(&self, codec: Codec) -> Result<EncoderPlan, String> {
        if self.encoder_backend != EncoderBackend::Gpu {
            return Ok(EncoderPlan::cpu());
        }
        let Some(snapshot) = self.encoder_probe.as_ref() else {
            return Err("Проверяем видеокарту…".to_owned());
        };
        let resolved = snapshot
            .resolve(self.codec)
            .map_err(|rejection| rejection.to_string())?;
        if resolved.codec != codec {
            return Err("Выбранный кодек не совпал с проверенным видеокартой.".to_owned());
        }
        let Some(adapter) = resolved.adapter.clone() else {
            return Err("Видеокарта не подтвердила адаптер.".to_owned());
        };
        Ok(EncoderPlan::gpu(
            adapter,
            resolved.profile,
            snapshot.generation(),
        ))
    }
}

impl VoicePanel {
    /// «Процессор» / «Видеокарта». Switching to the GPU starts a new probe;
    /// switching back keeps the previous answer only as a label, never as a
    /// launch permit.
    pub(super) fn select_encoder_backend(
        &mut self,
        backend: EncoderBackend,
        cx: &mut Context<Self>,
    ) {
        if self.screen.encoder_backend == backend {
            return;
        }
        self.screen.encoder_backend = backend;
        self.screen.encoder_probe = None;
        self.screen.encoder_probe_generation = self.screen.encoder_probe_generation.wrapping_add(1);
        if backend == EncoderBackend::Gpu {
            self.begin_encoder_probe(cx);
        }
        // A codec that only the other backend provides is re-resolved by the
        // selector; an explicit unavailable item stays explicit and blocked.
        cx.notify();
    }

    /// One backend row: the two choices, the resolved adapter name and the
    /// reason when the GPU path is unavailable.
    pub(super) fn encoder_backend_row(
        &self,
        ring: &mut Vec<(FocusHandle, ShareKey)>,
        row: &mut dyn FnMut(&Self, &mut Vec<(FocusHandle, ShareKey)>, ShareKey) -> FocusHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let selected = self.screen.encoder_backend;
        let mut options = div().flex().items_center().gap_2();
        for (backend, label) in [
            (EncoderBackend::Cpu, "Процессор"),
            (EncoderBackend::Gpu, "Видеокарта"),
        ] {
            let key = ShareKey::EncoderBackend(backend);
            let focus = row(self, ring, key);
            options = options.child(self.choice_enabled(
                label,
                selected == backend,
                true,
                22.,
                focus,
                key,
                cx,
            ));
        }
        let (label, refused) = match selected {
            EncoderBackend::Cpu => (
                "Кодирование: Процессор · программный WebRTC".to_owned(),
                false,
            ),
            EncoderBackend::Gpu => match self.screen.encoder_probe.as_ref() {
                None => ("Кодирование: Видеокарта · проверяем…".to_owned(), false),
                Some(snapshot) => {
                    let adapter = snapshot
                        .adapter()
                        .map(|adapter| {
                            format!(
                                "Кодирование: Видеокарта · {} · драйвер 0x{:x}",
                                adapter.name, adapter.driver_version
                            )
                        })
                        .unwrap_or_else(|| {
                            "Кодирование: Видеокарта · нет аппаратного адаптера".to_owned()
                        });
                    (adapter, !snapshot.has_hardware_adapter())
                }
            },
        };
        let reason = match selected {
            EncoderBackend::Cpu => "SDR · программное кодирование".to_owned(),
            EncoderBackend::Gpu => match self.screen.encoder_probe.as_ref() {
                None => "Проверяем видеокарту и кодировщики…".to_owned(),
                Some(snapshot) => match snapshot.adapter() {
                    Some(adapter) => format!(
                        "Аппаратный MFT · {}{}",
                        adapter.name,
                        if snapshot.gpu_available() {
                            ""
                        } else {
                            " · кодировщик недоступен"
                        }
                    ),
                    None => {
                        "Аппаратное кодирование недоступно: нет подходящей видеокарты".to_owned()
                    }
                },
            },
        };
        let label_selector = format!("share-encoder-label:{label}");
        let reason_selector = format!("share-encoder-reason:{reason}");
        div()
            .debug_selector(|| "share-encoder".into())
            .flex()
            .items_start()
            .gap_2()
            .mb(px(2.))
            .child(
                div()
                    .w(px(92.))
                    .flex_shrink_0()
                    .pt(px(4.))
                    .text_size(px(12.))
                    .text_color(rgb(c.muted))
                    .child("Кодирование"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(options)
                    .child(
                        div()
                            .debug_selector(move || label_selector.clone().into())
                            .text_size(px(10.))
                            .child(label),
                    )
                    .child(
                        div()
                            .debug_selector(move || reason_selector.clone().into())
                            .text_size(px(10.))
                            .text_color(rgb(if refused { c.danger } else { c.muted }))
                            .truncate()
                            .child(reason),
                    ),
            )
            .into_any_element()
    }

    /// Probe the selected adapter/codec pair off the UI thread. The answer is
    /// dropped when the dialog moved on (another generation or CPU mode).
    ///
    /// Every call owns a fresh [`EncoderProbeService`], so the snapshot
    /// generation starts over on each probe. The generation binds the answer to
    /// this dialog; the live-change comparison uses the checked pair
    /// (`EncoderPlan::same_pair`), never the generation, so re-opening the
    /// dialog cannot turn an unchanged GPU request into a restart.
    pub(super) fn begin_encoder_probe(&mut self, cx: &mut Context<Self>) {
        let generation = self.screen.encoder_probe_generation;
        let preference = self.screen.encoder_preference();
        let query = self.screen.encoder_query();
        let target = EncoderProbeTarget::default();
        let task = cx.background_spawn(async move {
            let service = EncoderProbeService::platform().ok()?;
            let requested = service
                .set_video_encoder_backend(EncoderRequest::new(preference, query, target))
                .ok()?;
            let snapshot = service.wait_for(requested, Duration::from_secs(60));
            service.shutdown();
            snapshot
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |panel, cx| {
                if panel.screen.encoder_probe_generation != generation
                    || panel.screen.encoder_backend != EncoderBackend::Gpu
                {
                    return;
                }
                panel.screen.encoder_probe = result;
                cx.notify();
            });
        })
        .detach();
    }

    /// «Продолжить на процессоре» of a failed GPU attempt. Only this explicit
    /// click starts the CPU attempt; the codec is unchanged.
    pub(in crate::voice_panel) fn continue_screen_on_cpu(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = &self.handle {
            handle.continue_screen_on_cpu();
        }
        self.screen.error.clear();
        self.screen.busy = true;
        cx.notify();
    }
}
