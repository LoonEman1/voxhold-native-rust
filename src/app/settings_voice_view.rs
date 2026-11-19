//! The "Голос и звук" tab of the personal settings dialog.
//!
//! The form mirrors Go `settings_voice.go` and the frontend
//! `VoiceSettingsDialog.tsx`. R07b applies input gain, the software gate and
//! APM NS/AGC in the live call; this tab shows the active capture chain
//! (`software` or `platform` ADM) and reports AEC as unavailable/unmeasured
//! because there is no render reference, while keeping the WinMM preview/test
//! tone as real actions. The sentences live in `voice_form_text` so they stay
//! testable without a UI query API.
//!
//! Layout follows the web `voice-settings__section` cards: devices with two
//! compact selectors, levels, bitrate presets, interface sounds, microphone
//! processing, input mode/hotkeys and a collapsed «Подробнее» diagnostics block.

use super::settings_view::{settings_card_metrics, ButtonKind};
use super::*;
use crate::audio_capabilities::{capabilities_for, Support};
use crate::voice_settings::{device_choices, BITRATES, UNAVAILABLE_DEVICE};
use gpui::ElementId;

/// Fixed logical meter track, as in the Go `levelMeter` box. The fill is
/// computed against this width, so the indicator cannot overflow or collapse
/// when the dialog is resized.
const METER_WIDTH: f32 = 220.0;

/// Minimum inner form width for the two device selectors: 538 px at the 620 px
/// card keeps two columns, while a 600 px window leaves 502 px and stacks them.
/// Compared with `CardMetrics::form_width`, i.e. the painted card.
pub(super) const TWO_COLUMN_FORM_WIDTH: f32 = 536.0;

/// Painted advance of the widest preset label, «128 кбит/с», in bundled Inter
/// Bold at 13 px: 68.26 px, rounded up. The layout uses this budget because
/// GPUI exposes no glyph bounds; the tests assert the resulting cell budget.
pub(super) const BITRATE_LABEL_WIDTH: f32 = 68.5;

/// One preset cell paints `px(10.)` per side instead of the shared 14 px.
pub(super) const BITRATE_BUTTON_PADDING: f32 = 20.0;

/// The secondary preset style adds a 1 px border per side.
pub(super) const BITRATE_BORDER: f32 = 2.0;

/// Gap between the preset cells, matching `gap_2`.
pub(super) const BITRATE_GAP: f32 = 8.0;

/// `BITRATES` always offers six presets.
pub(super) const BITRATE_COLUMNS: usize = 6;

/// Smallest inner width where every one-row cell fits the widest label:
/// 6 * (68.5 + 20 + 2) + 5 * 8 = 583 px. The 680 px card has 598 px, one row
/// with 2.5 px slack per cell; the 620 px card has 538 px and uses 3 + 3.
pub(super) const BITRATE_ONE_ROW_MIN_WIDTH: f32 = BITRATE_COLUMNS as f32
    * (BITRATE_LABEL_WIDTH + BITRATE_BUTTON_PADDING + BITRATE_BORDER)
    + (BITRATE_COLUMNS - 1) as f32 * BITRATE_GAP;

/// Width of one preset cell on a single row, before padding (layout tests only).
#[cfg(test)]
pub(super) fn bitrate_cell_width(form_width: f32) -> f32 {
    (form_width - BITRATE_GAP * (BITRATE_COLUMNS - 1) as f32) / BITRATE_COLUMNS as f32
}

/// A saved endpoint that is no longer listed is the only name painted with the
/// danger tone. Matching the shared constant keeps the form and
/// `device_choices` on the same string.
pub(super) fn device_name_is_unavailable(current: &str) -> bool {
    current == UNAVAILABLE_DEVICE
}

/// Slider id for one participant. It is the stable user id, not the row index,
/// so a roster change cannot move a value to a neighbour.
pub(super) fn user_volume_slider_id(user: i64) -> String {
    format!("user-volume-{user}")
}

