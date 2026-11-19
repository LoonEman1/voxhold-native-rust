//! Voice room, sidebar dock and the floating stream player.
//!
//! The panel owns the call attempt: confirmed room access, the current
//! `VoiceHandle`, the roster, local mutes and the stream player. Viewing a room
//! and being in a call are separate: the room can show another channel while
//! the call keeps running in the dock.

use crate::theme::Palette;
use crate::voice_room::{
    self, CallPhase, Hotkey, Rect, Tone, VoiceAction, DOCK_ACTIONS, PLAYER_DEFAULT_WIDTH,
    TOOLBAR_ACTIONS, VOICE_ACTIONS,
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
    controller::{AppliedAudio, Participant, Publication},
    dsp::NoiseMode,
    runtime::{VoiceHandle, VoiceUpdate},
    Controls,
};

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
    /// Applied capture chain reported by the engine. Rendered as a diagnostic
    /// label in the active room; not part of the Go room layout.
    applied_audio: Option<AppliedAudio>,
    self_muted: bool,
    /// Server-authoritative mute of this user. It outranks the local toggle:
    /// the microphone stays closed even while a PTT key is held.
    server_muted: bool,
    /// The connection id this client was given on join. The roster may contain
    /// two connections of the same user, so server mute is found by this id.
    self_connection: Option<String>,
    /// Per-user output volume by stable user id (0–200 %). Persisted with the
    /// voice preferences, applied by the media mixer through track owners.
    user_volumes: std::collections::BTreeMap<i64, i64>,
    /// Last measured mixer level and counters.
    playout: Option<voxhold_media::mixer::PlayoutStats>,
    /// The system is suspended: the sink is released and the microphone is
    /// closed until the resume handler reopens them.
    power_paused: bool,
    push_to_talk: bool,
    ptt_held: bool,
    ptt_blocked: bool,
    /// Combination that opened the current hold. Releasing its main key or a
    /// required modifier closes the capture; auto-repeat cannot reopen it.
    ptt_combo: Option<Hotkey>,
    participants: Vec<Participant>,
    room_roster: Vec<Participant>,
    speaking: HashMap<String, bool>,
    tracks: HashSet<String>,
    local_mutes: HashSet<String>,
    local_level: f64,
    device_error: String,
    /// Newest explicit input request. A typed result for an older generation
    /// must not overwrite a newer choice; zero marks an engine-initiated
    /// fallback and is always current.
    input_generation: u64,
    streams: Vec<Publication>,
    /// Publications of the displayed room from the account WebSocket; the
    /// call's runtime owns `streams` for the room it is in.
    room_streams: Vec<Publication>,
    /// Permission Stream of the displayed room, confirmed by the channel list.
    can_stream: bool,
    /// Screen-share picker, on-air card and preview.
    screen: ScreenShareUi,
    /// Monotonic id of the newest screen-audio probe. A slower, older probe
    /// must not overwrite the availability of a newer dialog.
    screen_audio_probe: u64,
    /// False while the authoritative channel ACL is being reloaded. Both the
    /// room roster and the call worker's roster/streams stay hidden until the
    /// fresh channel list confirms access again.
    access_confirmed: bool,
    frame: Option<Arc<RenderImage>>,
    stream_status: String,
    stream_error: String,
    /// Audio-output notice for the player. Unlike a stream failure it keeps the
    /// decoded picture and the call alive.
    stream_audio_error: String,
    stream_muted: bool,
    /// Monotonic selection id. Every frame and status is tagged with it so a
    /// late update from A can never touch the player showing B.
    stream_selection: u64,
    watching_selection: Option<u64>,
    stream_connected: bool,
    watching: Option<Publication>,
    player_open: bool,
    player_area: Rect,
    player_pos: (f32, f32),
    player_width: f32,
    player_placed: bool,
    player_fullscreen: bool,
    owns_fullscreen: bool,
    /// Physical size last handed to the media worker. Repeated renders must not
    /// rewrite the same slot, but a real change is never skipped.
    requested_viewport: Option<(u32, u32)>,
    player_drag: Option<PlayerDrag>,
    join_focus: FocusHandle,
    action_focus: Vec<FocusHandle>,
    dock_focus: Vec<FocusHandle>,
    dock_visible: bool,
    stream_focus: Vec<FocusHandle>,
    /// Focus handles of the «Экран» toolbar action and the on-air actions.
    /// Index 0 is the on-air stop action, index 1 the toolbar button, index 2
    /// the live quality action.
    screen_focus: Vec<FocusHandle>,
    /// Focus handles of the source picker. A separate range cannot collide
    /// with the toolbar (`screen_focus[1]`) or the on-air card (`screen_focus[0]`).
    picker_focus: Vec<FocusHandle>,
    member_focus: Vec<FocusHandle>,
    player_actions: Vec<FocusHandle>,
    room_size: (f32, f32),
    scale: f32,
    room_scroll: f32,
    recordings_focus: FocusHandle,
    recordings_handler: Option<Box<dyn Fn(&mut Window, &mut gpui::App)>>,
    recordings_live: Option<std::rc::Rc<std::cell::RefCell<Option<String>>>>,
    /// Called when the detected speech set changes. The channel sidebar is
    /// drawn by the parent view; GPUI repaints ancestors while the panel is
    /// mounted, but the dock-only case and the sidebar rings need an explicit
    /// wake-up.
    speaking_handler: Option<Box<dyn Fn(&mut gpui::App)>>,
    /// Called after the call starts or ends. The parent uses it to start/stop
    /// the power poller so an idle window has no timer.
    call_handler: Option<Box<dyn Fn(&mut gpui::App)>>,
    palette: Palette,
    /// Test-only gate override: the capture stack and codec of the host must
    /// not decide whether a keyboard-path test can run.
    #[cfg(test)]
    test_share_gate: Option<Result<(), &'static str>>,
    /// Test-only replacement for the blocking endpoint probe. It runs on the
    /// same background task as the real one.
    #[cfg(test)]
    test_audio_probe: Option<
        Arc<dyn Fn() -> Result<voxhold_media::system_audio::AudioDeviceInfo, String> + Send + Sync>,
    >,
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
        cx.spawn_in(window, async move |this, cx| loop {
            executor.timer(Duration::from_millis(100)).await;
            let Some(entity) = this.upgrade() else { break };
            if entity
                .update_in(cx, |panel, window, cx| panel.poll(window, cx))
                .is_err()
            {
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
            applied_audio: None,
            self_muted: false,
            server_muted: false,
            self_connection: None,
            user_volumes: std::collections::BTreeMap::new(),
            playout: None,
            power_paused: false,
            push_to_talk,
            ptt_held: false,
            ptt_blocked: false,
            ptt_combo: None,
            participants: Vec::new(),
            room_roster: Vec::new(),
            speaking: HashMap::new(),
            tracks: HashSet::new(),
            local_mutes: HashSet::new(),
            local_level: 0.0,
            device_error: String::new(),
            input_generation: 0,
            streams: Vec::new(),
            room_streams: Vec::new(),
            can_stream: false,
            screen: ScreenShareUi::default(),
            screen_audio_probe: 0,
            access_confirmed: true,
            frame: None,
            stream_status: String::new(),
            stream_error: String::new(),
            stream_audio_error: String::new(),
            stream_muted: false,
            stream_selection: 0,
            watching_selection: None,
            stream_connected: false,
            watching: None,
            player_open: false,
            player_area: Rect::default(),
            player_pos: (0.0, 0.0),
            player_width: PLAYER_DEFAULT_WIDTH,
            player_placed: false,
            player_fullscreen: false,
            owns_fullscreen: false,
            requested_viewport: None,
            player_drag: None,
            join_focus: cx.focus_handle(),
            action_focus: (0..VOICE_ACTIONS.len())
                .map(|_| cx.focus_handle())
                .collect(),
            dock_focus: (0..DOCK_ACTIONS.len()).map(|_| cx.focus_handle()).collect(),
            dock_visible: false,
            stream_focus: (0..8).map(|_| cx.focus_handle()).collect(),
            screen_focus: (0..3).map(|_| cx.focus_handle()).collect(),
            picker_focus: (0..32).map(|_| cx.focus_handle()).collect(),
            member_focus: (0..16).map(|_| cx.focus_handle()).collect(),
            player_actions: (0..3).map(|_| cx.focus_handle()).collect(),
            room_size: (0.0, 0.0),
            scale: 1.0,
            room_scroll: 0.0,
            recordings_focus: cx.focus_handle(),
            recordings_handler: None,
            recordings_live: None,
            speaking_handler: None,
            call_handler: None,
            palette: crate::theme::dark_palette(),
            #[cfg(test)]
            test_share_gate: None,
            #[cfg(test)]
            test_audio_probe: None,
        }
    }

    /// Theme switch: the room repaints with the new tokens on the next frame.
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    fn colors(&self) -> VoiceColors {
        VoiceColors::resolve(self.palette)
    }

    pub fn set_recordings_handler(&mut self, handler: Box<dyn Fn(&mut Window, &mut gpui::App)>) {
        self.recordings_handler = Some(handler);
    }

    /// The parent view owns the channel sidebar; it repaints when the speech
    /// set changes so the speaking rings track the call.
    pub fn set_speaking_handler(&mut self, handler: Box<dyn Fn(&mut gpui::App)>) {
        self.speaking_handler = Some(handler);
    }

    pub fn set_call_handler(&mut self, handler: Box<dyn Fn(&mut gpui::App)>) {
        self.call_handler = Some(handler);
    }

    /// Tell the parent that the call started or ended. The handler defers its
    /// own update, so the parent is never mutably borrowed twice.
    fn notify_call_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(handler) = &self.call_handler {
            handler(cx);
        }
    }

    /// Replace the detected speech set. Returns whether anything changed; the
    /// parent is woken only for a real change.
    fn set_speaking(&mut self, speaking: HashMap<String, bool>, cx: &mut gpui::App) -> bool {
        if self.speaking == speaking {
            return false;
        }
        self.speaking = speaking;
        if let Some(handler) = &self.speaking_handler {
            handler(cx);
        }
        true
    }

    fn clear_speaking(&mut self, cx: &mut gpui::App) {
        self.set_speaking(HashMap::new(), cx);
    }

    /// Server mute is read from the roster row of *this* connection. A second
    /// connection of the same user must not mute or unmute this client.
    fn refresh_server_mute(&mut self) {
        let Some(connection) = self.self_connection.as_deref() else {
            return;
        };
        let Some(mute) = self
            .participants
            .iter()
            .find(|participant| participant.connection_id == connection)
            .map(|participant| participant.server_mute)
        else {
            return;
        };
        if self.server_muted != mute {
            self.server_muted = mute;
            self.refresh_capture();
        }
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

    /// Confirmed ACL for the selected server. A reload hides room data instead
    /// of letting a stale roster or the active call bypass the check.
    pub fn set_access_confirmed(&mut self, confirmed: bool) {
        self.access_confirmed = confirmed;
    }

    pub fn set_self_user(&mut self, user_id: i64) {
        self.self_user_id = user_id;
    }

    pub fn set_members(&mut self, members: HashMap<i64, String>) {
        self.members = members;
    }

    /// Switch the displayed room without touching the active call. Room,
    /// roster and publications are one operation: a setter must never validate
    /// against the old room and drop the new room's data.
    pub fn set_room_state(
        &mut self,
        room: i64,
        roster: Vec<Participant>,
        streams: Vec<Publication>,
    ) {
        if self.room != room {
            self.room = room;
            self.room_scroll = 0.0;
        }
        self.room_roster = roster;
        self.room_streams = streams;
    }

    pub fn room_streams(&self) -> &[Publication] {
        if !self.access_confirmed {
            return &[];
        }
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
        // A DPI change moves the same window rectangle to different physical
        // pixels, so the decoder must be told before the next frame arrives.
        self.sync_viewport();
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

    /// Whether the media runtime currently detects speech on this connection.
    /// Only the room of the active call is analysed, so other rooms answer
    /// false; the sidebar uses this for the same ring as the room cards.
    pub fn speaking(&self, connection_id: &str) -> bool {
        self.speaking.get(connection_id).copied().unwrap_or(false)
    }

    #[cfg(test)]
    pub fn attempt(&self) -> u64 {
        self.attempt
    }

    #[cfg(test)]
    pub fn test_palette(&self) -> Palette {
        self.palette
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
    pub fn test_self_muted(&self) -> bool {
        self.self_muted
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
        if !self.access_confirmed {
            self.error = "Права на голосовой канал ещё не подтверждены.".into();
            cx.notify();
            return;
        }
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
                self.clear_speaking(cx);
                self.tracks.clear();
                self.local_mutes.clear();
                self.close_player();
                self.refresh_capture();
                self.notify_call_changed(cx);
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
            if let Some(selection) = self.watching_selection {
                handle.leave_stream(selection);
            }
            handle.leave();
        }
        self.handle.take();
        self.close_player();
        self.reset_screen();
        self.call_channel = 0;
        self.phase = CallPhase::Idle;
        self.status.clear();
        self.error.clear();
        self.ptt_held = false;
        self.ptt_combo = None;
        self.applied_audio = None;
        self.server_muted = false;
        self.self_connection = None;
        self.playout = None;
        self.power_paused = false;
        // The next join starts with a closed microphone until the server
        // confirms `voice.joined`, even if the previous call was unmuted.
        self.controls.muted = true;
        self.participants.clear();
        self.clear_speaking(cx);
        self.tracks.clear();
        self.local_mutes.clear();
        self.streams.clear();
        self.local_level = 0.0;
        self.notify_call_changed(cx);
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
            self.ptt_combo = None;
            self.refresh_capture();
        }
    }

    /// A fresh key-down with all required modifiers opens the hold and records
    /// the combination. Auto-repeat is rejected by the caller.
    pub fn activate_push_to_talk(&mut self, combo: Hotkey) {
        if self.ptt_blocked || !self.push_to_talk || self.ptt_held {
            return;
        }
        self.ptt_combo = Some(combo);
        self.ptt_held = true;
        self.refresh_capture();
    }

    /// Test entry point: a bare hold without a recorded combo.
    #[cfg(test)]
    pub fn set_push_to_talk_held(&mut self, held: bool) {
        if held {
            self.activate_push_to_talk(Hotkey::default());
        } else {
            self.release_push_to_talk();
        }
    }

    /// Close the hold when either the main key or a required modifier is
    /// released. Returns whether the event belonged to the active combination.
    pub fn release_push_to_talk_key(&mut self, key: &str) -> bool {
        let Some(combo) = self.ptt_combo.clone() else {
            return false;
        };
        if key.eq_ignore_ascii_case(&combo.key)
            || voice_room::modifier_key(key).is_some_and(|m| combo.uses(m))
        {
            self.release_push_to_talk();
            return true;
        }
        false
    }

    /// Windows reports a modifier transition even when the main key is still
    /// down. If a required modifier is gone, the combination is broken.
    pub fn release_push_to_talk_modifiers(
        &mut self,
        ctrl: bool,
        shift: bool,
        alt: bool,
        platform: bool,
    ) -> bool {
        let Some(combo) = self.ptt_combo.clone() else {
            return false;
        };
        if (combo.ctrl && !ctrl)
            || (combo.shift && !shift)
            || (combo.alt && !alt)
            || (combo.platform && !platform)
        {
            self.release_push_to_talk();
            return true;
        }
        false
    }

    pub fn release_push_to_talk(&mut self) {
        if self.ptt_held || self.ptt_combo.is_some() {
            self.ptt_held = false;
            self.ptt_combo = None;
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
        let joined = self.handle.is_some() && self.phase == CallPhase::Connected;
        // Local mute/PTT/deafen and the server mute are the only owners of
        // capture permission; input volume 0 is silence inside the DSP, not a
        // second mute flag. A suspended system keeps the microphone closed.
        self.controls.muted = self.power_paused
            || !voice_room::microphone_open(
                joined && self.call_channel != 0,
                self.self_muted,
                self.server_muted,
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

    /// Whether the local user muted this participant on their own device.
    pub fn is_local_muted(&self, connection: &str) -> bool {
        self.local_mutes.contains(connection)
    }

    /// Applied capture chain reported by the engine. `software` honours the
    /// DSP toggles; `platform` is the ADM fallback with forced NS/AGC.
    pub fn applied_audio(&self) -> Option<&AppliedAudio> {
        self.applied_audio.as_ref()
    }

    /// Device failure reported by the engine (for example a hot-unplug). The
    /// settings meter polls this and refreshes the device list.
    pub fn device_error(&self) -> &str {
        &self.device_error
    }

    /// Local speech level from the transmission (0..=1), used by the settings
    /// meter while the call is active.
    pub fn local_level(&self) -> f64 {
        self.local_level
    }

    /// Start watching the selected publication. The runtime leaves the previous
    /// attempt and waits for its acknowledgement.
    pub fn watch(&mut self, publication: Publication, cx: &mut Context<Self>) {
        if !self.access_confirmed
            || self.call_channel == 0
            || publication.channel_id != self.call_channel
        {
            return;
        }
        self.stream_selection = self.stream_selection.wrapping_add(1).max(1);
        let selection = self.stream_selection;
        self.player_open = true;
        self.stream_error.clear();
        self.stream_status = "Подключение…".into();
        self.stream_connected = false;
        self.frame = None;
        self.watching = Some(publication.clone());
        self.watching_selection = Some(selection);
        self.stream_muted = false;
        if let Some(handle) = &self.handle {
            handle.watch_stream(selection, publication);
        }
        cx.notify();
    }

    pub fn leave_stream(&mut self, cx: &mut Context<Self>) {
        let selection = self.stream_selection;
        if let Some(handle) = &self.handle {
            handle.leave_stream(selection);
        }
        self.watching_selection = None;
        self.close_player();
        cx.notify();
    }

    fn close_player(&mut self) {
        self.frame = None;
        self.watching = None;
        self.watching_selection = None;
        self.player_open = false;
        // The next player may be served by another handle; its first viewport
        // must reach that worker even if the size matches the closed player.
        self.requested_viewport = None;
        self.stream_status.clear();
        self.stream_error.clear();
        self.stream_audio_error.clear();
        self.stream_connected = false;
        self.stream_muted = false;
        self.player_drag = None;
    }

    fn close_player_in_window(&mut self, window: &mut Window) {
        if self.player_fullscreen || self.owns_fullscreen {
            self.exit_fullscreen(window);
        }
        self.close_player();
    }

    pub fn toggle_stream_mute(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.watching_selection else {
            return;
        };
        self.stream_muted = !self.stream_muted;
        if let Some(handle) = &self.handle {
            handle.stream_mute(selection, self.stream_muted);
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
            "voice-mic" if self.self_muted || self.server_muted => "icons/mic-off.svg",
            "voice-mic" => "icons/mic.svg",
            "voice-deaf" if self.controls.deafened => "icons/headphones-off.svg",
            "voice-deaf" => "icons/headphones.svg",
            "voice-records" => "icons/recordings.svg",
            _ => "icons/logout.svg",
        }
    }

    /// Active state of one action, shared by the room toolbar and the dock.
    pub fn action_active_flag(&self, action_id: &str) -> bool {
        match action_id {
            "voice-mic" => self.self_muted || self.server_muted,
            "voice-deaf" => self.controls.deafened,
            "voice-leave" => true,
            _ => false,
        }
    }

    pub fn player_fullscreen(&self) -> bool {
        self.player_fullscreen
    }

    #[cfg(test)]
    pub fn watching(&self) -> Option<&Publication> {
        self.watching.as_ref()
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_viewport();
        let preview_changed = self.take_screen_preview();
        let Some(handle) = &self.handle else {
            if preview_changed {
                cx.notify();
            }
            return;
        };
        let mut changed = preview_changed;
        let mut updates = Vec::new();
        while let Some(update) = handle.try_update() {
            updates.push(update);
        }
        let frame = handle.take_frame();
        for update in updates {
            changed |= self.apply_update(update, window, cx);
        }
        if let Some(frame) = frame {
            if self.watching_selection != Some(frame.selection) {
                // A frame that was already in the last-value slot when the
                // user switched selections belongs to the old player.
            } else if let Some(buffer) =
                image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
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
            // The ring follows the painted toolbar: microphone, sound, screen,
            // records, leave. A focus entry exists only for a painted action.
            for id in TOOLBAR_ACTIONS {
                if id == "voice-screen" {
                    ring.push(("voice-screen", self.screen_focus[1].clone()));
                } else if let Some(index) = VOICE_ACTIONS.iter().position(|action| action.id == id)
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

    #[cfg(test)]
    pub fn test_visible_roster_len(&self) -> usize {
        self.visible_roster().len()
    }

    #[cfg(test)]
    pub fn test_visible_streams_len(&self) -> usize {
        self.visible_streams().len()
    }

    #[cfg(test)]
    pub fn test_audio_for(&self, connection: &str) -> bool {
        self.audio_for(connection)
    }

    fn visible_roster(&self) -> Vec<Participant> {
        if !self.access_confirmed {
            return Vec::new();
        }
        if self.call_channel != 0 && self.call_channel == self.room {
            self.participants.clone()
        } else {
            self.room_roster.clone()
        }
    }

    fn audio_for(&self, connection: &str) -> bool {
        self.access_confirmed
            && self.call_channel != 0
            && self.call_channel == self.room
            && self.tracks.contains(connection)
    }

    fn member_name(&self, user_id: i64) -> String {
        self.members
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| format!("Участник {user_id}"))
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

mod colors;
mod controls;
mod player;
mod screen_share;
use screen_share::ScreenShareUi;
#[cfg(test)]
mod test_support;
mod updates;
mod view;

pub(crate) use colors::VoiceColors;
