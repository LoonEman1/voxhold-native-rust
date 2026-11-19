//! The Tab ring shared by the room toolbar, participant cards, the floating
//! player and the sidebar dock. Kept apart from `voice_panel.rs` so the panel
//! file stays within its size budget; the methods still read the panel's
//! private focus handles and painted-frame flags.

use super::*;

impl VoicePanel {
    pub(super) fn focus_ring(&self) -> Vec<(&'static str, FocusHandle)> {
        let mut ring = Vec::new();
        // Room entries exist only for elements the shell actually painted.
        // The room pane is hidden on a text channel, another room or a DM, and
        // its toolbar, join button, cards and streams are unmounted then.
        if self.room_painted {
            let active = self.call_channel != 0 && self.call_channel == self.room;
            if active {
                // The ring follows the painted toolbar: microphone, sound,
                // screen, records, leave. A focus entry exists only for a
                // painted action.
                for id in TOOLBAR_ACTIONS {
                    if id == "voice-screen" {
                        ring.push(("voice-screen", self.screen_focus[1].clone()));
                    } else if let Some(index) =
                        VOICE_ACTIONS.iter().position(|action| action.id == id)
                    {
                        ring.push((VOICE_ACTIONS[index].id, self.action_focus[index].clone()));
                    }
                }
                if matches!(
                    self.screen.sharing.phase,
                    voxhold_media::publisher::Phase::Publishing
                        | voxhold_media::publisher::Phase::Live
                        | voxhold_media::publisher::Phase::Stopping
                        | voxhold_media::publisher::Phase::Failed
                ) {
                    ring.push(("screen-stop", self.screen_focus[0].clone()));
                }
            } else {
                ring.push(("voice-join", self.join_focus.clone()));
            }
            for (index, publication) in self
                .room_streams()
                .iter()
                .filter(|publication| publication.channel_id == self.room)
                .take(self.stream_focus.len())
                .enumerate()
            {
                if self.call_active() && self.call_channel == self.room {
                    ring.push((
                        if self.watching.as_ref().is_some_and(|current| {
                            current.publisher_connection_id == publication.publisher_connection_id
                        }) {
                            "stream-full"
                        } else {
                            "stream-watch"
                        },
                        self.stream_focus[index].clone(),
                    ));
                }
            }
            let roster = self.visible_roster();
            let mut card = 0;
            for participant in roster {
                if !self.audio_for(&participant.connection_id) {
                    continue;
                }
                let Some(handle) = self.member_focus.get(card) else {
                    break;
                };
                ring.push(("voice-local", handle.clone()));
                card += 1;
            }
        }
        // The floating player is a shell-level overlay and is not tied to the
        // room pane, so it keeps its entries independently.
        if self.player_open {
            if self.has_audio() {
                ring.push(("stream-audio", self.player_actions[0].clone()));
            }
            ring.push(("stream-fullscreen", self.player_actions[1].clone()));
            ring.push(("stream-close", self.player_actions[2].clone()));
        }
        if self.dock_visible && self.call_channel != 0 {
            for (index, id) in DOCK_ACTIONS.iter().enumerate() {
                ring.push((dock_focus_id(id), self.dock_focus[index].clone()));
            }
        }
        ring
    }

    pub fn cycle_focus(
        &mut self,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let ring = self.focus_ring();
        if ring.is_empty() {
            return false;
        }
        let current = ring
            .iter()
            .position(|(_, handle)| handle.is_focused(window));
        let next = match current {
            None => 0,
            Some(index) => {
                if shift {
                    (index + ring.len() - 1) % ring.len()
                } else {
                    (index + 1) % ring.len()
                }
            }
        };
        window.focus(&ring[next].1);
        cx.notify();
        true
    }

    #[cfg(test)]
    pub fn focused_action(&self, window: &Window) -> Option<&'static str> {
        self.focus_ring()
            .into_iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(id, _)| id)
    }
}

/// Focus id of a dock action. The room toolbar owns `voice-mic` and friends, so
/// the dock entries are named separately even when both are painted.
fn dock_focus_id(action_id: &str) -> &'static str {
    match action_id {
        "voice-mic" => "dock-voice-mic",
        "voice-deaf" => "dock-voice-deaf",
        _ => "dock-voice-leave",
    }
}