/// Adjacent endpoint ids for one device selector. `ids` starts with the empty
/// system default, exactly like `device_choices`. A saved id that is no longer
/// listed (hot-unplug) is kept visible by the caller, but both arrows recover
/// to the first valid choice instead of selecting a row by index.
pub(super) fn adjacent_device_ids(ids: &[String], selected: &str) -> (String, String) {
    let fallback = ids.first().cloned().unwrap_or_default();
    let Some(index) = ids.iter().position(|id| id == selected) else {
        return (fallback.clone(), fallback);
    };
    let previous = if index == 0 {
        ids[0].clone()
    } else {
        ids[index - 1].clone()
    };
    let next = ids
        .get(index + 1)
        .cloned()
        .unwrap_or_else(|| ids[index].clone());
    (previous, next)
}

/// The measured mixer line shown in the voice tab. It depends on both the
/// applied path and the live report: a stale `running` snapshot after a death
/// or an ADM fallback must not keep claiming a working software mixer.
pub(super) fn mixer_status_line(
    stats: Option<voxhold_media::mixer::PlayoutStats>,
    software_mixer: bool,
) -> Option<String> {
    let stats = stats?;
    if !software_mixer || !stats.running {
        return None;
    }
    Some(format!(
        "Микшер после общего микса: {} · уровень {:.0} % · p95 {} мкс · underrun {} · drop {} · ограничено {} · release {} · переподключений {} · буфер {} мс",
        stats.phase.label(),
        f64::from(stats.level) * 100.0,
        stats.p95_micros,
        stats.underruns,
        stats.dropped,
        stats.limited_blocks,
        stats.limiter_releases,
        stats.reconnects,
        stats.latency_ms
    ))
}

impl Voxhold {
    pub(super) fn voice_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let devices = self.voice_settings.devices.clone();
        let loading = self.voice_settings.devices_loading;
        let devices_error = if self.voice_settings.devices_error.is_empty() {
            self.voice_settings.saved_error.clone()
        } else {
            self.voice_settings.devices_error.clone()
        };
        let prefs = self.voice_settings.prefs.clone();
        let diagnostics_open = self.voice_settings.diagnostics_open;
        #[cfg(feature = "voxhold-media")]
        let applied = self.voice.as_ref().and_then(|panel| {
            panel.read(cx).applied_audio().map(|audio| {
                (
                    audio.capture,
                    audio.noise_mode,
                    audio.echo_cancellation,
                    audio.auto_gain_control,
                    audio.software_mixer,
                )
            })
        });
        #[cfg(not(feature = "voxhold-media"))]
        let applied: Option<(&'static str, &'static str, bool, bool, bool)> = None;
        let capabilities = capabilities_for(applied.map(|(capture, ..)| capture));
        // A stale applied snapshot must not keep promising a mixer that stopped:
        // the panel combines it with the live playout report.
        let mixer_was_software = applied.is_some_and(|(_, _, _, _, mixer)| mixer);
        #[cfg(feature = "voxhold-media")]
        let software_mixer = self
            .voice
            .as_ref()
            .is_some_and(|panel| panel.read(cx).software_mixer());
        #[cfg(not(feature = "voxhold-media"))]
        let software_mixer = false;
        #[cfg(feature = "voxhold-media")]
        let mixer_stats = mixer_status_line(
            self.voice
                .as_ref()
                .and_then(|panel| panel.read(cx).playout_stats()),
            software_mixer,
        );
        #[cfg(not(feature = "voxhold-media"))]
        let mixer_stats: Option<String> = None;
        let level = self.voice_settings.preview_level.clamp(0.0, 1.0);
        let threshold = (prefs.noise_mode == "threshold").then_some(prefs.noise_gate);
        let preview_error = self.voice_settings.preview_error.clone();
        let tone_error = self.voice_settings.tone_error.clone();
        let device_notice = self.voice_settings.device_notice.clone();
        let tone_pending = self.voice_settings.tone_pending;
        #[cfg(feature = "voxhold-media")]
        let in_call = self
            .voice
            .as_ref()
            .is_some_and(|panel| panel.read(cx).call_active());
        #[cfg(not(feature = "voxhold-media"))]
        let in_call = false;
        let (_, input_ids, input_current) = device_choices(&prefs, &devices, true);
        let (_, output_ids, output_current) = device_choices(&prefs, &devices, false);
        // The two decisions follow the painted settings form, not a window size
        // guess: the selectors only need room for two columns, while the
        // bitrate row must fit six padded cells with the widest label.
        let form_width = settings_card_metrics(self.viewport, self.ui_scale).form_width();
        let selectors_wide = form_width >= TWO_COLUMN_FORM_WIDTH;
        let bitrate_wide = form_width >= BITRATE_ONE_ROW_MIN_WIDTH;

