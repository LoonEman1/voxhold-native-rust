//! The hotkey rows of the voice tab: the current binding, the open recorder
//! and the rejection notice for a combination another action already owns.
//!
//! Kept out of the card sequence in `settings_voice_view` so the action names,
//! the recorder hint and the conflict sentence stay together.

use super::settings_view::ButtonKind;
use super::*;
use crate::voice_settings::{shortcut_action_name, shortcut_label};
use gpui::ElementId;

impl Voxhold {
    /// Go `voiceShortcuts`: one row per action with the current binding. While
    /// recording, the row that owns the recorder says "Нажмите…" and the next
    /// key-down in the settings card becomes the binding. A combination owned
    /// by another action is rejected with its name and the recorder stays open.
    pub(super) fn voice_shortcuts(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefs = self.voice_settings.prefs.clone();
        let recording = self.voice_settings.recording_shortcut;
        let conflict = self.voice_settings.shortcut_conflict.clone();
        let rows = [
            prefs.mute.clone(),
            prefs.deafen.clone(),
            prefs.push_to_talk.clone(),
        ];
        let mut column = div().w_full().flex().flex_col().gap(px(6.));
        for (field, value) in rows.into_iter().enumerate() {
            let title = shortcut_action_name(field).unwrap_or("Сочетание");
            let active = recording == Some(field);
            let label = if active {
                "Нажмите сочетание…".to_owned()
            } else {
                format!("{title}: {}", shortcut_label(&value))
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
            let (hint, danger) = if conflict.is_empty() {
                (
                    "Нажмите сочетание клавиш или Escape, чтобы отменить.",
                    false,
                )
            } else {
                (conflict.as_str(), true)
            };
            column = column.child(self.form_text(hint, danger));
        }
        column.into_any_element()
    }
}
