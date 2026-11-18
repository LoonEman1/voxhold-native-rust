//! The "Голос и звук" tab of the personal settings dialog.
//!
//! The form mirrors Go `settings_voice.go` and the frontend
//! `VoiceSettingsDialog.tsx`. R07b applies input gain, the software gate and
//! APM NS/AGC in the live call; this tab shows the active capture chain
//! (`software` or `platform` ADM) and reports AEC as unavailable/unmeasured
//! because there is no render reference, while keeping the WinMM preview/test
//! tone as real actions. The exact sentences live in `voice_form_text` so they
//! stay testable without a UI query API.

use super::settings_view::ButtonKind;
use super::*;
use crate::audio_capabilities::{capabilities_for, Support};
use crate::voice_settings::{device_choices, BITRATES};
use gpui::ElementId;

/// Fixed logical meter track, as in the Go `levelMeter` box. The fill is
/// computed against this width, so the indicator cannot overflow or collapse
/// when the dialog is resized.
const METER_WIDTH: f32 = 220.0;

/// Slider id for one participant. It is the stable user id, not the row index,
/// so a roster change cannot move a value to a neighbour.
pub(super) fn user_volume_slider_id(user: i64) -> String {
    format!("user-volume-{user}")
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
        let mixer_stats = self
            .voice
            .as_ref()
            .and_then(|panel| panel.read(cx).playout_stats())
            .filter(|stats| stats.running)
            .map(|stats| {
                format!(
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
                )
            });
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
        let (input_names, input_ids, input_current) = device_choices(&prefs, &devices, true);
        let (output_names, output_ids, output_current) = device_choices(&prefs, &devices, false);

        let mut root = div().flex().flex_col().gap(px(14.)).child(self.heading(
            "Голос и звук",
            "Устройства, битрейт и обработка микрофона применяются к звонку без перезапуска комнаты; ниже показан фактический тракт.",
        ));

        root = root
            .child(self.heading(
                "Устройства",
                "Выберите, откуда брать голос и куда выводить звук комнаты.",
            ))
            .child(self.form_text(&format!("Устройство ввода · {input_current}"), false));
        for index in 0..input_names.len() {
            root = root.child(self.settings_button(
                ElementId::Name(format!("audio-in-{index}").into()),
                input_names[index].clone(),
                if input_names[index] == input_current {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                input_names[index] == input_current,
                false,
                // The endpoint id captured at draw time, not the row index: a
                // refreshed list would otherwise select another device.
                SettingsAct::Personal(personal_settings::Action::SelectInput(
                    input_ids[index].clone(),
                )),
                window,
                cx,
            ));
        }
        root = root.child(self.form_text(&format!("Устройство вывода · {output_current}"), false));
        for index in 0..output_names.len() {
            root = root.child(self.settings_button(
                ElementId::Name(format!("audio-out-{index}").into()),
                output_names[index].clone(),
                if output_names[index] == output_current {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                output_names[index] == output_current,
                false,
                SettingsAct::Personal(personal_settings::Action::SelectOutput(
                    output_ids[index].clone(),
                )),
                window,
                cx,
            ));
        }
        root = root.child(self.settings_button(
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
        ));

        root = root
            .child(self.separator())
            .child(self.heading(
                "Уровни звука",
                "Громкость микрофона применяется в software-тракте до Opus. При platform-fallback действует встроенная обработка ADM.",
            ))
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
            root = root.child(self.form_text(&preview_error, true));
        }
        root = root.child(self.form_text(
            if in_call {
                "В звонке индикатор показывает pre-gate уровень активного тракта с учётом усиления микрофона и mute: закрытый микрофон даёт 0, gate к индикатору не применяется."
            } else if capabilities.microphone_preview == Support::Applied {
                "Вне звонка индикатор показывает предпросмотр WinMM с выбранного устройства, умноженный на сохранённое усиление; коралловая риска — планка, к предпросмотру gate не применяется."
            } else {
                "Предпросмотр микрофона недоступен в этой сборке."
            },
            false,
        ));
        root = root
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
                if software_mixer {
                    root = root.child(self.form_text(
                        "Громкость участника 0–200 % применяется программным микшером комнаты; локальный mute — отдельный признак.",
                        false,
                    ));
                } else if mixer_was_software {
                    root = root.child(self.form_text(
                        "Микшер остановлен или перезапускается: усиление участников сейчас не применяется, работает только mute.",
                        true,
                    ));
                } else {
                    root = root.child(self.form_text(
                        "Программный микшер не открылся: усиление участников не применяется, доступен только mute.",
                        true,
                    ));
                }
                root = root.child(self.form_text(
                    "Уровень после микса измеряется в микшере; значения p95 и underrun видны в комнате.",
                    false,
                ));
                if let Some(stats) = &mixer_stats {
                    root = root.child(self.form_text(stats, false));
                }
                for (user, name, volume) in rows {
                    // The id is the stable user id, not a row index: a roster
                    // change during a drag cannot move the value to a neighbour.
                    let id = user_volume_slider_id(user);
                    root = root
                        .child(self.form_text(&name, false))
                        .child(self.settings_slider(
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
                        ));
                }
            }
        }
        root = root.child(self.form_text(
            &format!("Качество передачи · {} Кбит/с", prefs.bitrate),
            false,
        ));
        root = root.child(
            self.voice_segments(
                "bitrate",
                BITRATES.iter().map(|bitrate| bitrate.to_string()).collect(),
                BITRATES
                    .iter()
                    .position(|bitrate| *bitrate == prefs.bitrate),
                |index| {
                    SettingsAct::Personal(personal_settings::Action::SetBitrate(BITRATES[index]))
                },
                window,
                cx,
            ),
        );
        root = root.child(self.form_text(
            "Это максимальный битрейт Opus. При нестабильной сети WebRTC может временно снизить его автоматически.",
            false,
        ));

        root = root
            .child(self.separator())
            .child(self.heading(
                "Звуки интерфейса",
                "Короткие сигналы о подключении, микрофоне и звуке. Слышите их только вы.",
            ))
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
            root = root
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
                root = root.child(self.form_text(&tone_error, true));
            }
        }

        root = root
            .child(self.separator())
            .child(self.heading("Обработка микрофона", ""))
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
            root = root.child(self.settings_slider(
                "noise-gate",
                "Планка шума",
                prefs.noise_gate,
                100,
                |value| SettingsAct::Personal(personal_settings::Action::SetVolume("noise", value)),
                window,
                cx,
            ));
        }
        let capture_note = match applied {
            Some(("software", noise_mode, echo, agc, _)) => {
                crate::voice_form_text::software_capture_note(noise_mode, echo, agc)
            }
            Some(_) => crate::voice_form_text::PLATFORM_CAPTURE_NOTE.to_owned(),
            None => crate::voice_form_text::PENDING_CAPTURE_NOTE.to_owned(),
        };
        root = root
            .child(self.form_text(&capture_note, false))
            .child(self.voice_capability(
                "Эхоподавление",
                capabilities.echo_cancellation,
                crate::voice_form_text::echo_cancellation_note(capabilities.echo_cancellation),
            ))
            .child(self.settings_toggle(
                ElementId::Name("auto-gain".into()),
                "Автоматическое усиление",
                crate::voice_form_text::AUTO_GAIN_DESCRIPTION,
                prefs.auto_gain,
                capabilities.auto_gain == Support::Automatic,
                SettingsAct::Personal(personal_settings::Action::ToggleAutoGain),
                window,
                cx,
            ))
            .child(self.voice_capability(
                "Шумоподавление",
                capabilities.noise_suppression,
                "auto — APM NS; threshold — программный gate; platform ADM применяет своё NS",
            ))
            .child(self.voice_capability(
                "Усиление микрофона",
                capabilities.input_gain,
                crate::voice_form_text::input_gain_note(capabilities.input_gain),
            ))
            .child(self.voice_capability(
                "Планка шума",
                capabilities.noise_gate,
                crate::voice_form_text::noise_gate_note(capabilities.noise_gate),
            ))
            .child(self.voice_capability(
                "Громкость вывода",
                capabilities.output_gain,
                crate::voice_form_text::OUTPUT_GAIN_NOTE,
            ))
            .child(self.voice_capability(
                "Качество передачи",
                capabilities.bitrate,
                "максимальный битрейт Opus применяется сразу",
            ))
            .child(self.form_text(
                "Тракт по умолчанию — software 48 кГц mono; при отказе WinMM движок переходит на platform ADM и сообщает об этом. AEC в software-пути недоступно без render-reference.",
                false,
            ));
        root = root
            .child(self.separator())
            .child(self.heading("Режим ввода и горячие клавиши", ""))
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
            .child(self.voice_shortcuts(window, cx));
        root = root.child(self.form_text(
            "Сочетания действуют, пока окно Voxhold активно: в этой сборке нет глобального перехвата клавиатуры (RegisterHotKey/WH_KEYBOARD_LL), поэтому «глобальный» режим не заявляется.",
            false,
        ));
        if !prefs.shortcuts_valid() {
            root = root
                .child(self.form_text("Назначьте разные сочетания для каждого действия.", true));
        }
        if !device_notice.is_empty() {
            root = root.child(self.form_text(&device_notice, false));
        }
        if !devices_error.is_empty() {
            root = root.child(self.form_text(&devices_error, true));
        }
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

    /// One capability row: a label, a short support tag and the honest note.
    fn voice_capability(&self, label: &str, support: Support, note: &str) -> AnyElement {
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