        let mut root = div().flex().flex_col().gap(px(12.)).child(self.heading(
            "Голос и звук",
            "Устройства, битрейт и обработка микрофона применяются к звонку без перезапуска комнаты.",
        ));

        // Devices: two compact selectors and the refresh action inside one
        // card, as in the web `voice-device-grid` and its heading button.
        let input_selector = self.voice_device_selector(
            "audio-in",
            "Устройство ввода",
            &input_ids,
            &prefs.input,
            &input_current,
            true,
            window,
            cx,
        );
        let output_selector = self.voice_device_selector(
            "audio-out",
            "Устройство вывода",
            &output_ids,
            &prefs.output,
            &output_current,
            false,
            window,
            cx,
        );
        let selectors = if selectors_wide {
            div()
                .w_full()
                .flex()
                .items_start()
                .gap(px(12.))
                .child(div().flex_1().min_w(px(0.)).child(input_selector))
                .child(div().flex_1().min_w(px(0.)).child(output_selector))
        } else {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(input_selector)
                .child(output_selector)
        };
        let mut devices_body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(selectors);
        if !devices_error.is_empty() {
            devices_body = devices_body.child(self.form_text(&devices_error, true));
        }
        if !device_notice.is_empty() {
            devices_body = devices_body.child(self.form_text(&device_notice, false));
        }
        let refresh = self.settings_button(
            ElementId::Name("audio-refresh".into()),
            if loading {
                "Обновляем…"
            } else {
                "Обновить"
            },
            ButtonKind::Secondary,
            false,
            loading,
            SettingsAct::Personal(personal_settings::Action::RefreshDevices),
            window,
            cx,
        );
        root = root.child(self.settings_card(
            "voice-devices-card",
            "Устройства",
            "Выберите, откуда брать голос и куда выводить звук комнаты.",
            Some(refresh.into_any_element()),
            devices_body.into_any_element(),
        ));

