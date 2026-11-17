//! Voice room lifecycle at the application level: confirmed room access,
//! remote rosters, the panel entity and its dock, and the keyboard binding
//! between the app and the panel.
//!
//! Viewing a room and being in a call are separate. Selecting another channel
//! never ends the call; only an explicit leave, logout, lost access or a
//! revoked session does.

#[cfg(feature = "voxhold-media")]
use super::*;
#[cfg(feature = "voxhold-media")]
use crate::voice_panel::RoomAccess;
#[cfg(feature = "voxhold-media")]
use crate::voice_room::{Hotkey, PlayerKey};
#[cfg(feature = "voxhold-media")]
use gpui::{img, prelude::*, px, rgb, Focusable};
#[cfg(feature = "voxhold-media")]
use std::collections::HashMap;

#[cfg(feature = "voxhold-media")]
impl Voxhold {
    /// Confirmed access for every voice channel of the selected server. Only
    /// readable channels without an E2EE requirement can be joined.
    pub(super) fn voice_access(&self) -> HashMap<i64, RoomAccess> {
        self.channels
            .iter()
            .filter(|channel| channel.kind == "voice")
            .map(|channel| {
                (
                    channel.id,
                    RoomAccess {
                        name: channel.name.clone(),
                        readable: channel.readable() && !channel.e2ee_required,
                        e2ee: channel.e2ee_required,
                    },
                )
            })
            .collect()
    }

    /// Snapshot data is only shown for rooms the confirmed channel list still
    /// allows. While an ACL reload is in flight the server's data is hidden;
    /// other servers keep their cache until they become current.
    pub(super) fn voice_channel_visible(&self, server: i64, channel: i64) -> bool {
        if server <= 0 || channel <= 0 {
            return false;
        }
        if self.voice_unconfirmed.contains(&server) {
            return false;
        }
        if server != self.server && self.server != 0 {
            return true;
        }
        // The account snapshot may arrive before the channel list; keep the
        // already-filtered events until the authoritative list prunes them.
        if self.channels.is_empty() {
            return true;
        }
        self.channels
            .iter()
            .any(|item| item.id == channel && item.kind == "voice" && item.readable())
    }

    fn voice_roster(
        &self,
        server: i64,
        channel: i64,
    ) -> Vec<voxhold_media::controller::Participant> {
        if !self.voice_channel_visible(server, channel) {
            return Vec::new();
        }
        self.voice_rooms
            .get(&(server, channel))
            .cloned()
            .unwrap_or_default()
    }

    fn voice_stream_list(
        &self,
        server: i64,
        channel: i64,
    ) -> Vec<voxhold_media::controller::Publication> {
        if !self.voice_channel_visible(server, channel) {
            return Vec::new();
        }
        self.voice_streams
            .get(&(server, channel))
            .cloned()
            .unwrap_or_default()
    }

    /// Badge count from the confirmed roster of a visible room.
    pub(super) fn voice_count(&self, channel: i64) -> usize {
        self.voice_roster(self.server, channel).len()
    }

    pub(super) fn voice_has_stream(&self, channel: i64) -> bool {
        !self.voice_stream_list(self.server, channel).is_empty()
    }

