use gpui::{
    div, img, prelude::*, px, rgb, Context, FocusHandle, RenderImage, Subscription, Window,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use voxhold_api::Client;
use voxhold_media::{
    controller::{Participant, Publication},
    devices::AudioDevice,
    runtime::{VoiceHandle, VoiceUpdate},
    Controls,
};

/// Caller must enforce voice-channel read access and reject E2EE-required channels.
pub struct VoicePanel {
    focus: FocusHandle,
    _focus_subscriptions: Vec<Subscription>,
    client: Client,
    server: i64,
    channel: i64,
    handle: Option<VoiceHandle>,
    controls: Controls,
    self_muted: bool,
    push_to_talk: bool,
    ptt_held: bool,
    participants: Vec<Participant>,
    speaking: HashMap<String, bool>,
    devices: Vec<AudioDevice>,
    show_devices: bool,
    streams: Vec<Publication>,
    frame: Option<Arc<RenderImage>>,
    stream_status: String,
    stream_muted: bool,
    stream_fullscreen: bool,
    owns_fullscreen: bool,
    local_mutes: HashSet<String>,
    status: String,
    recordings_focus: FocusHandle,
    recordings_handler: Option<Box<dyn Fn(&mut Window, &mut gpui::App)>>,
    recordings_live: Option<std::rc::Rc<std::cell::RefCell<Option<String>>>>,
}
impl VoicePanel {
    pub fn new(
        client: Client,
        server: i64,
        channel: i64,
        push_to_talk: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |s, _, _| s.set_push_to_talk_held(false));
        let activation = cx.observe_window_activation(window, |s, window, _| {
            if !window.is_window_active() {
                s.set_push_to_talk_held(false);
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
        // Push-to-talk starts muted: the first `VoiceHandle::join` must not
        // open the microphone before Space is held.
        let controls = Controls {
            muted: push_to_talk,
            ..Controls::default()
        };
        Self {
            focus,
            _focus_subscriptions: vec![blur, activation],
            client,
            server,
            channel,
            handle: None,
            controls,
            self_muted: false,
            push_to_talk,
            ptt_held: false,
            participants: Vec::new(),
            speaking: HashMap::new(),
            devices: Vec::new(),
            show_devices: false,
            streams: Vec::new(),
            frame: None,
            stream_status: String::new(),
            stream_muted: false,
            stream_fullscreen: false,
            owns_fullscreen: false,
            local_mutes: HashSet::new(),
            status: String::new(),
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
    fn join(&mut self, cx: &mut Context<Self>) {
        if self.handle.is_some() {
            return;
        }
        match VoiceHandle::join(
            self.client.clone(),
            self.server,
            self.channel,
            self.controls.clone(),
            self.self_muted,
        ) {
            Ok(handle) => {
                self.handle = Some(handle);
                self.status = "Подключение…".into();
            }
            Err(error) => self.status = error,
        }
        cx.notify();
    }
    pub fn leave_in_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.exit_fullscreen(window);
        self.leave(cx);
    }
    fn exit_fullscreen(&mut self, window: &mut Window) {
        if self.owns_fullscreen && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.owns_fullscreen = false;
        self.stream_fullscreen = false;
    }
    pub fn leave(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.handle.take() {
            handle.leave();
            drop(handle);
        }
        self.participants.clear();
        self.speaking.clear();
        self.streams.clear();
        self.frame = None;
        self.stream_status.clear();
        self.local_mutes.clear();
        self.status.clear();
        cx.notify();
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut ended = false;
        if let Some(handle) = &self.handle {
            if let Some(frame) = handle.take_frame() {
                let mut rgba = frame.rgba;
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
                if let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, rgba) {
                    self.frame = Some(Arc::new(RenderImage::new(smallvec::smallvec![
                        image::Frame::new(buffer)
                    ])));
                    changed = true;
                }
            }
            while let Some(update) = handle.try_update() {
                changed = true;
                match update {
                    VoiceUpdate::Connecting => self.status = "Подключение…".into(),
                    VoiceUpdate::Connected => self.status = "Голос подключён".into(),
                    VoiceUpdate::Participants(p) => self.participants = p,
                    VoiceUpdate::Speaking(s) => self.speaking = s,
                    VoiceUpdate::Error(e) => self.status = e,
                    VoiceUpdate::Left => ended = true,
                    VoiceUpdate::Streams(streams) => self.streams = streams,
                    VoiceUpdate::StreamStatus(status) => {
                        if status.is_empty() {
                            self.frame = None;
                        }
                        self.stream_status = status;
                    }
                }
            }
        }
        if ended {
            self.handle.take();
            self.participants.clear();
            self.speaking.clear();
        }
        if changed {
            cx.notify();
        }
    }
    pub fn set_devices(&mut self, input: Option<String>, output: Option<String>) {
        self.controls.input = input;
        self.controls.output = output;
        self.update_controls();
    }
    pub fn set_push_to_talk(&mut self, enabled: bool) {
        self.push_to_talk = enabled;
        self.ptt_held = false;
        self.refresh_capture();
    }
    pub fn set_push_to_talk_held(&mut self, held: bool) {
        self.ptt_held = held;
        self.refresh_capture();
    }
    fn refresh_capture(&mut self) {
        self.controls.muted = self.self_muted || (self.push_to_talk && !self.ptt_held);
        self.update_controls();
    }
    fn update_controls(&self) {
        if let Some(handle) = &self.handle {
            handle.controls(self.controls.clone(), self.self_muted);
        }
    }
    fn mute(&mut self, cx: &mut Context<Self>) {
        self.self_muted = !self.self_muted;
        self.refresh_capture();
        cx.notify();
    }
    fn deafen(&mut self, cx: &mut Context<Self>) {
        self.controls.deafened = !self.controls.deafened;
        self.update_controls();
        cx.notify();
    }
    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        self.show_devices = true;
        let task = cx.background_spawn(async { voxhold_media::devices::list_devices() });
        cx.spawn(async move |this, cx| {
            let devices = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    s.devices = devices;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn local_mute(&mut self, id: String, cx: &mut Context<Self>) {
        let muted = if self.local_mutes.remove(&id) {
            false
        } else {
            self.local_mutes.insert(id.clone());
            true
        };
        if let Some(handle) = &self.handle {
            handle.local_mute(id, muted);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn test_controls_muted(&self) -> bool {
        self.controls.muted
    }

    #[cfg(test)]
    pub fn test_push_to_talk(&self) -> bool {
        self.push_to_talk
    }
}
impl Render for VoicePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut roster = div().flex().flex_col().gap_2();
        for (index, p) in self.participants.iter().enumerate() {
            let id = p.connection_id.clone();
            let muted = self.local_mutes.contains(&id);
            let state = if p.reconnecting {
                "Переподключение"
            } else if p.self_deaf {
                "Звук выключен"
            } else if p.server_mute {
                "Микрофон отключён сервером"
            } else if p.self_mute {
                "Микрофон выключен"
            } else if self.speaking.get(&id).copied().unwrap_or(false) {
                "Говорит"
            } else {
                "В канале"
            };
            roster = roster.child(
                div()
                    .p_3()
                    .bg(rgb(0x1e1e1b))
                    .rounded_md()
                    .flex()
                    .justify_between()
                    .child(div().child(format!("Пользователь {} · {}", p.user_id, state)))
                    .child(
                        div()
                            .id(("voice-local-mute", index))
                            .cursor_pointer()
                            .on_click(cx.listener(move |s, _, _, cx| s.local_mute(id.clone(), cx)))
                            .child(if muted {
                                "Включить звук"
                            } else {
                                "Выключить звук"
                            }),
                    ),
            );
        }
        let mut streams = div().flex().flex_col().gap_2();
        for (index, publication) in self.streams.iter().cloned().enumerate() {
            streams = streams.child(
                div()
                    .id(("stream-watch", index))
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, _, cx| {
                        if let Some(handle) = &s.handle {
                            handle.watch_stream(publication.clone());
                        }
                        s.stream_status = "Подключение…".into();
                        cx.notify();
                    }))
                    .child(format!(
                        "Смотреть трансляцию пользователя {}",
                        self.streams[index].publisher_user_id
                    )),
            );
        }
        if !self.stream_status.is_empty() {
            streams = streams
                .child(
                    div()
                        .id("stream-fullscreen")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, window, cx| {
                            if s.stream_fullscreen {
                                s.exit_fullscreen(window);
                            } else {
                                s.stream_fullscreen = true;
                                if !window.is_fullscreen() {
                                    window.toggle_fullscreen();
                                    s.owns_fullscreen = true;
                                }
                            }
                            cx.notify();
                        }))
                        .child(if self.stream_fullscreen {
                            "Свернуть трансляцию"
                        } else {
                            "На весь экран"
                        }),
                )
                .child(self.stream_status.clone())
                .child(
                    div()
                        .id("stream-mute")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.stream_muted = !s.stream_muted;
                            if let Some(handle) = &s.handle {
                                handle.stream_mute(s.stream_muted);
                            }
                            cx.notify();
                        }))
                        .child(if self.stream_muted {
                            "Включить звук трансляции"
                        } else {
                            "Выключить звук трансляции"
                        }),
                )
                .child(
                    div()
                        .id("stream-leave")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, window, cx| {
                            s.exit_fullscreen(window);
                            if let Some(handle) = &s.handle {
                                handle.leave_stream();
                            }
                            s.frame = None;
                            s.stream_status.clear();
                            cx.notify();
                        }))
                        .child("Закрыть трансляцию"),
                );
        }
        if let Some(frame) = self.frame.clone() {
            streams = streams.child(img(frame).w_full().h(if self.stream_fullscreen {
                px(720.)
            } else {
                px(360.)
            }));
        }
        let mut devices = div().flex().flex_col().gap_2();
        if self.show_devices {
            devices = devices
                .child("Аудиоустройства")
                .child(
                    div()
                        .id("device-default-input")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.controls.input = Some(String::new());
                            s.update_controls();
                            cx.notify();
                        }))
                        .child("Микрофон по умолчанию"),
                )
                .child(
                    div()
                        .id("device-default-output")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.controls.output = Some(String::new());
                            s.update_controls();
                            cx.notify();
                        }))
                        .child("Динамики по умолчанию"),
                );
            for (index, device) in self.devices.iter().cloned().enumerate() {
                let label = format!(
                    "{}: {}",
                    if device.input {
                        "Микрофон"
                    } else {
                        "Динамики"
                    },
                    device.name
                );
                devices = devices.child(
                    div()
                        .id(("audio-device", index))
                        .cursor_pointer()
                        .on_click(cx.listener(move |s, _, _, cx| {
                            if device.input {
                                s.controls.input = Some(device.id.clone());
                            } else {
                                s.controls.output = Some(device.id.clone());
                            }
                            s.update_controls();
                            cx.notify();
                        }))
                        .child(label),
                );
            }
        }
        let connected = self.handle.is_some();
        let mut controls = div().flex().gap_3();
        if connected {
            controls = controls.child(
                div()
                    .id("voice-devices")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.refresh_devices(cx)))
                    .child("Устройства"),
            );
            controls = controls.child(
                div()
                    .id("voice-ptt")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.set_push_to_talk(!s.push_to_talk);
                        if s.push_to_talk {
                            window.focus(&s.focus);
                        }
                        cx.notify();
                    }))
                    .child(if self.push_to_talk {
                        "Режим: удерживайте пробел"
                    } else {
                        "Режим: голосовая активация"
                    }),
            );
            controls = controls
                .child(
                    div()
                        .id("voice-mic")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| s.mute(cx)))
                        .child(if self.self_muted {
                            "Включить микрофон"
                        } else {
                            "Выключить микрофон"
                        }),
                )
                .child(
                    div()
                        .id("voice-deafen")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| s.deafen(cx)))
                        .child(if self.controls.deafened {
                            "Включить звук"
                        } else {
                            "Выключить звук"
                        }),
                )
                .child(
                    div()
                        .id("voice-recordings")
                        .cursor_pointer()
                        .track_focus(&self.recordings_focus)
                        .tab_index(0)
                        .on_click(cx.listener(|s, _, window, cx| {
                            if let Some(handler) = &s.recordings_handler {
                                handler(window, cx);
                            }
                        }))
                        .child("Записи"),
                )
                .child(
                    div()
                        .id("voice-leave")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, window, cx| s.leave_in_window(window, cx)))
                        .child("Выйти"),
                );
        } else {
            controls = controls.child(
                div()
                    .id("voice-join")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.join(cx)))
                    .child("Подключиться"),
            );
        }
        let live_recording = self
            .recordings_live
            .as_ref()
            .and_then(|slot| slot.borrow().clone());
        div()
            .id("voice-panel")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, _, _| {
                if s.push_to_talk && event.keystroke.key == "space" {
                    s.set_push_to_talk_held(true);
                }
            }))
            .on_key_up(cx.listener(|s, event: &gpui::KeyUpEvent, _, _| {
                if event.keystroke.key == "space" {
                    s.set_push_to_talk_held(false);
                }
            }))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .text_color(rgb(0xf3f4eb))
            .child(div().text_size(px(18.)).child("Голосовой канал"))
            .child(self.status.clone())
            .when(live_recording.is_some(), |panel| {
                panel.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(0xff6b6b))
                        .child(format!(
                            "Идёт запись · {}",
                            live_recording.unwrap_or_default()
                        )),
                )
            })
            .child(controls)
            .child(devices)
            .child(roster)
            .child(streams)
    }
}
