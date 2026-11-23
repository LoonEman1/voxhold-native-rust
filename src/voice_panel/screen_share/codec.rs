//! Codec selector of the picker and the live quality dialog.
//!
//! The website's «Автоматически» plus its four SDR codecs, in the same order
//! (H.264, VP8, VP9, AV1). The buttons are real focus-ring members: mouse,
//! Tab/Enter/Space all reach them. An item this build cannot encode is greyed,
//! names the probe's reason and is inert — Start/Apply refuses it instead of
//! substituting another codec. The confirmed codec of the live attempt is
//! painted separately, so the desired choice never impersonates what is really
//! on air.

use super::*;

impl VoicePanel {
    /// One compact codec row. `row` is the caller's focus-ring allocator, so
    /// the buttons join the same Tab order as every other dialog control.
    pub(super) fn codec_row(
        &self,
        ring: &mut Vec<(FocusHandle, ShareKey)>,
        row: &mut dyn FnMut(&Self, &mut Vec<(FocusHandle, ShareKey)>, ShareKey) -> FocusHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = self.colors();
        let intent = self.screen.codec;
        let resolution = self.codec_resolution();
        let label = match &resolution {
            Ok(codec) => match intent {
                CodecIntent::Auto => format!("Автоматически · {}", codec.label()),
                CodecIntent::Explicit(_) => codec.label().to_owned(),
            },
            // Only the chosen explicit codec can be the refused one here; the
            // other items may still be encodable. «Нет доступного» is reserved
            // for Auto, where the whole build really has no SDR encoder.
            Err(_) => match intent {
                CodecIntent::Auto => "Нет доступного SDR-кодека".to_owned(),
                CodecIntent::Explicit(codec) => format!("{} недоступен", codec.label()),
            },
        };
        // gpui exposes painted bounds, not text; the selector carries the exact
        // line so a UI test asserts what the dialog says, not only its slot.
        let label_selector = format!("share-codec-label:{label}");
        let refused = resolution.is_err();
        let reason = resolution
            .err()
            .unwrap_or_else(|| "SDR · серверная передача".to_owned());
        let reason_selector = format!("share-codec-reason:{reason}");
        let mut options = div().flex().flex_wrap().items_center().gap_2().min_w_0();
        for choice in std::iter::once(CodecIntent::Auto)
            .chain(Codec::ALL.into_iter().map(CodecIntent::Explicit))
        {
            let key = ShareKey::Codec(choice);
            let focus = row(self, ring, key);
            // Auto stays selectable even when nothing resolves: the reason is
            // then painted by the row and Start is blocked. A concrete codec
            // with no encoder is not a reachable choice.
            let enabled = match choice {
                CodecIntent::Auto => true,
                CodecIntent::Explicit(codec) => self.codec_availability(codec).is_ok(),
            };
            options = options.child(self.choice_enabled(
                choice.label(),
                intent == choice,
                enabled,
                22.,
                focus,
                key,
                cx,
            ));
        }
        // Unavailable items keep a short probe reason on the same compact line
        // as the resolved codec. The line stays one row high so the normal
        // dialog does not start to scroll.
        let mut unavailable: Vec<String> = Vec::new();
        for codec in Codec::ALL {
            if let Err(reason) = self.codec_availability(codec) {
                unavailable.push(format!("{} недоступен: {reason}", codec.label()));
            }
        }
        let any_refused = !unavailable.is_empty();
        let unavailable_text = unavailable.join(" · ");
        let unavailable_selector = format!("share-codec-unavailable:{unavailable_text}");
        // The live dialog shows what is really on air next to the choice. The
        // encoder-confirmed codec from the stats wins over the configuration
        // the attempt accepted: before the first stats sample the confirmed
        // configuration is only a promise, while the measured codec is the
        // payload that truly leaves this client.
        let confirmed = (self.screen.sharing.phase == Phase::Live)
            .then(|| {
                self.screen
                    .sharing
                    .stats
                    .and_then(|stats| stats.confirmed_codec)
                    .or_else(|| {
                        self.screen
                            .sharing
                            .confirmed
                            .as_ref()
                            .map(|live| live.codec)
                    })
            })
            .flatten();
        div()
            .debug_selector(|| "share-codec".into())
            .flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .w(px(92.))
                    .flex_shrink_0()
                    .pt(px(4.))
                    .text_size(px(12.))
                    .text_color(rgb(c.muted))
                    .child("Кодек"),
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
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .debug_selector(move || label_selector.clone().into())
                                    .text_size(px(10.))
                                    .child(label),
                            )
                            .child(
                                div()
                                    .debug_selector(move || reason_selector.clone().into())
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(10.))
                                    .text_color(rgb(if refused { c.danger } else { c.muted }))
                                    .truncate()
                                    .child(reason),
                            )
                            .when(any_refused, |line| {
                                line.child(
                                    div()
                                        .debug_selector(move || unavailable_selector.clone())
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_size(px(10.))
                                        .text_color(rgb(c.muted))
                                        .truncate()
                                        .child(unavailable_text),
                                )
                            }),
                    )
                    .when_some(confirmed, |column, codec| {
                        let selector = format!("share-codec-confirmed:{}", codec.label());
                        column.child(
                            div()
                                .debug_selector(move || selector.clone())
                                .text_size(px(10.))
                                .text_color(rgb(c.muted))
                                .child(format!(
                                    "В эфире: {} · подтверждён кодировщиком",
                                    codec.label()
                                )),
                        )
                    }),
            )
            .into_any_element()
    }
}