    /// Display a voice channel's room. The call panel entity is reused so the
    /// active call survives navigation; only the room shown changes.
    pub(super) fn open_voice_room(
        &mut self,
        channel: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let rooms = self.voice_access();
        let members: HashMap<i64, String> = self
            .members
            .iter()
            .map(|member| (member.user_id, member.username.clone()))
            .collect();
        let roster = self.voice_roster(self.server, channel);
        let streams = self.voice_stream_list(self.server, channel);
        let prefs = self.voice_settings.prefs.clone();
        if let Some(panel) = &self.voice {
            let self_user = self.self_user_id;
            panel.update(cx, |panel, cx| {
                panel.set_access_confirmed(!self.voice_unconfirmed.contains(&self.server));
                panel.set_rooms(rooms);
                panel.set_self_user(self_user);
                panel.set_members(members);
                panel.set_devices(Some(prefs.input.clone()), Some(prefs.output.clone()));
                panel.set_bitrate(prefs.bitrate as u32);
                panel.set_output_volume(prefs.output_volume);
                panel.set_room_state(channel, roster, streams);
                cx.notify();
            });
        } else {
            let self_user = self.self_user_id;
            let access_confirmed = !self.voice_unconfirmed.contains(&self.server);
            let push_to_talk = prefs.input_mode == "push_to_talk";
            let panel = cx.new(|cx| {
                let mut panel = voice_panel::VoicePanel::new(
                    client,
                    self.server,
                    channel,
                    push_to_talk,
                    window,
                    cx,
                );
                panel.set_access_confirmed(access_confirmed);
                panel.set_rooms(rooms);
                panel.set_self_user(self_user);
                panel.set_members(members);
                panel.set_devices(Some(prefs.input.clone()), Some(prefs.output.clone()));
                panel.set_bitrate(prefs.bitrate as u32);
                panel.set_output_volume(prefs.output_volume);
                panel.set_room_state(channel, roster, streams);
                panel
            });
            let weak_view = cx.entity().downgrade();
            let recordings_live = self.recordings_live.clone();
            panel.update(cx, |panel, _| {
                panel.set_recordings_live(recordings_live);
                panel.set_recordings_handler(Box::new(move |window, cx| {
                    let Some(_view) = weak_view.upgrade() else {
                        return;
                    };
                    let weak = weak_view.clone();
                    window.defer(cx, move |window, cx| {
                        if let Some(view) = weak.upgrade() {
                            view.update(cx, |view, cx| view.toggle_recordings("voice", window, cx));
                        }
                    });
                }));
            });
            self.voice = Some(panel);
        }
        self.voice_room = channel;
        if let Some(panel) = &self.voice {
            let focus = panel.read(cx).focus_handle();
            window.focus(&focus);
        }
        cx.notify();
    }

    /// Push a roster and fresh room access into the panel. Called after
    /// `voice.*` events and channel reloads.
    pub(super) fn sync_voice_panel(&mut self, cx: &mut Context<Self>) {
        // Keep caches for other servers, but drop every room this client can
        // no longer read after the authoritative channel list arrived.
        let allowed: std::collections::HashSet<(i64, i64)> = self
            .voice_rooms
            .keys()
            .chain(self.voice_streams.keys())
            .copied()
            .filter(|(server, channel)| {
                // An unconfirmed server is hidden, not deleted: its fresh
                // channel list decides later whether the room still exists.
                self.voice_unconfirmed.contains(server)
                    || self.voice_channel_visible(*server, *channel)
            })
            .collect();
        self.voice_rooms.retain(|key, _| allowed.contains(key));
        self.voice_streams.retain(|key, _| allowed.contains(key));
        let Some(panel) = &self.voice else {
            return;
        };
        let rooms = self.voice_access();
        let members: HashMap<i64, String> = self
            .members
            .iter()
            .map(|member| (member.user_id, member.username.clone()))
            .collect();
        let room = panel.read(cx).room();
        let roster = self.voice_roster(self.server, room);
        let streams = self.voice_stream_list(self.server, room);
        let self_user = self.self_user_id;
        let access_confirmed = !self.voice_unconfirmed.contains(&self.server);
        panel.update(cx, |panel, cx| {
            panel.set_access_confirmed(access_confirmed);
            panel.set_rooms(rooms);
            panel.set_self_user(self_user);
            panel.set_members(members);
            panel.set_room_state(room, roster, streams);
            cx.notify();
        });
    }

    /// Mirror the call channel and drop calls whose access was revoked. A
    /// channel deleted, made unreadable or switched to E2EE ends the media
    /// session before the stale lease can keep audio or video.
    pub(super) fn refresh_voice_call(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.voice.clone() else {
            self.voice_channel = 0;
            return;
        };
        let call = panel.read(cx).call_channel();
        self.voice_channel = call;
        if call == 0 {
            return;
        }
        let accessible = self.channels.iter().any(|channel| {
            channel.id == call
                && channel.kind == "voice"
                && channel.readable()
                && !channel.e2ee_required
        });
        if self.server != 0 && !accessible {
            panel.update(cx, |panel, cx| panel.abandon(window, cx));
            self.voice_channel = 0;
        }
    }

