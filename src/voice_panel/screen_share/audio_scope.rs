//! Audio-scope choices of the screen picker.
//!
//! Windows offers three honest scopes for the screen-share sound: the process
//! tree of the selected window, the whole endpoint except this client, or the
//! whole endpoint. The first two are process loopback (`Windows 10 build 20348+`
//! or Windows 11); the third is the plain endpoint loopback. The picker states
//! what the engine really captures instead of promising echo-free audio it
//! cannot deliver with the whole-device scope.

use super::*;
use voxhold_media::system_audio::AudioScope;

pub(super) const WINDOW_LABEL: &str = "Звук этого приложения";
pub(super) const EXCLUDE_LABEL: &str = "Системный звук без Voxhold";
pub(super) const DEVICE_LABEL: &str = "Всё устройство вывода";

pub(super) const WINDOW_SENTENCE: &str =
    "Захватываются только процессы выбранного окна через Windows process loopback; если Windows его не поддерживает, трансляция завершится ошибкой.";
pub(super) const EXCLUDE_SENTENCE: &str =
    "Захватывается весь звук устройства вывода, кроме процессов Voxhold: голос комнаты и плеер в трансляцию не попадут. Если Windows не поддерживает process loopback, запуск завершится ошибкой, а не переходом на весь звук устройства.";
pub(super) const DEVICE_SENTENCE: &str =
    "Захватывается всё, что воспроизводится на устройстве вывода по умолчанию, включая голос комнаты и плеер Voxhold.";

/// The default scope of one source kind. Audio itself starts off, so this is
/// only the choice the user sees if they enable it.
pub(super) fn default_audio_scope(kind: SourceKind) -> AudioScope {
    match kind {
        SourceKind::Window => AudioScope::WindowApplication,
        SourceKind::Monitor => AudioScope::EverythingExceptSelf,
    }
}

/// The two choices the picker offers for one source kind, in display order.
pub(super) fn audio_scope_choices(kind: SourceKind) -> [(AudioScope, &'static str); 2] {
    match kind {
        SourceKind::Window => [
            (AudioScope::WindowApplication, WINDOW_LABEL),
            (AudioScope::Device, DEVICE_LABEL),
        ],
        SourceKind::Monitor => [
            (AudioScope::EverythingExceptSelf, EXCLUDE_LABEL),
            (AudioScope::Device, DEVICE_LABEL),
        ],
    }
}

impl ScreenShareUi {
    pub(in crate::voice_panel) fn reset_audio_scope(&mut self, kind: SourceKind) {
        self.audio_scope = default_audio_scope(kind);
    }

    /// The scope a request for this source kind really carries. A stale
    /// window-only choice is replaced by the kind's default; the explicit
    /// whole-device choice is always preserved.
    pub(in crate::voice_panel) fn effective_audio_scope(&self, kind: SourceKind) -> AudioScope {
        match (self.audio_scope, kind) {
            (AudioScope::WindowApplication, SourceKind::Monitor) => default_audio_scope(kind),
            (AudioScope::EverythingExceptSelf, SourceKind::Window) => default_audio_scope(kind),
            (scope, _) => scope,
        }
    }

    /// The sentence under the scope choices: what is really captured.
    pub(in crate::voice_panel) fn audio_scope_sentence(&self) -> &'static str {
        match self.effective_audio_scope(self.kind) {
            AudioScope::WindowApplication => WINDOW_SENTENCE,
            AudioScope::EverythingExceptSelf => EXCLUDE_SENTENCE,
            AudioScope::Device => DEVICE_SENTENCE,
        }
    }
}

impl VoicePanel {
    /// Pick one of the offered scopes. Audio must be on and available; the
    /// choice is ignored otherwise, so no hidden control can change it.
    pub(in crate::voice_panel) fn select_audio_scope(
        &mut self,
        scope: AudioScope,
        cx: &mut Context<Self>,
    ) {
        if !self.screen.audio_requested() {
            return;
        }
        let allowed = audio_scope_choices(self.screen.kind)
            .iter()
            .any(|(choice, _)| *choice == scope);
        if !allowed {
            return;
        }
        self.screen.audio_scope = scope;
        cx.notify();
    }
}
