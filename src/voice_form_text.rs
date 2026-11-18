//! The exact sentences the voice tab paints about the audio path.
//!
//! GPUI exposes no text query in tests, so the wording that must be honest
//! lives here, free of UI types, and the view only renders what these functions
//! return. Each one is asserted in the unit tests below.

use crate::audio_capabilities::Support;

fn on_off(value: bool) -> &'static str {
    if value {
        "вкл"
    } else {
        "выкл"
    }
}

/// Note under the meter for the open software WinMM chain. The engine reports
/// AEC from its applied snapshot; without a render reference it is false and
/// the sentence says the canceller is unavailable instead of promising it.
pub fn software_capture_note(noise_mode: &str, echo_cancellation: bool, auto_gain: bool) -> String {
    let aec = if echo_cancellation {
        "вкл"
    } else {
        "недоступно: нет render-reference"
    };
    format!(
        "Активный тракт: software 48 кГц · NS {noise_mode} · AGC {} · AEC {aec}.",
        on_off(auto_gain)
    )
}

/// Note for the platform ADM fallback. Its own NS/AGC run inside the ADM; its
/// echo canceller is never measured, so the sentence says so and never claims
/// AEC is on.
pub const PLATFORM_CAPTURE_NOTE: &str = "Активный тракт: platform ADM · NS auto · AGC применяет ADM (значение не измерялось) · AEC не проверено: render-эталон не измерялся. Пользовательские gain и gate в этом тракте не применяются.";

/// Note before any confirmed call.
pub const PENDING_CAPTURE_NOTE: &str = "Активный тракт появится после подтверждённого входа в звонок. По умолчанию используется software 48 кГц; при отказе WinMM — platform ADM.";

/// Capability note for AEC.
pub fn echo_cancellation_note(support: Support) -> &'static str {
    match support {
        Support::Applied => "AEC подтверждён замером render-reference.",
        _ => {
            "в software-пути нет render-reference, поэтому AEC не включается; platform ADM может включать свой, но это не измерено"
        }
    }
}

/// Capability note for the input gain, which the ADM ignores.
pub fn input_gain_note(support: Support) -> &'static str {
    match support {
        Support::Unavailable => "0–200 % хранится, но активный platform ADM его не применяет",
        _ => "0–200 % в software-пути до Opus; 0 % — тишина",
    }
}

/// Capability note for the noise gate, which only the software path has.
pub fn noise_gate_note(support: Support) -> &'static str {
    match support {
        Support::Unavailable => "0–100 % хранится, но в platform ADM порог недоступен",
        _ => "0 % обходит gate; 1–100 % действует в software-пути",
    }
}

/// Capability note for the room output volume. The controller applies it to
/// every participant track in both capture paths; per-participant volumes, the
/// limiter and the voice/system/preview mix are W02c.
pub const OUTPUT_GAIN_NOTE: &str =
    "0 % закрывает дорожки участников, 1–200 % масштабирует их громкость";

/// Sentence under the output-volume slider.
pub const OUTPUT_VOLUME_NOTE: &str = "Общая громкость воспроизведения применяется к дорожкам участников. Раздельные громкости участников, лимитер и микширование voice/system/preview — W02c.";

/// Description of the AGC switch. The ADM's own AGC cannot be toggled from
/// here and was not measured.
pub const AUTO_GAIN_DESCRIPTION: &str =
    "APM в software-пути выравнивает голос; platform ADM применяет своё усиление, которое здесь не измеряется";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_note_never_claims_echo_cancellation() {
        let note = PLATFORM_CAPTURE_NOTE.to_lowercase();
        assert!(!note.contains("aec вкл"));
        assert!(!note.contains("aec включ"));
        assert!(note.contains("aec не проверено"));
        assert!(PLATFORM_CAPTURE_NOTE.contains("не измерялся"));
    }

    #[test]
    fn the_software_note_reports_the_applied_aec_value_honestly() {
        let unavailable = software_capture_note("auto", false, true);
        assert!(unavailable.contains("AEC недоступно"), "{unavailable}");
        assert!(!unavailable.to_lowercase().contains("aec вкл"));
        let enabled = software_capture_note("threshold", true, false);
        assert!(enabled.contains("AEC вкл"), "only a measured value says so");
        assert!(enabled.contains("NS threshold"));
        assert!(enabled.contains("AGC выкл"));
    }

    #[test]
    fn output_volume_is_described_as_applied_to_participant_tracks() {
        assert!(!OUTPUT_GAIN_NOTE.contains("не масштабирует"));
        assert!(!OUTPUT_VOLUME_NOTE.contains("не масштабирует"));
        assert!(OUTPUT_GAIN_NOTE.contains("1–200 %"));
        assert!(OUTPUT_VOLUME_NOTE.contains("дорожкам участников"));
        assert!(OUTPUT_VOLUME_NOTE.contains("W02c"));
    }

    #[test]
    fn unavailable_chain_values_are_not_promised() {
        assert!(input_gain_note(Support::Unavailable).contains("не применяет"));
        assert!(noise_gate_note(Support::Unavailable).contains("недоступен"));
        assert_eq!(
            echo_cancellation_note(Support::Unavailable),
            echo_cancellation_note(Support::Automatic)
        );
    }
}