    /// End the call and drop the room entity on logout, server switch or a
    /// lost session.
    pub(super) fn abandon_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.voice.take() {
            panel.update(cx, |panel, cx| panel.leave_in_window(window, cx));
        }
        self.voice_channel = 0;
        self.voice_room = 0;
    }

    /// Sidebar dock while the call runs in another channel or conversation.
    pub(super) fn voice_dock_visible(&self, cx: &gpui::App) -> bool {
        let Some(panel) = &self.voice else {
            return false;
        };
        let panel = panel.read(cx);
        panel.call_channel() != 0
            && (self.conversation.is_some()
                || self.voice_room != panel.call_channel()
                || !self.voice_room_visible())
    }

    /// Compact 98 px dock under the channel list: call status, room name and
    /// the same four actions in the Go order.
    pub(super) fn voice_dock(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let voice = self.voice.clone()?;
        if !self.voice_dock_visible(cx) {
            if let Some(panel) = &self.voice {
                panel.update(cx, |panel, _| panel.set_dock_visible(false));
            }
            return None;
        }
        let panel = voice.read(cx);
        let (status, tone) = crate::voice_room::dock_status(panel.phase());
        let active = panel.action_active_flags();
        let name = self
            .channels
            .iter()
            .find(|channel| channel.id == panel.call_channel())
            .map(|channel| channel.name.clone())
            .unwrap_or_default();
        let focuses: Vec<_> = (0..4).map(|index| panel.dock_focus(index)).collect();
        let icons: Vec<_> = crate::voice_room::VOICE_ACTIONS
            .iter()
            .map(|action| panel.action_icon_for(action.id))
            .collect();
        voice.update(cx, |panel, _| panel.set_dock_visible(true));
        let color = match tone {
            crate::voice_room::Tone::Lime => 0x72b790,
            crate::voice_room::Tone::Danger => 0xe06b5f,
            crate::voice_room::Tone::Muted => 0x9a9a90,
        };
        let mut actions = div().h(px(31.)).flex().gap(px(5.));
        for (index, action) in crate::voice_room::VOICE_ACTIONS.iter().enumerate() {
            let id = action.id;
            actions = actions.child(
                div()
                    .id(("voice-dock-action", index))
                    .h_full()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(9.))
                    .cursor_pointer()
                    .bg(rgb(if active[index] { 0x2a1c1a } else { 0x1e1e1b }))
                    .border_1()
                    .border_color(rgb(if active[index] { 0xe06b5f } else { 0x363630 }))
                    .track_focus(&focuses[index])
                    .child(img(icons[index]).w(px(15.)).h(px(15.)))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        let Some(voice) = &view.voice else {
                            return;
                        };
                        voice.update(cx, |panel, cx| match id {
                            "voice-mic" => panel.mute(cx),
                            "voice-deaf" => panel.deafen(cx),
                            "voice-records" => panel.trigger_recordings(window, cx),
                            _ => panel.leave_in_window(window, cx),
                        });
                        view.refresh_voice_call(window, cx);
                    })),
            );
        }
        let mut dock = div()
            .id("voice-dock")
            .debug_selector(|| "voice-dock".into())
            .key_context("VoiceRoom")
            .h(px(crate::voice_room::DOCK_HEIGHT))
            .w_full()
            .bg(rgb(0x1a1a17))
            .border_t_1()
            .border_color(rgb(0x363630))
            .px(px(10.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(24.))
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(rgb(color)))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(color))
                            .child(status),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(20.))
                    .child(img("icons/headphones.svg").w(px(12.)).h(px(12.)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0x9a9a90))
                            .truncate()
                            .child(name),
                    ),
            )
            .child(div().h(px(7.)))
            .child(actions);
        if let Some(record) = self.recordings_live.borrow().clone() {
            dock = dock.child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(0xe06b5f))
                    .truncate()
                    .child(format!("Идёт запись · {record}")),
            );
        }
        Some(dock.into_any_element())
    }

    pub(super) fn voice_room_visible(&self) -> bool {
        self.conversation.is_none()
            && self.voice_room != 0
            && self.channel == self.voice_room
            && self
                .channels
                .iter()
                .any(|channel| channel.id == self.voice_room && channel.kind == "voice")
    }

    /// True when a modal or the image viewer owns the keyboard, so a PTT press
    /// must not open the microphone.
    pub(super) fn voice_keyboard_blocked(&self) -> bool {
        self.active_settings.is_some()
            || self.recordings_dialog.is_some()
            || self.media.viewer.is_some()
            || self.forward.is_some()
            || self.editing.is_some()
            || self.confirm_delete.is_some()
            || self.show_search
            || self.message_menu.is_some()
            || self.member_menu.is_some()
            || self.forward.is_some()
    }

    pub(super) fn sync_push_to_talk_block(&self, cx: &mut Context<Self>) {
        let blocked = self.voice_keyboard_blocked();
        if let Some(panel) = &self.voice {
            let current = panel.read(cx).push_to_talk_blocked();
            if current != blocked {
                panel.update(cx, |panel, _| panel.set_push_to_talk_blocked(blocked));
            }
        }
    }

    /// Keyboard routing for the voice surfaces. Returns whether the event was
    /// consumed. Go order: mute/deafen, then push-to-talk press.
    pub(super) fn voice_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(panel) = self.voice.clone() else {
            return false;
        };
        let key = event.keystroke.key.to_lowercase();
        let modifiers = event.keystroke.modifiers;
        let prefs = self.voice_settings.prefs.clone();
        let blocked = self.voice_keyboard_blocked();
        if key == "escape" {
            if panel.update(cx, |panel, _| panel.player_escape(window)) {
                return true;
            }
        }
        if blocked {
            return false;
        }
        if !panel.read(cx).call_active() {
            return false;
        }
        if prefs.input_mode == "push_to_talk" {
            let hotkey = Hotkey::parse(&prefs.push_to_talk);
            let editor_focused = self.composer.read(cx).focus_handle(cx).is_focused(window);
            if hotkey.matches(
                &key,
                modifiers.control,
                modifiers.shift,
                modifiers.alt,
                modifiers.platform,
            ) && !editor_focused
            {
                // Windows sets `is_held` on an auto-repeat. Only a fresh press
                // may open the hold; the repeat is consumed but ignored.
                if !event.is_held {
                    panel.update(cx, |panel, cx| {
                        panel.activate_push_to_talk(hotkey.clone());
                        cx.notify();
                    });
                }
                return true;
            }
        }
        if Hotkey::parse(&prefs.mute).matches(
            &key,
            modifiers.control,
            modifiers.shift,
            modifiers.alt,
            modifiers.platform,
        ) {
            panel.update(cx, |panel, cx| panel.mute(cx));
            return true;
        }
        if Hotkey::parse(&prefs.deafen).matches(
            &key,
            modifiers.control,
            modifiers.shift,
            modifiers.alt,
            modifiers.platform,
        ) {
            panel.update(cx, |panel, cx| panel.deafen(cx));
            return true;
        }
        let fullscreen = panel.read(cx).player_fullscreen();
        let fullscreen_focused = panel.read(cx).player_fullscreen_focused(window);
        match crate::voice_room::player_key(
            &key,
            modifiers.control,
            modifiers.shift,
            modifiers.alt,
            modifiers.platform,
            fullscreen,
            fullscreen_focused,
        ) {
            PlayerKey::ExitFullscreen => {
                panel.update(cx, |panel, _| {
                    panel.player_escape(window);
                });
                return true;
            }
            PlayerKey::ToggleFullscreen => {
                panel.update(cx, |panel, _| panel.player_key_f(window));
                return true;
            }
            PlayerKey::Ignore => {}
        }
        false
    }

    pub(super) fn voice_key_up(
        &mut self,
        event: &gpui::KeyUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(panel) = &self.voice else {
            return false;
        };
        let prefs = self.voice_settings.prefs.clone();
        if prefs.input_mode != "push_to_talk" {
            return false;
        }
        let key = event.keystroke.key.to_lowercase();
        let _ = window;
        panel.update(cx, |panel, _| panel.release_push_to_talk_key(&key))
    }

    /// A required modifier was released while the main key is still down.
    pub(super) fn voice_modifiers_changed(
        &mut self,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(panel) = &self.voice else {
            return false;
        };
        panel.update(cx, |panel, _| {
            panel.release_push_to_talk_modifiers(
                modifiers.control,
                modifiers.shift,
                modifiers.alt,
                modifiers.platform,
            )
        })
    }

    /// Mouse forwarding for the player drag/resize. The player can leave its
    /// own bounds while the pointer is captured by the window.
    pub(super) fn voice_pointer_moved(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = &self.voice else {
            return;
        };
        if !panel.read(cx).player_dragging() {
            return;
        }
        let (x, y) = (position.x.into(), position.y.into());
        panel.update(cx, |panel, cx| panel.player_pointer_moved(x, y, cx));
    }

    pub(super) fn voice_pointer_released(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = &self.voice {
            panel.update(cx, |panel, _| panel.player_pointer_up());
        }
    }
}
