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
use voxhold_media::video_encoder::{
    EncoderProbeService, EncoderProbeState, EncoderProbeTarget, EncoderRequest,
    VideoEncoderSnapshot,
};

const PROBE_POLL: Duration = Duration::from_millis(50);
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const PROBE_PENDING: &str = "Проверяем видеокарту…";

#[derive(Default)]
pub(super) struct DialogEncoderProbe {
    service: Option<EncoderProbeService>,
    query: Option<EncodeQuery>,
    task: Option<gpui::Task<()>>,
    result: Option<Result<Arc<VideoEncoderSnapshot>, String>>,
}

impl ScreenShareUi {
    pub(super) fn encoder_query(&self) -> EncodeQuery {
        let mut quality = self.quality().clamped();
        let source = self.resolution_source();
        if !self.restart && source.is_some() {
            quality.resolution = quality.resolution.for_source(source);
        }
        EncodeQuery::from_source(quality, source.unwrap_or_default())
    }

    fn gpu_snapshot(&self) -> Result<&VideoEncoderSnapshot, &str> {
        if self.encoder_probe.query != Some(self.encoder_query()) {
            return Err(PROBE_PENDING);
        }
        match &self.encoder_probe.result {
            Some(Ok(snapshot)) => Ok(snapshot),
            Some(Err(reason)) => Err(reason),
            None => Err(PROBE_PENDING),
        }
    }

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
            let snapshot = self.gpu_snapshot().map_err(str::to_owned)?;
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
            let snapshot = self.gpu_snapshot().map_err(str::to_owned)?;
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
        let snapshot = self.gpu_snapshot().map_err(str::to_owned)?;
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
        self.cancel_encoder_probe();
        self.begin_encoder_probe(cx);
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
            EncoderBackend::Gpu => match self.screen.gpu_snapshot() {
                Err(reason) => (
                    "Кодирование: Видеокарта".to_owned(),
                    reason != PROBE_PENDING,
                ),
                Ok(snapshot) => {
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
            EncoderBackend::Gpu => match self.screen.gpu_snapshot() {
                Err(reason) => reason.to_owned(),
                Ok(snapshot) => match snapshot.adapter() {
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

    pub(in crate::voice_panel) fn cancel_encoder_probe(&mut self) {
        self.screen.encoder_probe = DialogEncoderProbe::default();
    }

    /// Probe one exact capture format. A setting change supersedes the pending
    /// request on the same worker, so rapid edits cannot open competing MFTs.
    pub(in crate::voice_panel) fn begin_encoder_probe(&mut self, cx: &mut Context<Self>) {
        if !self.screen.open || self.screen.encoder_backend != EncoderBackend::Gpu {
            return;
        }
        let query = self.screen.encoder_query();
        let probe = &mut self.screen.encoder_probe;
        if probe.query == Some(query) {
            return;
        }
        probe.query = Some(query);
        probe.task = None;
        probe.result = None;
        if let Err(error) = query.validate() {
            if let Some(service) = probe.service.as_ref() {
                service.cancel();
            }
            probe.result = Some(Err(error.to_string()));
            cx.notify();
            return;
        }
        if probe.service.is_none() {
            match EncoderProbeService::platform() {
                Ok(service) => probe.service = Some(service),
                Err(error) => {
                    probe.result = Some(Err(format!("Проверка видеокарты недоступна: {error}")));
                    cx.notify();
                    return;
                }
            }
        }
        let preference = VideoEncoderPreference {
            backend: EncoderBackend::Gpu,
            adapter: self.screen.encoder_adapter,
            allow_fallback: false,
        };
        let request = EncoderRequest::new(preference, query, EncoderProbeTarget::default());
        let generation = match probe
            .service
            .as_ref()
            .unwrap()
            .set_video_encoder_backend(request)
        {
            Ok(generation) => generation,
            Err(error) => {
                probe.result = Some(Err(error.to_string()));
                cx.notify();
                return;
            }
        };
        let timer = cx.background_executor().clone();
        probe.task = Some(cx.spawn(async move |this, cx| {
            for _ in 0..(PROBE_TIMEOUT.as_millis() / PROBE_POLL.as_millis()) {
                timer.timer(PROBE_POLL).await;
                let done = this
                    .update(cx, |panel, cx| {
                        let probe = &mut panel.screen.encoder_probe;
                        let Some(service) = probe.service.as_ref() else {
                            return true;
                        };
                        let snapshot = service.snapshot();
                        if snapshot.generation() != generation || !panel.screen.open {
                            return true;
                        }
                        if snapshot.state() != EncoderProbeState::Ready {
                            return false;
                        }
                        probe.result = Some(Ok(snapshot));
                        cx.notify();
                        true
                    })
                    .unwrap_or(true);
                if done {
                    return;
                }
            }
            let _ = this.update(cx, |panel, cx| {
                let probe = &mut panel.screen.encoder_probe;
                if let Some(service) = probe.service.as_ref() {
                    service.cancel();
                }
                probe.result = Some(Err("Проверка видеокарты не ответила вовремя.".into()));
                cx.notify();
            });
        }));
        cx.notify();
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
