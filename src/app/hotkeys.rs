//! Shortcut capture and the voice keyboard rules that need the app context.
//!
//! Only the window-scoped route exists: this build registers no
//! `RegisterHotKey`/`WH_KEYBOARD_LL` hook, so a binding is honoured only while
//! the Voxhold window has focus. The form must not offer a "global" mode that
//! does nothing.

use super::*;
use gpui::Focusable;

/// Combination string in the Go/frontend format (`Ctrl+Shift+M`). Modifier-only
/// presses return `None` so the recorder can keep waiting for the main key.
pub fn shortcut_from_key(key: &str, modifiers: gpui::Modifiers) -> Option<String> {
    let key = key.trim();
    if key.is_empty()
        || matches!(
            key,
            "control" | "ctrl" | "shift" | "alt" | "super" | "win" | "cmd" | "meta" | "platform"
        )
    {
        return None;
    }
    let mut parts = Vec::new();
    if modifiers.control {
        parts.push("Ctrl");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.platform {
        parts.push("Win");
    }
    let label = match key {
        "space" => "Space".to_owned(),
        "escape" => "Escape".to_owned(),
        other if other.chars().count() == 1 => other.to_uppercase(),
        other => other.to_owned(),
    };
    parts.push(&label);
    Some(parts.join("+"))
}

impl Voxhold {
    /// Begin recording the combination for one of the three voice actions.
    /// Opening the recorder clears `disable while typing` handling only for the
    /// captured keys: the settings card owns the keyboard while it is open.
    pub(super) fn start_shortcut_recording(&mut self, field: usize, cx: &mut Context<Self>) {
        if field > 2 {
            return;
        }
        self.voice_settings.recording_shortcut = Some(field);
        self.voice_settings.shortcut_conflict.clear();
        cx.notify();
    }

    /// Feed a key-down to the recorder. Returns whether the event was consumed.
    /// Escape cancels; a modifier-only press keeps the recorder open; a key
    /// that another action already owns is rejected without writing anything
    /// and leaves the recorder open for the next attempt. Any other key
    /// becomes the new binding and is persisted with the voice snapshot.
    pub(super) fn record_shortcut_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(field) = self.voice_settings.recording_shortcut else {
            return false;
        };
        if !self.voice_tab_open() {
            self.voice_settings.recording_shortcut = None;
            self.voice_settings.shortcut_conflict.clear();
            return false;
        }
        let key = event.keystroke.key.to_lowercase();
        if key == "escape" {
            self.voice_settings.recording_shortcut = None;
            self.voice_settings.shortcut_conflict.clear();
            cx.notify();
            return true;
        }
        let Some(combo) = shortcut_from_key(&key, event.keystroke.modifiers) else {
            return true;
        };
        let own = match field {
            0 => &self.voice_settings.prefs.mute,
            1 => &self.voice_settings.prefs.deafen,
            _ => &self.voice_settings.prefs.push_to_talk,
        };
        let unchanged = crate::voice_settings::canonical_shortcut(&combo)
            == crate::voice_settings::canonical_shortcut(own);
        if unchanged {
            // Re-pressing the action's own combination is accepted as a no-op:
            // it must not read as a conflict with itself or trigger a write.
            self.voice_settings.recording_shortcut = None;
            self.voice_settings.shortcut_conflict.clear();
            cx.notify();
            return true;
        }
        if let Some(owner) = self.voice_settings.prefs.shortcut_owner(&combo, field) {
            let name =
                crate::voice_settings::shortcut_action_name(owner).unwrap_or("другое действие");
            self.voice_settings.shortcut_conflict =
                crate::voice_form_text::shortcut_conflict_message(name);
            // The recorder stays open, the old binding and `saved_error` stay
            // untouched, and nothing is persisted or applied.
            cx.notify();
            return true;
        }
        let prefs = &mut self.voice_settings.prefs;
        match field {
            0 => prefs.mute = combo,
            1 => prefs.deafen = combo,
            _ => prefs.push_to_talk = combo,
        }
        self.voice_settings.recording_shortcut = None;
        self.voice_settings.shortcut_conflict.clear();
        self.persist_voice(cx);
        self.apply_voice_snapshot(cx);
        cx.notify();
        true
    }

    /// True while a text surface owns the keyboard. The frontend
    /// (`isEditableKeyboardTarget`) blocks every voice shortcut there, not only
    /// push-to-talk, so a typed `m` cannot mute the call mid-message.
    pub(super) fn voice_typing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.composer.read(cx).focus_handle(cx).is_focused(window)
    }
}