        // Levels: the microphone slider, its meter directly underneath, the
        // preview action and explanation, then the output volume.
        let meter_explanation = if in_call {
            "В звонке индикатор показывает pre-gate уровень активного тракта с учётом усиления микрофона и mute: закрытый микрофон даёт 0, gate к индикатору не применяется."
        } else if capabilities.microphone_preview == Support::Applied {
            "Вне звонка индикатор показывает предпросмотр WinMM с выбранного устройства, умноженный на сохранённое усиление; коралловая риска — планка, к предпросмотру gate не применяется."
        } else {
            "Предпросмотр микрофона недоступен в этой сборке."
        };
        let mut levels = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.settings_slider(
                "input-volume",
                "Громкость микрофона",
                prefs.input_volume,
                200,
                |value| SettingsAct::Personal(personal_settings::Action::SetVolume("input", value)),
                window,
                cx,
            ))
            .child(self.voice_meter(level, threshold))
            .child(self.settings_button(
                ElementId::Name("voice-test-mic".into()),
                if in_call {
                    "Уровень идёт из передачи"
                } else {
                    "Проверить микрофон"
                },
                ButtonKind::Secondary,
                false,
                capabilities.microphone_preview != Support::Applied || in_call,
                SettingsAct::Personal(personal_settings::Action::TestMicrophone),
                window,
                cx,
            ));
        if !preview_error.is_empty() {
            levels = levels.child(self.form_text(&preview_error, true));
        }
        levels = levels
            .child(self.form_text(meter_explanation, false))
            .child(self.settings_slider(
                "output-volume",
                "Громкость вывода",
                prefs.output_volume,
                200,
                |value| {
                    SettingsAct::Personal(personal_settings::Action::SetVolume("output", value))
                },
                window,
                cx,
            ))
            .child(self.form_text(crate::voice_form_text::OUTPUT_VOLUME_NOTE, false));
        root = root.child(self.settings_card(
            "voice-levels-card",
            "Уровни звука",
            "Громкость микрофона применяется в software-тракте до Opus. При platform-fallback действует встроенная обработка ADM.",
            None,
            levels.into_any_element(),
        ));

        // Per-user room volume: the stable key is the user id, so a reconnect
        // that changes connection ids does not move a volume to another person.
        #[cfg(feature = "voxhold-media")]
        if in_call {
            let rows: Vec<(i64, String, i64)> = self
                .voice
                .as_ref()
                .map(|panel| {
                    let panel = panel.read(cx);
                    panel
                        .call_participants()
                        .into_iter()
                        .map(|(user, name)| (user, name, panel.user_volume(user)))
                        .collect()
                })
                .unwrap_or_default();
            if !rows.is_empty() {
                let mut participants = div().w_full().flex().flex_col().gap(px(8.));
                if software_mixer {
                    participants = participants.child(self.form_text(
                        "Громкость участника 0–200 % применяется программным микшером комнаты; локальный mute — отдельный признак.",
                        false,
                    ));
                } else if mixer_was_software {
                    participants = participants.child(self.form_text(
                        "Микшер остановлен или перезапускается: усиление участников сейчас не применяется, работает только mute.",
                        true,
                    ));
                } else {
                    participants = participants.child(self.form_text(
                        "Программный микшер не открылся: усиление участников не применяется, доступен только mute.",
                        true,
                    ));
                }
                participants = participants.child(self.form_text(
                    "Уровень после микса измеряется в микшере; значения p95 и underrun видны в комнате.",
                    false,
                ));
                if let Some(stats) = &mixer_stats {
                    participants = participants.child(self.form_text(stats, false));
                }
                for (user, name, volume) in rows {
                    // The id is the stable user id, not a row index: a roster
                    // change during a drag cannot move the value to a neighbour.
                    let id = user_volume_slider_id(user);
                    participants = participants.child(self.form_text(&name, false)).child(
                        self.settings_slider(
                            &id,
                            "Громкость участника",
                            volume,
                            200,
                            move |value| {
                                SettingsAct::Personal(
                                    personal_settings::Action::SetVoiceUserVolume(user, value),
                                )
                            },
                            window,
                            cx,
                        ),
                    );
                }
                root = root.child(self.settings_card(
                    "voice-participants-card",
                    "Громкость участников",
                    "Отдельная громкость каждого собеседника в комнате.",
                    None,
                    participants.into_any_element(),
                ));
            }
        }

        // Quality: six Opus presets, one row of six or two rows of three.
        let quality = self.voice_bitrate_presets(bitrate_wide, prefs.bitrate, window, cx);
        root = root.child(self.settings_card(
            "voice-quality-card",
            "Качество передачи",
            "Максимальный битрейт Opus; ползунки уровней выше не меняют этот предел.",
            None,
            quality,
        ));

        // Interface sounds.
        let mut sounds = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.settings_toggle(
                ElementId::Name("sounds".into()),
                "Звуки подключения и микрофона",
                "",
                prefs.sounds,
                false,
                SettingsAct::Personal(personal_settings::Action::ToggleSounds),
                window,
                cx,
            ));
        if prefs.sounds {
            sounds = sounds
                .child(self.settings_slider(
                    "sound-volume",
                    "Громкость сигналов",
                    prefs.sound_volume,
                    100,
                    |value| {
                        SettingsAct::Personal(personal_settings::Action::SetVolume("sound", value))
                    },
                    window,
                    cx,
                ))
                .child(self.settings_button(
                    ElementId::Name("voice-test-tone".into()),
                    if tone_pending {
                        "Проверяем…"
                    } else {
                        "Проверить"
                    },
                    ButtonKind::Secondary,
                    false,
                    capabilities.output_test != Support::Applied || tone_pending,
                    SettingsAct::Personal(personal_settings::Action::TestTone),
                    window,
                    cx,
                ));
            if !tone_error.is_empty() {
                sounds = sounds.child(self.form_text(&tone_error, true));
            }
        }
        root = root.child(self.settings_card(
            "voice-sounds-card",
            "Звуки интерфейса",
            "Короткие сигналы о подключении, микрофоне и звуке. Слышите их только вы.",
            None,
            sounds.into_any_element(),
        ));

        // Microphone processing stays a real card outside the diagnostics.
        let mut processing =
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(self.voice_segments(
                    "noise-mode",
                    vec!["Автоматически".into(), "С нижней планкой".into()],
                    Some(if prefs.noise_mode == "threshold" {
                        1
                    } else {
                        0
                    }),
                    |index| SettingsAct::Personal(personal_settings::Action::SetNoiseMode(index)),
                    window,
                    cx,
                ));
        if prefs.noise_mode == "threshold" {
            processing = processing
                .child(self.settings_slider(
                    "noise-gate",
                    "Планка шума",
                    prefs.noise_gate,
                    100,
                    |value| {
                        SettingsAct::Personal(personal_settings::Action::SetVolume("noise", value))
                    },
                    window,
                    cx,
                ))
                .child(self.form_text(
                    "Коралловая линия на индикаторе — планка. Всё левее неё передаётся как тишина: помолчите и поднимайте ползунок, пока уровень в тишине не окажется слева от линии.",
                    false,
                ));
        }
        processing = processing.child(self.settings_toggle(
            ElementId::Name("auto-gain".into()),
            "Автоматическое усиление",
            crate::voice_form_text::AUTO_GAIN_DESCRIPTION,
            prefs.auto_gain,
            capabilities.auto_gain == Support::Automatic,
            SettingsAct::Personal(personal_settings::Action::ToggleAutoGain),
            window,
            cx,
        ));
        root = root.child(self.settings_card(
            "voice-processing-card",
            "Обработка микрофона",
            "Настройки передаются движку при захвате выбранного устройства.",
            None,
            processing.into_any_element(),
        ));

        // Input mode and hotkeys.
        let mut input_card = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(self.settings_option(
                ElementId::Name("input-mode-voice".into()),
                "Постоянный микрофон",
                "Голос передаётся, пока микрофон не выключен.",
                prefs.input_mode == "voice_activity",
                false,
                SettingsAct::Personal(personal_settings::Action::SetInputMode(0)),
                window,
                cx,
            ))
            .child(self.settings_option(
                ElementId::Name("input-mode-ptt".into()),
                "Нажми и говори",
                "Микрофон работает только при удержании клавиши.",
                prefs.input_mode == "push_to_talk",
                false,
                SettingsAct::Personal(personal_settings::Action::SetInputMode(1)),
                window,
                cx,
            ))
            .child(self.voice_shortcuts(window, cx))
            .child(self.form_text(
                "Сочетания действуют, пока окно Voxhold активно: в этой сборке нет глобального перехвата клавиатуры (RegisterHotKey/WH_KEYBOARD_LL), поэтому «глобальный» режим не заявляется.",
                false,
            ));
        if !prefs.shortcuts_valid() {
            input_card = input_card
                .child(self.form_text("Назначьте разные сочетания для каждого действия.", true));
        }
        root = root.child(self.settings_card(
            "voice-input-card",
            "Режим ввода и горячие клавиши",
            "",
            None,
            input_card.into_any_element(),
        ));

        // Diagnostics: collapsed by default; only explanation rows live here.
        let capture_note = match applied {
            Some(("software", noise_mode, echo, agc, _)) => {
                crate::voice_form_text::software_capture_note(noise_mode, echo, agc)
            }
            Some(_) => crate::voice_form_text::PLATFORM_CAPTURE_NOTE.to_owned(),
            None => crate::voice_form_text::PENDING_CAPTURE_NOTE.to_owned(),
        };
        let mut diagnostics = div().w_full().flex().flex_col().gap(px(8.));
        if diagnostics_open {
            diagnostics = diagnostics
                .child(self.voice_note("voice-capture-note", &capture_note, false))
                .child(self.voice_capability(
                    "cap-echo",
                    "Эхоподавление",
                    capabilities.echo_cancellation,
                    crate::voice_form_text::echo_cancellation_note(capabilities.echo_cancellation),
                ))
                .child(self.voice_capability(
                    "cap-noise",
                    "Шумоподавление",
                    capabilities.noise_suppression,
                    "auto — APM NS; threshold — программный gate; platform ADM применяет своё NS",
                ))
                .child(self.voice_capability(
                    "cap-gain",
                    "Усиление микрофона",
                    capabilities.input_gain,
                    crate::voice_form_text::input_gain_note(capabilities.input_gain),
                ))
                .child(self.voice_capability(
                    "cap-gate",
                    "Планка шума",
                    capabilities.noise_gate,
                    crate::voice_form_text::noise_gate_note(capabilities.noise_gate),
                ))
                .child(self.voice_capability(
                    "cap-output",
                    "Громкость вывода",
                    capabilities.output_gain,
                    crate::voice_form_text::OUTPUT_GAIN_NOTE,
                ))
                .child(self.voice_capability(
                    "cap-bitrate",
                    "Качество передачи",
                    capabilities.bitrate,
                    "максимальный битрейт Opus применяется сразу",
                ))
                .child(self.voice_note(
                    "voice-chain-note",
                    "Тракт по умолчанию — software 48 кГц mono; при отказе WinMM движок переходит на platform ADM и сообщает об этом. AEC в software-пути недоступно без render-reference.",
                    false,
                ));
        }
        let diagnostics_toggle = self.settings_button(
            ElementId::Name("voice-diagnostics-toggle".into()),
            if diagnostics_open {
                "Свернуть"
            } else {
                "Подробнее"
            },
            ButtonKind::Secondary,
            false,
            false,
            SettingsAct::Personal(personal_settings::Action::ToggleVoiceDiagnostics),
            window,
            cx,
        );
        root = root.child(self.settings_card(
            "voice-diagnostics-card",
            "Диагностика",
            "Фактический тракт захвата и доступность обработки.",
            Some(diagnostics_toggle.into_any_element()),
            diagnostics.into_any_element(),
        ));

        root = root.child(self.separator()).child(
            div()
                .flex()
                .gap_2()
                .child(self.settings_button(
                    ElementId::Name("voice-reset".into()),
                    "Сбросить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::ResetVoice),
                    window,
                    cx,
                ))
                .child(self.settings_button(
                    ElementId::Name("voice-done".into()),
                    "Готово",
                    ButtonKind::Primary,
                    false,
                    false,
                    SettingsAct::Close,
                    window,
                    cx,
                )),
        );
        root.into_any_element()
    }

    /// One compact device selector: label, then the current name between the
    /// prev/next affordances. The choice is the endpoint id captured from
    /// `device_choices`, never the row index, so a refreshed list cannot slide
    /// the selection to another row.
    fn voice_device_selector(
        &mut self,
        prefix: &'static str,
        label: &str,
        ids: &[String],
        selected: &str,
        current: &str,
        input: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let (previous, next) = adjacent_device_ids(ids, selected);
        let action = |id: String| {
            if input {
                personal_settings::Action::SelectInput(id)
            } else {
                personal_settings::Action::SelectOutput(id)
            }
        };
        let selector_id = format!("{prefix}-selector");
        let name_id = format!("{prefix}-name");
        let unavailable = device_name_is_unavailable(current);
        let previous = self
            .settings_button(
                ElementId::Name(format!("{prefix}-prev").into()),
                "‹",
                ButtonKind::Secondary,
                false,
                false,
                SettingsAct::Personal(action(previous)),
                window,
                cx,
            )
            .h(px(32.))
            .w(px(32.))
            .px(px(0.));
        let next = self
            .settings_button(
                ElementId::Name(format!("{prefix}-next").into()),
                "›",
                ButtonKind::Secondary,
                false,
                false,
                SettingsAct::Personal(action(next)),
                window,
                cx,
            )
            .h(px(32.))
            .w(px(32.))
            .px(px(0.));
        div()
            .w_full()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.muted))
                    .child(label.to_owned()),
            )
            .child(
                div()
                    .id(ElementId::Name(selector_id.clone().into()))
                    .debug_selector(move || selector_id.clone())
                    .w_full()
                    .h(px(40.))
                    .px(px(4.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(px(9.))
                    .border_1()
                    .border_color(ink(p.line))
                    .bg(ink(p.bg))
                    .child(previous)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .px(px(4.))
                            .text_size(px(13.))
                            .text_color(ink(if unavailable { p.danger } else { p.fg }))
                            .debug_selector(move || name_id.clone())
                            .child(current.to_owned()),
                    )
                    .child(next),
            )
            .into_any_element()
    }

    /// Six Opus presets as in the web `.voice-bitrate-setting`: one row of six
    /// on a wide form, two rows of three otherwise; the selected one is lime.
    fn voice_bitrate_presets(
        &mut self,
        one_row: bool,
        bitrate: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let columns = if one_row { BITRATE_COLUMNS } else { 3 };
        let mut body = div().w_full().flex().flex_col().gap(px(8.));
        for row in BITRATES.chunks(columns) {
            let mut line = div().w_full().flex().gap_2();
            for value in row {
                let index = BITRATES
                    .iter()
                    .position(|preset| preset == value)
                    .unwrap_or(0);
                let selected = bitrate == *value;
                let button = self
                    .settings_button(
                        ElementId::Name(format!("bitrate-{index}").into()),
                        format!("{value} кбит/с"),
                        if selected {
                            ButtonKind::Primary
                        } else {
                            ButtonKind::Secondary
                        },
                        selected,
                        false,
                        SettingsAct::Personal(personal_settings::Action::SetBitrate(*value)),
                        window,
                        cx,
                    )
                    .px(px(10.));
                line = line.child(div().flex_1().min_w(px(0.)).child(button.w_full()));
            }
            body = body.child(line);
        }
        body.child(self.form_text(
            "Это максимальный битрейт Opus. При нестабильной сети WebRTC может временно снизить его автоматически.",
            false,
        ))
        .into_any_element()
    }

    /// Go `voiceShortcuts`: one row per action with the current binding. While
    /// recording, the row that owns the recorder says "Нажмите…" and the next
    /// key-down in the settings card becomes the binding.
    fn voice_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let prefs = self.voice_settings.prefs.clone();
        let recording = self.voice_settings.recording_shortcut;
        let rows = [
            (0usize, "Микрофон вкл/выкл", prefs.mute.clone()),
            (1, "Звук вкл/выкл", prefs.deafen.clone()),
            (2, "Нажми и говори", prefs.push_to_talk.clone()),
        ];
        let mut column = div().w_full().flex().flex_col().gap(px(6.));
        for (field, title, value) in rows {
            let active = recording == Some(field);
            let label = if active {
                "Нажмите сочетание…".to_owned()
            } else {
                format!("{title}: {}", crate::voice_settings::shortcut_label(&value))
            };
            column = column.child(self.settings_button(
                ElementId::Name(format!("voice-shortcut-{field}").into()),
                label,
                if active {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                active,
                false,
                SettingsAct::Personal(personal_settings::Action::RecordShortcut(field)),
                window,
                cx,
            ));
        }
        if recording.is_some() {
            column = column.child(self.form_text(
                "Нажмите сочетание клавиш или Escape, чтобы отменить.",
                false,
            ));
        }
        column.into_any_element()
    }

    /// Go `f.segments`: one row of equal-width mode buttons. The selected
    /// segment uses the tab kind so the active mode is unambiguous.
    fn voice_segments(
        &mut self,
        id: &'static str,
        labels: Vec<String>,
        active: Option<usize>,
        action: impl Fn(usize) -> SettingsAct + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div().w_full().flex().gap_2();
        for (index, label) in labels.into_iter().enumerate() {
            let selected = active == Some(index);
            let button = self.settings_button(
                ElementId::Name(format!("{id}-{index}").into()),
                label,
                if selected {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                selected,
                false,
                action(index),
                window,
                cx,
            );
            row = row.child(div().flex_1().min_w(px(0.)).child(button.w_full()));
        }
        row.into_any_element()
    }

    /// Fixed 220 px track with the coral gate riser. The fill and the riser are
    /// both computed against `METER_WIDTH`, never against the painted width.
    fn voice_meter(&self, level: f32, threshold: Option<i64>) -> AnyElement {
        let p = self.palette;
        let ratio = level.clamp(0.0, 1.0);
        let gated = threshold.is_some_and(|limit| ratio * 100.0 < limit as f32);
        let fill = if gated { p.muted } else { p.lime };
        let mut track = div()
            .w(px(METER_WIDTH))
            .h(px(9.))
            .rounded(px(4.))
            .bg(ink(p.selection))
            .relative()
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(0.))
                    .h(px(9.))
                    .w(px(METER_WIDTH * ratio))
                    .rounded(px(4.))
                    .bg(ink(fill)),
            );
        if let Some(threshold) = threshold {
            let left = (METER_WIDTH * (threshold.clamp(0, 100) as f32 / 100.0) - 1.0).max(0.0);
            track = track.child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(-2.))
                    .w(px(2.))
                    .h(px(13.))
                    .bg(ink(p.danger)),
            );
        }
        let label = match threshold {
            Some(threshold) => format!(
                "Уровень микрофона · {}% · планка {threshold}%",
                (ratio * 100.0).round() as i64
            ),
            None => format!("Уровень микрофона · {}%", (ratio * 100.0).round() as i64),
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(label),
            )
            .child(track)
            .into_any_element()
    }

    /// A note with a debug selector, so the layout tests can prove that the
    /// diagnostics block reveals and hides the technical text.
    fn voice_note(&self, id: &'static str, text: &str, danger: bool) -> AnyElement {
        div()
            .w_full()
            .debug_selector(move || id.to_string())
            .child(self.form_text(text, danger))
            .into_any_element()
    }

    /// One capability row: a label, a short support tag and the honest note.
    fn voice_capability(
        &self,
        id: &'static str,
        label: &str,
        support: Support,
        note: &str,
    ) -> AnyElement {
        let p = self.palette;
        let tag = match support {
            Support::Applied => "применяется",
            Support::Automatic => "автоматически",
            Support::Unavailable => "недоступно",
        };
        let tag_color = match support {
            Support::Applied => p.lime,
            Support::Automatic => p.muted,
            Support::Unavailable => p.danger,
        };
        div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || id.to_string())
            .w_full()
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_size(px(13.)).child(format!("{label} · ")))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(ink(tag_color))
                            .child(tag.to_owned()),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(note.to_owned()),
            )
            .into_any_element()
    }
}
