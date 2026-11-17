//! Voice room, sidebar dock and the floating stream player.
//!
//! The panel owns the call attempt: confirmed room access, the current
//! `VoiceHandle`, the roster, local mutes and the stream player. Viewing a room
//! and being in a call are separate: the room can show another channel while
//! the call keeps running in the dock.

use crate::voice_room::{
    self, CallPhase, Rect, Tone, VoiceAction, PLAYER_DEFAULT_WIDTH, VOICE_ACTIONS,
};
use gpui::{
    div, img, prelude::*, px, rgb, Context, FocusHandle, MouseButton, RenderImage, Subscription,
    Window,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use voxhold_api::Client;
use voxhold_media::{
    controller::{Participant, Publication},
    runtime::{VoiceHandle, VoiceUpdate},
    Controls,
};

const SURFACE: u32 = 0x1e1e1b;
const LINE: u32 = 0x363630;
const FG: u32 = 0xf3f4eb;
const MUTED: u32 = 0x9a9a90;
const LIME: u32 = 0x72b790;
const DANGER: u32 = 0xe06b5f;
const DARK: u32 = 0x10100f;

gpui::actions!(voice_keys, [VoiceTab, VoiceTabPrev]);

/// What the app confirmed about a voice channel before the room can be joined.
#[derive(Clone, Debug)]
pub struct RoomAccess {
    pub name: String,
    pub readable: bool,
    pub e2ee: bool,
}

impl Default for RoomAccess {
    fn default() -> Self {
        Self {
            name: String::new(),
            readable: false,
            e2ee: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragKind {
    Move,
    Resize,
}

#[derive(Clone, Copy, Debug)]
struct PlayerDrag {
    kind: DragKind,
    offset_x: f32,
    offset_y: f32,
}

pub struct VoicePanel {
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    client: Client,
    server: i64,
    room: i64,
    rooms: HashMap<i64, RoomAccess>,
    self_user_id: i64,
    members: HashMap<i64, String>,
    handle: Option<VoiceHandle>,
    attempt: u64,
    call_channel: i64,
    phase: CallPhase,
    status: String,
    error: String,
    controls: Controls,
    self_muted: bool,
    push_to_talk: bool,
    ptt_held: bool,
    ptt_blocked: bool,
    participants: Vec<Participant>,
    room_roster: Vec<Participant>,
    speaking: HashMap<String, bool>,
    tracks: HashSet<String>,
    local_mutes: HashSet<String>,
    local_level: f64,
    device_error: String,
    streams: Vec<Publication>,
    /// Publications of the displayed room from the account WebSocket; the
    /// call's runtime owns `streams` for the room it is in.
    room_streams: Vec<Publication>,
    frame: Option<Arc<RenderImage>>,
    stream_status: String,
    stream_error: String,
    stream_muted: bool,
    watching: Option<Publication>,
    player_open: bool,
    player_area: Rect,
    player_pos: (f32, f32),
    player_width: f32,
    player_placed: bool,
    player_fullscreen: bool,
    owns_fullscreen: bool,
    player_drag: Option<PlayerDrag>,
    join_focus: FocusHandle,
    action_focus: Vec<FocusHandle>,
    dock_focus: Vec<FocusHandle>,
    dock_visible: bool,
    stream_focus: Vec<FocusHandle>,
    member_focus: Vec<FocusHandle>,
    player_actions: Vec<FocusHandle>,
    room_size: (f32, f32),
    scale: f32,
    room_scroll: f32,
    recordings_focus: FocusHandle,
    recordings_handler: Option<Box<dyn Fn(&mut Window, &mut gpui::App)>>,
    recordings_live: Option<std::rc::Rc<std::cell::RefCell<Option<String>>>>,
}

impl VoicePanel {
    pub fn new(
        client: Client,
        server: i64,
        room: i64,
        push_to_talk: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |panel, _, _| panel.release_push_to_talk());
        let activation = cx.observe_window_activation(window, |panel, window, _| {
            if !window.is_window_active() {
                panel.release_push_to_talk();
            }
        });
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| loop {
            executor.timer(Duration::from_millis(100)).await;
            let Some(entity) = this.upgrade() else { break };
            if entity.update(cx, |panel, cx| panel.poll(cx)).is_err() {
                break;
            }
        })
        .detach();
        let controls = Controls {
            muted: true,
            ..Controls::default()
        };
        Self {
            focus,
            _subscriptions: vec![blur, activation],
            client,
            server,
            room,
            rooms: HashMap::new(),
            self_user_id: 0,
            members: HashMap::new(),
            handle: None,
            attempt: 0,
            call_channel: 0,
            phase: CallPhase::Idle,
            status: String::new(),
            error: String::new(),
            controls,
            self_muted: false,
            push_to_talk,
            ptt_held: false,
            ptt_blocked: false,
            participants: Vec::new(),
            room_roster: Vec::new(),
            speaking: HashMap::new(),
            tracks: HashSet::new(),
            local_mutes: HashSet::new(),
            local_level: 0.0,
            device_error: String::new(),
            streams: Vec::new(),
            room_streams: Vec::new(),
            frame: None,
            stream_status: String::new(),
            stream_error: String::new(),
            stream_muted: false,
            watching: None,
            player_open: false,
            player_area: Rect::default(),
            player_pos: (0.0, 0.0),
            player_width: PLAYER_DEFAULT_WIDTH,
            player_placed: false,
            player_fullscreen: false,
            owns_fullscreen: false,
            player_drag: None,
            join_focus: cx.focus_handle(),
            action_focus: (0..VOICE_ACTIONS.len())
                .map(|_| cx.focus_handle())
                .collect(),
            dock_focus: (0..VOICE_ACTIONS.len())
                .map(|_| cx.focus_handle())
                .collect(),
            dock_visible: false,
            stream_focus: (0..8).map(|_| cx.focus_handle()).collect(),
            member_focus: (0..16).map(|_| cx.focus_handle()).collect(),
            player_actions: (0..3).map(|_| cx.focus_handle()).collect(),
            room_size: (0.0, 0.0),
            scale: 1.0,
            room_scroll: 0.0,
            recordings_focus: cx.focus_handle(),
            recordings_handler: None,
            recordings_live: None,
        }
    }

    pub fn set_recordings_handler(&mut self, handler: Box<dyn Fn(&mut Window, &mut gpui::App)>) {
        self.recordings_handler = Some(handler);
    }

    pub fn set_recordings_live(&mut self, live: std::rc::Rc<std::cell::RefCell<Option<String>>>) {
        self.recordings_live = Some(live);
    }

    pub fn recordings_focus(&self) -> FocusHandle {
        self.recordings_focus.clone()
    }

    /// Root focus handle: the room receives Tab, Space and the player keys.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn recordings_button_mounted(&self) -> bool {
        self.handle.is_some()
    }

    /// The "Записи" action shared by the room toolbar and the sidebar dock.
    pub fn trigger_recordings(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handler) = &self.recordings_handler {
            handler(window, cx);
        }
    }

    /// Confirmed channel snapshot. The room may only join a readable voice
    /// channel that does not require E2EE.
    pub fn set_rooms(&mut self, rooms: HashMap<i64, RoomAccess>) {
        self.rooms = rooms;
    }

    pub fn set_self_user(&mut self, user_id: i64) {
        self.self_user_id = user_id;
    }

    pub fn set_members(&mut self, members: HashMap<i64, String>) {
        self.members = members;
    }

    /// Switch the displayed room without touching the active call.
    pub fn set_room(&mut self, room: i64, roster: Vec<Participant>) {
        if self.room != room {
            self.room = room;
            self.room_scroll = 0.0;
        }
        self.room_roster = roster;
    }

    pub fn set_room_roster(&mut self, room: i64, roster: Vec<Participant>) {
        if self.room == room {
            self.room_roster = roster;
        }
    }

    /// Publications of the displayed room, from the account WebSocket. They
    /// are informational until the user joins: watching requires the call.
    pub fn set_room_streams(&mut self, room: i64, streams: Vec<Publication>) {
        if self.room == room {
            self.room_streams = streams;
        }
    }

    pub fn room_streams(&self) -> &[Publication] {
        if self.call_channel != 0 && self.call_channel == self.room {
            &self.streams
        } else {
            &self.room_streams
        }
    }

    pub fn set_room_size(&mut self, width: f32, height: f32) {
        self.room_size = (width.max(1.0), height.max(1.0));
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.1);
    }

    pub fn room(&self) -> i64 {
        self.room
    }

    pub fn call_channel(&self) -> i64 {
        self.call_channel
    }

    pub fn call_active(&self) -> bool {
        self.call_channel != 0
    }

    #[cfg(test)]
    pub fn attempt(&self) -> u64 {
        self.attempt
    }

    pub fn phase(&self) -> CallPhase {
        self.phase
    }

    #[cfg(test)]
    pub fn status(&self) -> &str {
        &self.status
    }

    #[cfg(test)]
    pub fn error(&self) -> &str {
        &self.error
    }

    #[cfg(test)]
    pub fn test_controls_muted(&self) -> bool {
        self.controls.muted
    }

    #[cfg(test)]
    pub fn test_push_to_talk(&self) -> bool {
        self.push_to_talk
    }

    #[cfg(test)]
    pub fn test_ptt_held(&self) -> bool {
        self.ptt_held
    }

    #[cfg(test)]
    pub fn participant_ids(&self) -> Vec<String> {
        self.participants
            .iter()
            .map(|participant| participant.connection_id.clone())
            .collect()
    }

    /// Join the displayed room. Joining another room leaves the current call
    /// first, so no audio or video stays on the old lease.
    pub fn join(&mut self, cx: &mut Context<Self>) {
        let Some(access) = self.rooms.get(&self.room) else {
            self.error = "Нет доступа к голосовому каналу.".into();
            cx.notify();
            return;
        };
        if !access.readable {
            self.error = "Нет доступа к голосовому каналу.".into();
            cx.notify();
            return;
        }
        if access.e2ee {
            self.error = "Этот канал требует E2EE; голос пока недоступен.".into();
            cx.notify();
            return;
        }
        if self.handle.is_some() && self.call_channel != self.room {
            self.leave_call(cx);
        }
        if self.handle.is_some() {
            return;
        }
        match VoiceHandle::join(
            self.client.clone(),
            self.server,
            self.room,
            self.controls.clone(),
            self.self_muted,
        ) {
            Ok(handle) => {
                self.attempt = handle.attempt();
                self.handle = Some(handle);
                self.call_channel = self.room;
                self.phase = CallPhase::Connecting;
                self.status = "Подключение…".into();
                self.error.clear();
                self.participants.clear();
                self.speaking.clear();
                self.tracks.clear();
                self.local_mutes.clear();
                self.close_player();
                self.refresh_capture();
            }
            Err(error) => self.error = error,
        }
        cx.notify();
    }

    pub fn leave_in_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.exit_fullscreen(window);
        self.leave_call(cx);
    }

    /// Normal leave: stream session and voice session are both released and
    /// the room falls back to the join button.
    pub fn leave_call(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = &self.handle {
            if self.player_open {
                handle.leave_stream();
            }
            handle.leave();
        }
        self.handle.take();
        self.close_player();
        self.call_channel = 0;
        self.phase = CallPhase::Idle;
        self.status.clear();
        self.ptt_held = false;
        // The next join starts with a closed microphone until the server
        // confirms `voice.joined`, even if the previous call was unmuted.
        self.controls.muted = true;
        self.participants.clear();
        self.speaking.clear();
        self.tracks.clear();
        self.local_mutes.clear();
        self.streams.clear();
        self.local_level = 0.0;
        cx.notify();
    }

    /// The call was revoked, the user logged out or access changed: stop media
    /// and close the player without keeping a stale frame.
    pub fn abandon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.exit_fullscreen(window);
        self.leave_call(cx);
        self.room_roster.clear();
    }

    pub fn set_push_to_talk(&mut self, enabled: bool) {
        if self.push_to_talk != enabled {
            self.push_to_talk = enabled;
            self.ptt_held = false;
            self.refresh_capture();
        }
    }

    pub fn set_push_to_talk_held(&mut self, held: bool) {
        if self.ptt_blocked && held {
            return;
        }
        self.ptt_held = held;
        self.refresh_capture();
    }

    pub fn release_push_to_talk(&mut self) {
        if self.ptt_held {
            self.ptt_held = false;
            self.refresh_capture();
        }
    }

    /// Modals and other surfaces that own the keyboard: pressing the PTT key
    /// there must not open the microphone, and opening one releases a hold.
    pub fn set_push_to_talk_blocked(&mut self, blocked: bool) {
        self.ptt_blocked = blocked;
        if blocked {
            self.release_push_to_talk();
        }
    }

    pub fn push_to_talk_blocked(&self) -> bool {
        self.ptt_blocked
    }

    fn refresh_capture(&mut self) {
        let joined = self.handle.is_some() && self.phase != CallPhase::Connecting;
        self.controls.muted = !voice_room::microphone_open(
            joined && self.call_channel != 0,
            self.self_muted,
            self.controls.deafened,
            self.push_to_talk,
            self.ptt_held,
            self.ptt_blocked,
        );
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }

    pub fn mute(&mut self, cx: &mut Context<Self>) {
        self.self_muted = !self.self_muted;
        self.refresh_capture();
        cx.notify();
    }

    pub fn deafen(&mut self, cx: &mut Context<Self>) {
        self.controls.deafened = !self.controls.deafened;
        self.refresh_capture();
        cx.notify();
    }

    pub fn local_mute(&mut self, connection: String, cx: &mut Context<Self>) {
        let muted = !self.local_mutes.remove(&connection);
        if muted {
            self.local_mutes.insert(connection.clone());
        }
        if let Some(handle) = &self.handle {
            handle.local_mute(connection, muted);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn is_local_muted(&self, connection: &str) -> bool {
        self.local_mutes.contains(connection)
    }

    /// Device selection from the settings tab. The engine switches the live
    /// path and keeps the previous one if the new device fails.
    pub fn set_devices(&mut self, input: Option<String>, output: Option<String>) {
        self.controls.input = input;
        self.controls.output = output;
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }

    /// Encoder bitrate from the settings tab.
    pub fn set_bitrate(&mut self, kbps: u32) {
        self.controls.bitrate_kbps = Some(kbps);
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }

    /// Room output gain from the settings tab (0–200 %).
    pub fn set_output_volume(&mut self, percent: i64) {
        self.controls.output_gain = Some(percent.clamp(0, 200) as f64 / 100.0);
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }

    #[cfg(test)]
    pub fn device_error(&self) -> &str {
        &self.device_error
    }

    #[cfg(test)]
    pub fn local_level(&self) -> f64 {
        self.local_level
    }

    /// Start watching the selected publication. The runtime leaves the previous
    /// attempt and waits for its acknowledgement.
    pub fn watch(&mut self, publication: Publication, cx: &mut Context<Self>) {
        if self.call_channel == 0 || publication.channel_id != self.call_channel {
            return;
        }
        self.player_open = true;
        self.stream_error.clear();
        self.stream_status = "Подключение…".into();
        self.frame = None;
        self.watching = Some(publication.clone());
        self.stream_muted = false;
        if let Some(handle) = &self.handle {
            handle.watch_stream(publication);
        }
        cx.notify();
    }

    pub fn leave_stream(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = &self.handle {
            handle.leave_stream();
        }
        self.close_player();
        cx.notify();
    }

    fn close_player(&mut self) {
        self.frame = None;
        self.watching = None;
        self.player_open = false;
        self.stream_status.clear();
        self.stream_error.clear();
        self.stream_muted = false;
        self.player_drag = None;
    }

    pub fn toggle_stream_mute(&mut self, cx: &mut Context<Self>) {
        self.stream_muted = !self.stream_muted;
        if let Some(handle) = &self.handle {
            handle.stream_mute(self.stream_muted);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn stream_muted(&self) -> bool {
        self.stream_muted
    }

    #[cfg(test)]
    pub fn player_open(&self) -> bool {
        self.player_open
    }

    /// The sidebar dock renders outside the room pane, but its focus handles
    /// belong to the same Tab ring.
    pub fn set_dock_visible(&mut self, visible: bool) {
        self.dock_visible = visible;
    }

    pub fn dock_focus(&self, index: usize) -> FocusHandle {
        self.dock_focus[index.min(self.dock_focus.len() - 1)].clone()
    }

    pub fn action_icon_for(&self, action_id: &str) -> &'static str {
        match action_id {
            "voice-mic" if self.self_muted => "icons/mic-off.svg",
            "voice-mic" => "icons/mic.svg",
            "voice-deaf" if self.controls.deafened => "icons/headphones-off.svg",
            "voice-deaf" => "icons/headphones.svg",
            "voice-records" => "icons/recordings.svg",
            _ => "icons/logout.svg",
        }
    }

    /// `(mic, deafen, records, leave)` active flags for the dock buttons.
    pub fn action_active_flags(&self) -> [bool; 4] {
        [self.self_muted, self.controls.deafened, false, true]
    }

    pub fn player_fullscreen(&self) -> bool {
        self.player_fullscreen
    }

    #[cfg(test)]
    pub fn watching(&self) -> Option<&Publication> {
        self.watching.as_ref()
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        if let Some(player) = self.player_box() {
            let geometry = voice_room::player_geometry(self.player_area, player, self.has_audio());
            let picture = geometry.picture;
            let width = (picture.w * self.scale).max(1.0) as u32;
            let height = (picture.h * self.scale).max(1.0) as u32;
            if let Some(handle) = &self.handle {
                handle.set_viewport(width, height);
            }
        }
        let Some(handle) = &self.handle else {
            return;
        };
        let mut changed = false;
        let mut updates = Vec::new();
        while let Some(update) = handle.try_update() {
            updates.push(update);
        }
        let frame = handle.take_frame();
        for update in updates {
            let mut repaint = true;
            match update {
                VoiceUpdate::Connecting => {
                    self.phase = CallPhase::Connecting;
                    self.status = "Подключение…".into();
                    self.controls.muted = true;
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
                    self.controls.muted = true;
                }
                VoiceUpdate::Participants(participants) => {
                    self.participants = participants;
                    self.local_mutes.retain(|connection| {
                        self.participants
                            .iter()
                            .any(|participant| &participant.connection_id == connection)
                    });
                }
                VoiceUpdate::Speaking(speaking) => self.speaking = speaking,
                VoiceUpdate::LocalLevel(level) => {
                    // Repaint the meter only for a visible step, not for every
                    // 20 Hz sample of the capture track.
                    repaint = (self.local_level - level).abs() > 0.04;
                    self.local_level = level;
                }
                VoiceUpdate::Tracks(tracks) => self.tracks = tracks,
                VoiceUpdate::Streams(streams) => self.streams = streams,
                VoiceUpdate::StreamStatus(status) => {
                    if status.is_empty() {
                        self.close_player();
                    } else {
                        self.stream_status = status;
                        self.stream_error.clear();
                    }
                }
                VoiceUpdate::StreamError(error) => {
                    self.stream_error = error;
                    self.stream_status.clear();
                    self.frame = None;
                }
                VoiceUpdate::DeviceError(error) => self.device_error = error,
                VoiceUpdate::Error(error) => {
                    self.error = error;
                    self.phase = CallPhase::Failed;
                    self.controls.muted = true;
                    self.close_player();
                }
                VoiceUpdate::Left => {
                    self.handle.take();
                    self.controls.muted = true;
                    self.close_player();
                    self.call_channel = 0;
                    self.phase = CallPhase::Idle;
                    self.status.clear();
                    self.participants.clear();
                    self.speaking.clear();
                    self.tracks.clear();
                    self.streams.clear();
                }
            }
            changed |= repaint;
        }
        if let Some(frame) = frame {
            if let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
            {
                self.frame = Some(Arc::new(RenderImage::new(smallvec::smallvec![
                    image::Frame::new(buffer)
                ])));
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    // ---- focus ring -----------------------------------------------------

    fn focus_ring(&self) -> Vec<(&'static str, FocusHandle)> {
        let mut ring = Vec::new();
        let active = self.call_channel != 0 && self.call_channel == self.room;
        if active {
            for (index, action) in VOICE_ACTIONS.iter().enumerate() {
                ring.push((action.id, self.action_focus[index].clone()));
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
        if self.player_open {
            if self.has_audio() {
                ring.push(("stream-audio", self.player_actions[0].clone()));
            }
            ring.push(("stream-fullscreen", self.player_actions[1].clone()));
            ring.push(("stream-close", self.player_actions[2].clone()));
        }
        if self.dock_visible && self.call_channel != 0 {
            for (index, action) in VOICE_ACTIONS.iter().enumerate() {
                ring.push((action.id, self.dock_focus[index].clone()));
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

    /// Test hook: install a confirmed call without opening a socket, so the
    /// room, dock and keyboard can be tested deterministically.
    #[cfg(test)]
    pub fn test_set_call(
        &mut self,
        channel: i64,
        phase: CallPhase,
        participants: Vec<Participant>,
        tracks: Vec<String>,
    ) {
        self.call_channel = channel;
        self.phase = phase;
        self.participants = participants;
        self.tracks = tracks.into_iter().collect();
    }

    #[cfg(test)]
    pub fn test_set_streams(&mut self, streams: Vec<Publication>) {
        self.streams = streams;
    }

    #[cfg(test)]
    pub fn test_open_player(&mut self, publication: Publication) {
        self.player_open = true;
        self.stream_status = "Подключено".into();
        self.watching = Some(publication);
    }

    #[cfg(test)]
    pub fn test_set_fullscreen(&mut self, fullscreen: bool) {
        self.player_fullscreen = fullscreen;
    }

    #[cfg(test)]
    pub fn test_controls(&self) -> &Controls {
        &self.controls
    }

    #[cfg(test)]
    pub fn test_player_area(&self) -> Rect {
        self.player_area
    }

    #[cfg(test)]
    pub fn test_room_size(&self) -> (f32, f32) {
        self.room_size
    }

    fn visible_roster(&self) -> Vec<Participant> {
        if self.call_channel != 0 && self.call_channel == self.room {
            self.participants.clone()
        } else {
            self.room_roster.clone()
        }
    }

    fn audio_for(&self, connection: &str) -> bool {
        self.call_channel != 0 && self.call_channel == self.room && self.tracks.contains(connection)
    }

    fn member_name(&self, user_id: i64) -> String {
        self.members
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| format!("Участник {user_id}"))
    }
}

mod player;
mod view;
