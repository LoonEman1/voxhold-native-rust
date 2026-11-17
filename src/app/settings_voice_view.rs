//! The "Голос и звук" tab of the personal settings dialog.
//!
//! The form mirrors Go `settings_voice.go` and the frontend
//! `VoiceSettingsDialog.tsx`, but every control says what it really does in
//! this build. Echo cancellation, automatic gain and noise suppression are
//! applied by the platform audio path and are shown as capabilities, not as
//! switches; input gain and the gate need the R07b PCM path; the microphone
//! preview and the interface tone are real WinMM ports.

use super::settings_view::ButtonKind;
use super::*;
use crate::audio_capabilities::{audio_capabilities, Support};
use crate::voice_settings::{device_choices, BITRATES};
use gpui::ElementId;

/// Fixed logical meter track, as in the Go `levelMeter` box. The fill is
/// computed against this width, so the indicator cannot overflow or collapse
/// when the dialog is resized.
const METER_WIDTH: f32 = 220.0;

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
        let capabilities = audio_capabilities();
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
        let (input_names, _, input_current) = device_choices(&prefs, &devices, true);
        let (output_names, _, output_current) = device_choices(&prefs, &devices, false);

        let mut root = div().flex().flex_col().gap(px(14.)).child(self.heading(
            "Голос и звук",
            "Устройства и битрейт применяются к звонку без перезапуска комнаты. Обработка микрофона показывает реальные возможности этого тракта.",
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
                SettingsAct::Personal(personal_settings::Action::SelectInput(index)),
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
                SettingsAct::Personal(personal_settings::Action::SelectOutput(index)),
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
                "Громкость микрофона применяется в проверке. Качество передачи применяется в голосовом канале.",
            ))
            .child(self.settings_value_slider(
                "input-volume",
                "Громкость микрофона",
                prefs.input_volume,
                200,
                5,
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
                "В звонке индикатор показывает уровень передачи, а не отдельный предпросмотр: микрофон уже принадлежит звонку."
            } else if capabilities.microphone_preview == Support::Applied {
                "Вне звонка индикатор показывает предпросмотр WinMM с выбранного устройства; в передачу этот захват не входит."
            } else {
                "Предпросмотр микрофона недоступен в этой сборке."
            },
            false,
        ));
        root = root
            .child(self.settings_value_slider(
                "output-volume",
                "Громкость вывода",
                prefs.output_volume,
                200,
                5,
                |value| {
                    SettingsAct::Personal(personal_settings::Action::SetVolume("output", value))
                },
                window,
                cx,
            ))
            .child(self.form_text(
                "0 % закрывает треки собеседников. 1–200 % пока не масштабирует звук: полный коэффициент появится с PCM-трактом R07b.",
                false,
            ))
            .child(self.form_text(
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
                .child(self.settings_value_slider(
                    "sound-volume",
                    "Громкость сигналов",
                    prefs.sound_volume,
                    100,
                    5,
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
            root = root.child(self.settings_value_slider(
                "noise-gate",
                "Планка шума",
                prefs.noise_gate,
                100,
                5,
                |value| SettingsAct::Personal(personal_settings::Action::SetVolume("noise", value)),
                window,
                cx,
            ));
        }
        root = root
            .child(self.voice_capability(
                "Эхоподавление",
                capabilities.echo_cancellation,
                "применяется системным аудиотрактом автоматически",
            ))
            .child(self.voice_capability(
                "Автоматическое усиление",
                capabilities.auto_gain,
                "выравнивает голос автоматически",
            ))
            .child(self.voice_capability(
                "Шумоподавление",
                capabilities.noise_suppression,
                "применяется системным аудиотрактом автоматически",
            ))
            .child(self.voice_capability(
                "Усиление микрофона",
                capabilities.input_gain,
                "сейчас действует только 0 % (микрофон закрыт); 1–200 % и порог начнут действовать с PCM-трактом R07b",
            ))
            .child(self.voice_capability(
                "Планка шума",
                capabilities.noise_gate,
                "порог сохраняется и показывается, но в передаче начнёт действовать с R07b",
            ))
            .child(self.voice_capability(
                "Громкость вывода",
                capabilities.output_gain,
                "0 % закрывает дорожки; 1–200 % пока не масштабирует звук",
            ))
            .child(self.voice_capability(
                "Качество передачи",
                capabilities.bitrate,
                "битрейт применяется сразу",
            ))
            .child(self.form_text(
                "Состояние этих возможностей проверено по закреплённому libwebrtc 0.4.0: программные AEC/AGC/шумоподавление включает сам аудиотракт, отдельных переключателей в этой сборке нет.",
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
            .child(self.form_text(
                &format!(
                    "Микрофон: {} · Звук: {} · Нажми и говори: {}",
                    prefs.mute, prefs.deafen, prefs.push_to_talk
                ),
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
            Support::Limited => "частично",
            Support::Unavailable => "после R07b",
        };
        let tag_color = match support {
            Support::Applied => p.lime,
            Support::Automatic | Support::Limited => p.muted,
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
