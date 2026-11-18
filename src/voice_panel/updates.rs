//! Update application for the voice panel. Kept apart from the panel struct
//! and its poll loop so the ownership rules stay reviewable.

use super::*;

impl VoicePanel {
    pub(crate) fn apply_update(
        &mut self,
        update: VoiceUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut repaint = true;
        match update {
            VoiceUpdate::Connecting => {
                self.phase = CallPhase::Connecting;
                self.status = "Подключение…".into();
                self.applied_audio = None;
                self.controls.muted = true;
                self.release_push_to_talk();
            }
            VoiceUpdate::Connected => {
                if self.phase != CallPhase::Connected {
                    self.phase = CallPhase::Connected;
                    self.status = "Подключено".into();
                    self.refresh_capture();
                }
            }
            VoiceUpdate::Reconnecting => {
                self.phase = CallPhase::Reconnecting;
                self.status = "Восстанавливаем соединение…".into();
                self.applied_audio = None;
                self.controls.muted = true;
                self.release_push_to_talk();
                // Go drops the confirmed roster until the fresh join; a
                // stale list must not look like a live call during a
                // reconnect.
                self.participants.clear();
                self.clear_speaking(cx);
                self.tracks.clear();
            }
            VoiceUpdate::Participants(participants) => {
                self.participants = participants;
                // The roster row for this user carries the server-authoritative
                // mute. It outranks the local toggle and a held PTT key.
                let self_user = self.self_user_id;
                if self_user != 0 {
                    if let Some(me) = self
                        .participants
                        .iter()
                        .find(|participant| participant.user_id == self_user)
                    {
                        if self.server_muted != me.server_mute {
                            self.server_muted = me.server_mute;
                            self.refresh_capture();
                        }
                    }
                }
                self.local_mutes.retain(|connection| {
                    self.participants
                        .iter()
                        .any(|participant| &participant.connection_id == connection)
                });
            }
            VoiceUpdate::Speaking(speaking) => {
                repaint = self.set_speaking(speaking, cx);
            }
            VoiceUpdate::LocalLevel(level) => {
                // Repaint the meter only for a visible step, not for every
                // 20 Hz sample of the capture track.
                repaint = (self.local_level - level).abs() > 0.04;
                self.local_level = level;
            }
            VoiceUpdate::Tracks(tracks) => self.tracks = tracks,
            VoiceUpdate::Streams(streams) => self.streams = streams,
            VoiceUpdate::StreamStatus { selection, status } => {
                if self.watching_selection != Some(selection) {
                    return false;
                }
                if status.is_empty() {
                    self.close_player_in_window(window);
                } else {
                    self.stream_status = status;
                    self.stream_error.clear();
                }
            }
            VoiceUpdate::StreamError { selection, error } => {
                if self.watching_selection != Some(selection) {
                    return false;
                }
                self.stream_error = error;
                self.stream_status.clear();
                self.stream_connected = false;
                self.frame = None;
            }
            VoiceUpdate::StreamAudio { selection, error } => {
                if self.watching_selection != Some(selection) {
                    return false;
                }
                // Keep the frame and the peer: the previous output may still be
                // playing after a successful rollback.
                self.stream_audio_error = error.unwrap_or_default();
            }
            VoiceUpdate::StreamConnected {
                selection,
                connected,
            } => {
                if self.watching_selection != Some(selection) {
                    return false;
                }
                self.stream_connected = connected;
            }
            VoiceUpdate::DeviceError(error) => {
                // A notice is not proof that the saved choice is dead: the
                // typed `InputApplied` result below is the only thing allowed
                // to replace `controls.input`. Clearing it here let a late
                // notice undo a newer explicit selection.
                self.device_error = error;
            }
            VoiceUpdate::InputApplied {
                generation,
                input,
                error,
            } => {
                // A result for an older explicit request must not overwrite a
                // newer selection. Zero is an engine-initiated fallback and is
                // always the current applied device.
                if generation != 0 {
                    if generation < self.input_generation {
                        return false;
                    }
                    self.input_generation = generation;
                }
                self.controls.input = input;
                // A typed success means the saved (or fallback) input opened:
                // an earlier hot-unplug notice is stale now.
                self.device_error = error.unwrap_or_default();
            }
            VoiceUpdate::AudioMode(applied) => {
                self.applied_audio = Some(applied);
            }
            VoiceUpdate::Playout(stats) => {
                self.playout = Some(stats);
            }
            VoiceUpdate::Ended(error) => {
                self.handle.take();
                self.applied_audio = None;
                self.server_muted = false;
                self.playout = None;
                self.controls.muted = true;
                self.ptt_held = false;
                self.ptt_combo = None;
                self.close_player_in_window(window);
                self.call_channel = 0;
                self.phase = if error.is_some() {
                    CallPhase::Failed
                } else {
                    CallPhase::Idle
                };
                self.error = error.unwrap_or_default();
                self.status.clear();
                self.participants.clear();
                self.clear_speaking(cx);
                self.tracks.clear();
                self.streams.clear();
            }
        }
        repaint
    }

    #[cfg(test)]
    pub fn test_apply_update(
        &mut self,
        update: VoiceUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.apply_update(update, window, cx)
    }
}
