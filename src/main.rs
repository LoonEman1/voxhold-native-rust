#![cfg_attr(windows, windows_subsystem = "windows")]
mod attachments;
mod auth;
mod background;
mod channel_settings;
mod inbox;
mod layout;
mod message_tools;
mod personal_settings;
mod search;
mod server_settings;
mod theme;
#[cfg(feature = "voxhold-media")]
mod voice_panel;
#[cfg(windows)]
mod window_chrome;
use gpui::{
    div, list, prelude::*, px, rgba, size, AnyElement, App, Application, Bounds, Context, Entity,
    ListAlignment, ListState, MouseButton, Window, WindowBounds, WindowOptions,
};
use gpui_component::{
    input::{InputState, TextInput},
    Root,
};
use layout::{SplitterDrag, SplitterSide};
use voxhold_api::{Channel, Client, Member, Message, Server};

struct Voxhold {
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
    instance: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    composer: Entity<InputState>,
    client: Option<Client>,
    self_user_id: i64,
    search: Option<Entity<search::SearchPanel>>,
    show_search: bool,
    message_tools: Option<Entity<message_tools::MessageTools>>,
    tools_revision: u64,
    show_tools: bool,
    attachments: Option<Entity<attachments::Attachments>>,
    attachment_pending: Option<(u64, String)>,
    settings: Option<Entity<personal_settings::PersonalSettings>>,
    show_settings: bool,
    channel_settings: Option<Entity<channel_settings::ChannelSettings>>,
    show_channel_settings: bool,
    server_settings: Option<Entity<server_settings::ServerSettings>>,
    show_server_settings: bool,
    inbox: Option<Entity<inbox::Inbox>>,
    show_inbox: bool,
    show_members: bool,
    show_channels: bool,
    #[cfg(feature = "voxhold-media")]
    voice: Option<Entity<voice_panel::VoicePanel>>,
    #[cfg(feature = "voxhold-media")]
    voice_channel: i64,
    servers: Vec<Server>,
    channels: Vec<Channel>,
    members: Vec<Member>,
    messages: Vec<Message>,
    history_list: ListState,
    history_keys: Vec<(i64, i64, i64)>,
    before: Option<i64>,
    has_more: bool,
    loading_older: bool,
    event_task: Option<tokio::task::JoinHandle<()>>,
    events: Option<
        std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<voxhold_api::RealtimeEvent>>>,
    >,
    events_overflow: std::sync::Arc<std::sync::atomic::AtomicBool>,
    events_version: u64,
    reads: std::collections::HashMap<i64, i64>,
    read_sent: i64,
    read_pending: bool,
    server: i64,
    channel: i64,
    error: String,
    busy: bool,
    generation: u64,
    drafts: std::collections::HashMap<(i64, i64), String>,
    auth: auth::AuthState,
    palette: theme::Palette,
    theme_mode: theme::ThemeMode,
    theme_path: Option<std::path::PathBuf>,
    system_light: bool,
    shell: layout::Settings,
    shell_path: Option<std::path::PathBuf>,
    shell_initialized: bool,
    drag: Option<SplitterDrag>,
    viewport: (f32, f32),
    auth_background: Option<(u32, u32, bool, std::sync::Arc<gpui::RenderImage>)>,
    initial_focus_done: bool,
    /// `(message_id, offset_in_item)` captured before a prepend so the visible
    /// row stays under the same pixel after older rows are merged.
    pending_anchor: Option<(i64, gpui::Pixels)>,
    /// Deleting a message is terminal: a late page or a repeated event must not
    /// bring the row back.
    deleted_ids: std::collections::HashSet<i64>,
    _activation_subscription: gpui::Subscription,
}
impl Voxhold {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        prefill: &str,
        shell_path: Option<std::path::PathBuf>,
    ) -> Self {
        let history_list = ListState::new(0, ListAlignment::Bottom, px(400.));
        let weak = cx.entity().downgrade();
        history_list.set_scroll_handler(move |event, window, cx| {
            if let Some(view) = weak.upgrade() {
                view.update(cx, |s, cx| {
                    s.mark_visible_read(event.visible_range.clone(), window, cx);
                    // Go loads the previous page when the reader reaches the
                    // top area instead of waiting for the explicit button.
                    if event.visible_range.start <= 3 && s.has_more && !s.loading_older {
                        s.load_older(cx);
                    }
                });
            }
        });
        let system_light = theme::system_light();
        let theme_path = theme::theme_path();
        let theme_mode = theme::load_mode(theme_path.as_deref());
        let shell = shell_path
            .as_deref()
            .map(layout::Settings::load)
            .unwrap_or_default();
        let view = Self {
            runtime: std::sync::Arc::new(tokio::runtime::Runtime::new().expect("Tokio runtime")),
            instance: cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com")),
            username: cx.new(|cx| InputState::new(window, cx).placeholder("например, neva")),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Минимум 8 символов")
                    .masked(true)
            }),
            composer: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line()
                    .placeholder("Написать сообщение")
            }),
            client: None,
            self_user_id: 0,
            search: None,
            show_search: false,
            message_tools: None,
            tools_revision: 0,
            show_tools: false,
            attachments: None,
            attachment_pending: None,
            settings: None,
            show_settings: false,
            channel_settings: None,
            show_channel_settings: false,
            server_settings: None,
            show_server_settings: false,
            inbox: None,
            show_inbox: false,
            show_members: false,
            show_channels: false,
            #[cfg(feature = "voxhold-media")]
            voice: None,
            #[cfg(feature = "voxhold-media")]
            voice_channel: 0,
            servers: vec![],
            channels: vec![],
            members: vec![],
            messages: vec![],
            history_list,
            history_keys: Vec::new(),
            before: None,
            has_more: false,
            loading_older: false,
            event_task: None,
            events: None,
            events_overflow: Default::default(),
            events_version: 0,
            reads: Default::default(),
            read_sent: 0,
            read_pending: false,
            server: 0,
            channel: 0,
            error: String::new(),
            busy: false,
            generation: 0,
            drafts: Default::default(),
            auth: auth::AuthState::with_prefill(prefill),
            palette: theme::Palette::resolve(theme_mode, system_light),
            theme_mode,
            theme_path,
            system_light,
            shell,
            shell_path,
            shell_initialized: false,
            drag: None,
            viewport: (0.0, 0.0),
            auth_background: None,
            initial_focus_done: false,
            pending_anchor: None,
            deleted_ids: Default::default(),
            _activation_subscription: cx.observe_window_activation(window, |s, window, cx| {
                if s.theme_mode == theme::ThemeMode::System {
                    let light = theme::system_light();
                    if light != s.system_light {
                        s.system_light = light;
                        s.palette = theme::Palette::resolve(s.theme_mode, light);
                        s.auth_background = None;
                        window_chrome::apply(window, s.palette.light);
                        cx.notify();
                    }
                }
            }),
        };
        window_chrome::apply(window, view.palette.light);
        view
    }
    /// First step: validate the instance address and move to the credential
    /// form. The address is kept exactly as confirmed so the operator can step
    /// back without retyping it.
    fn submit_instance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.instance.read(cx).value().to_string();
        if self.auth.submit_instance(&raw).is_some() {
            self.username
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Back to the address step; keeps the confirmed origin.
    #[allow(dead_code)]
    fn back_to_instance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth.busy {
            return;
        }
        self.auth.screen = auth::AuthScreen::Instance;
        self.auth.status.clear();
        self.instance.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn submit_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let user = self.username.read(cx).value().to_string();
        let pass = self.password.read(cx).value().to_string();
        let Some(request) = self.auth.submit_login(&user, &pass) else {
            cx.notify();
            return;
        };
        self.error.clear();
        cx.notify();
        let attempt = request.attempt;
        let origin = request.origin.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                let mut c = Client::new(&origin).map_err(|e| e.to_string())?;
                c.login(&request.username, &request.password)
                    .await
                    .map_err(|e| auth::AuthState::describe_error(&e))?;
                let servers = c
                    .servers()
                    .await
                    .map_err(|e| auth::AuthState::describe_error(&e))?;
                let self_user_id = match c.profile().await {
                    Ok(profile) => profile.user_id,
                    Err(error) => {
                        // Go retries the profile once before actions that need a
                        // confirmed account id.
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        c.profile()
                            .await
                            .map(|profile| profile.user_id)
                            .map_err(|_| auth::AuthState::describe_error(&error))?
                    }
                };
                Ok::<_, String>((c, servers, self_user_id))
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                let outcome = result.as_ref().map(|_| ()).map_err(|e| e.clone());
                if !s.auth.finish_login(attempt, &outcome) {
                    cx.notify();
                    return;
                }
                let Ok((c, servers, self_user_id)) = result else {
                    return;
                };
                s.password
                    .update(cx, |input, cx| input.set_value("", window, cx));
                let runtime = s.runtime.clone();
                let attachment_client = c.clone();
                let attachments = cx.new(|cx| {
                    attachments::Attachments::new(attachment_client, runtime, window, cx)
                });
                cx.observe_in(&attachments, window, |s, attachments, window, cx| {
                    let sent = attachments.update(cx, |a, _| a.take_sent());
                    if let Some(message) = sent {
                        if let Some((generation, text)) = s.attachment_pending.take() {
                            if generation == s.generation {
                                if s.composer.read(cx).value().as_ref() == text {
                                    s.composer
                                        .update(cx, |input, cx| input.set_value("", window, cx));
                                }
                                if !s.messages.iter().any(|m| m.id == message.id) {
                                    s.messages.push(message);
                                    s.messages.sort_by_key(|m| m.id);
                                }
                                if s.messages.len() > 1000 {
                                    s.messages.remove(0);
                                }
                            }
                        }
                        cx.notify();
                    }
                })
                .detach();
                s.attachments = Some(attachments);
                let runtime = s.runtime.clone();
                let settings_client = c.clone();
                s.settings = Some(cx.new(|cx| {
                    personal_settings::PersonalSettings::new(settings_client, runtime, window, cx)
                }));
                let runtime = s.runtime.clone();
                let server_admin_client = c.clone();
                s.server_settings = Some(cx.new(|cx| {
                    server_settings::ServerSettings::new(
                        server_admin_client,
                        runtime.clone(),
                        window,
                        cx,
                    )
                }));
                let admin_client = c.clone();
                s.channel_settings = Some(cx.new(|cx| {
                    channel_settings::ChannelSettings::new(admin_client, runtime, window, cx)
                }));
                let runtime = s.runtime.clone();
                let inbox_client = c.clone();
                s.inbox = Some(cx.new(|cx| inbox::Inbox::new(inbox_client, runtime, window, cx)));
                s.self_user_id = self_user_id;
                s.client = Some(c);
                s.servers = servers;
                s.auth.status = "Choose a server".into();
                // Go selects the first server after login; an empty list leaves
                // the workspace on its empty state.
                s.shell_initialized = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn logout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.event_task = None;
        self.events = None;
        self.client = None;
        self.servers.clear();
        self.channels.clear();
        self.members.clear();
        self.messages.clear();
        self.history_keys.clear();
        self.deleted_ids.clear();
        self.pending_anchor = None;
        self.history_list.reset(0);
        self.self_user_id = 0;
        self.server = 0;
        self.channel = 0;
        self.error.clear();
        self.busy = false;
        self.drafts.clear();
        self.auth = {
            let origin = self.auth.origin.clone();
            auth::AuthState::with_prefill(&origin)
        };
        self.auth.screen = auth::AuthScreen::Login;
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.instance.update(cx, |input, cx| {
            input.set_value(self.auth.origin.clone(), window, cx);
        });
        self.shell_initialized = false;
        self.initial_focus_done = false;
        cx.notify();
    }
    fn select_server(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.client.clone() else { return };
        if self.channel != 0 {
            self.drafts.insert(
                (self.server, self.channel),
                self.composer.read(cx).value().to_string(),
            );
        }
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        #[cfg(feature = "voxhold-media")]
        if self.server != id {
            if let Some(voice) = self.voice.take() {
                voice.update(cx, |voice, cx| voice.leave_in_window(window, cx));
            }
            self.voice_channel = 0;
        }
        self.server = id;
        self.attachment_pending = None;
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |a, cx| a.set_scope(id, 0, false, cx));
        }
        self.show_server_settings = false;
        let server_info = self.servers.iter().find(|s| s.id == id);
        let name = server_info.map(|s| s.name.clone()).unwrap_or_default();
        let owner = server_info.is_some_and(|s| s.role == "owner");
        if let Some(settings) = &self.server_settings {
            settings.update(cx, |settings, cx| {
                settings.set_scope(id, &name, owner, window, cx)
            });
        }
        self.channel = 0;
        let search_client = c.clone();
        let runtime = self.runtime.clone();
        self.search =
            Some(cx.new(|cx| search::SearchPanel::new(search_client, runtime, id, window, cx)));
        let tools_client = c.clone();
        let runtime = self.runtime.clone();
        let tools =
            cx.new(|cx| message_tools::MessageTools::new(tools_client, runtime, window, cx));
        cx.observe(&tools, |this, tools, cx| {
            let revision = tools.read(cx).revision();
            if revision != this.tools_revision {
                this.tools_revision = revision;
                this.refresh_history(cx);
            }
        })
        .detach();
        self.tools_revision = 0;
        self.message_tools = Some(tools);
        if self.inbox.is_none() {
            let client = c.clone();
            let runtime = self.runtime.clone();
            self.inbox = Some(cx.new(|cx| inbox::Inbox::new(client, runtime, window, cx)));
        }
        if self.settings.is_none() {
            let client = c.clone();
            let runtime = self.runtime.clone();
            let settings =
                cx.new(|cx| personal_settings::PersonalSettings::new(client, runtime, window, cx));
            self.settings = Some(settings);
        }
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        self.events = None;
        self.channels.clear();
        self.messages.clear();
        self.before = None;
        self.has_more = false;
        self.loading_older = false;
        self.members.clear();
        self.generation += 1;
        let generation = self.generation;
        self.error.clear();
        cx.notify();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async {
                let channels = c.channels(id).await.map_err(|e| e.to_string())?;
                let members = c.members(id).await.map_err(|e| e.to_string())?;
                Ok::<_, String>((channels, members))
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    match result {
                        Ok((channels, members)) => {
                            s.channels = channels;
                            s.members = members
                        }
                        Err(e) => s.error = e,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
    fn select_channel(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.client.clone() else { return };
        if self.channel != 0 {
            self.drafts.insert(
                (self.server, self.channel),
                self.composer.read(cx).value().to_string(),
            );
        }
        let draft = self
            .drafts
            .get(&(self.server, id))
            .cloned()
            .unwrap_or_default();
        self.composer
            .update(cx, |input, cx| input.set_value(draft, window, cx));
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        self.events = None;
        self.channel = id;
        self.attachment_pending = None;
        let writable = self.channels.iter().any(|ch| ch.id == id && ch.writable());
        if let Some(attachments) = &self.attachments {
            attachments.update(cx, |a, cx| a.set_scope(self.server, id, writable, cx));
        }
        self.reads.clear();
        self.read_sent = 0;
        self.read_pending = false;
        self.deleted_ids.clear();
        self.pending_anchor = None;
        self.show_tools = false;
        self.show_channel_settings = false;
        let role = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .map(|s| s.role.as_str())
            .unwrap_or("");
        let allowed = role == "owner" || role == "admin";
        let kind = self
            .channels
            .iter()
            .find(|ch| ch.id == id)
            .map(|ch| ch.kind.clone())
            .unwrap_or_default();
        if let Some(settings) = &self.channel_settings {
            settings.update(cx, |settings, cx| {
                settings.set_scope(self.server, id, &kind, allowed, cx)
            });
        }
        #[cfg(feature = "voxhold-media")]
        if self
            .channels
            .iter()
            .any(|ch| ch.id == id && ch.kind == "voice" && ch.readable() && !ch.e2ee_required)
            && self.voice_channel != id
        {
            if let Some(voice) = self.voice.take() {
                voice.update(cx, |voice, cx| voice.leave_in_window(window, cx));
            }
            let voice_client = c.clone();
            let server = self.server;
            self.voice = Some(
                cx.new(|cx| voice_panel::VoicePanel::new(voice_client, server, id, window, cx)),
            );
            self.voice_channel = id;
        }
        if let Some(tools) = &self.message_tools {
            tools.update(cx, |tools, cx| {
                tools.select(self.server, id, None, Default::default(), cx)
            });
        }
        if let Some(search) = &self.search {
            search.update(cx, |panel, cx| panel.set_scope(self.server, id, cx));
        }
        self.messages.clear();
        self.before = None;
        self.has_more = false;
        self.loading_older = false;
        self.generation += 1;
        let generation = self.generation;
        self.error.clear();
        let sid = self.server;
        if !self
            .channels
            .iter()
            .any(|ch| ch.id == id && ch.has_history())
        {
            cx.notify();
            return;
        }
        self.start_channel_events(c.clone(), sid, id, cx);
        let events_version = self.events_version;
        cx.notify();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(c.messages_page(sid, id, 0))
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    match result {
                        Ok(page) => {
                            s.before = page.pagination.next_before_id;
                            s.has_more = page.pagination.has_more;
                            if s.events_version != events_version {
                                s.refresh_history(cx);
                                return;
                            }
                            let mut current = std::mem::take(&mut s.messages);
                            s.messages = page.messages;
                            s.messages
                                .retain(|m| !current.iter().any(|newer| newer.id == m.id));
                            s.messages.append(&mut current);
                            s.messages.sort_by_key(|m| m.id);
                        }
                        Err(e) => s.error = e,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
    fn start_channel_events(
        &mut self,
        client: Client,
        server: i64,
        channel: i64,
        cx: &mut Context<Self>,
    ) {
        let (tx, rx) = std::sync::mpsc::sync_channel(128);
        let overflow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.events_overflow = overflow.clone();
        self.events = Some(std::sync::Arc::new(std::sync::Mutex::new(rx)));
        self.event_task = Some(self.runtime.spawn(async move {
            loop {
                let sender = tx.clone();
                let overflow = overflow.clone();
                let _ = client
                    .subscribe(server, channel, move |event| {
                        if sender.try_send(event).is_err() {
                            overflow.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    })
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }));
        self.schedule_events(self.generation, cx);
    }
    fn schedule_events(&self, generation: u64, cx: &mut Context<Self>) {
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(150));
        cx.spawn(async move |this, cx| {
            timer.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation || s.events.is_none() {
                        return;
                    }
                    let events: Vec<_> = s
                        .events
                        .as_ref()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .try_iter()
                        .collect();
                    let changed = !events.is_empty();
                    if changed {
                        s.events_version = s.events_version.wrapping_add(1);
                    }
                    let mut resync = s
                        .events_overflow
                        .swap(false, std::sync::atomic::Ordering::Relaxed);
                    for event in events {
                        if s.apply_realtime_event(&event.kind, &event.data) {
                            resync = true;
                        }
                    }
                    if resync {
                        s.refresh_history(cx);
                    }
                    if changed || resync {
                        cx.notify();
                    }
                    s.schedule_events(generation, cx);
                })
                .ok();
            }
        })
        .detach();
    }
    /// Apply one realtime event to the active scope. Returns `true` when an
    /// authoritative history refresh is required.
    ///
    /// The merge policy is the part that regresses most easily, so it lives in
    /// its own method with unit coverage: stale revisions never overwrite a
    /// confirmed row, a tombstone wins over late `created`/`updated` events and
    /// a repeated `created` for an existing id is an update.
    fn apply_realtime_event(&mut self, kind: &str, data: &serde_json::Value) -> bool {
        let mut resync = false;
        match kind {
            "channel.subscribed" | "channel.sync_required" => resync = true,
            "channel.read_snapshot" => {
                if data["server_id"].as_i64() == Some(self.server)
                    && data["channel_id"].as_i64() == Some(self.channel)
                {
                    if let Some(reads) = data["reads"].as_array() {
                        for value in reads {
                            if let Ok(read) =
                                serde_json::from_value::<voxhold_api::ChannelRead>(value.clone())
                            {
                                let slot = self.reads.entry(read.user_id).or_default();
                                *slot = (*slot).max(read.last_read_message_id);
                            }
                        }
                    }
                }
            }
            "channel.read" => {
                if let Ok(read) = serde_json::from_value::<voxhold_api::ChannelRead>(data.clone()) {
                    if read.server_id == self.server && read.channel_id == self.channel {
                        let slot = self.reads.entry(read.user_id).or_default();
                        *slot = (*slot).max(read.last_read_message_id);
                    }
                }
            }
            "message.created" | "message.updated" => {
                if let Ok(message) = serde_json::from_value::<Message>(data.clone()) {
                    if message.channel_id == self.channel && message.id > 0 {
                        if self.deleted_ids.contains(&message.id) {
                            // A tombstone wins over late events.
                        } else if let Some(existing) =
                            self.messages.iter_mut().find(|m| m.id == message.id)
                        {
                            if message.revision >= existing.revision {
                                *existing = message;
                            }
                        } else if kind == "message.created" {
                            self.messages.push(message);
                        }
                        self.messages.sort_by_key(|m| m.id);
                        if self.messages.len() > 1000 {
                            self.messages.remove(0);
                        }
                    }
                }
            }
            "message.deleted" => {
                if data["channel_id"].as_i64() == Some(self.channel) {
                    if let Some(id) = data["message_id"].as_i64() {
                        self.deleted_ids.insert(id);
                        self.messages.retain(|m| m.id != id);
                    }
                }
            }
            "message.reactions_changed" => {
                if let Ok(update) =
                    serde_json::from_value::<voxhold_api::ReactionsUpdate>(data.clone())
                {
                    if update.channel_id == self.channel {
                        if let Some(message) =
                            self.messages.iter_mut().find(|m| m.id == update.message_id)
                        {
                            if update.revision >= message.reactions_revision {
                                message.reactions = update.reactions;
                                message.reactions_revision = update.revision;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        resync
    }
    /// Rows currently intersecting the viewport, derived from the real list
    /// geometry. Used to aggregate read receipts only for messages the reader
    /// can actually see.
    fn visible_history_range(&self) -> std::ops::Range<usize> {
        let bounds = self.history_list.viewport_bounds();
        let start = self.history_list.logical_scroll_top().item_ix;
        let count = self.history_list.item_count();
        if bounds.size.height <= px(0.) || start >= count {
            return start..start;
        }
        let mut end = start;
        for ix in start..count.min(start + 200) {
            match self.history_list.bounds_for_item(ix) {
                Some(row) if row.origin.y < bounds.bottom() => end = ix + 1,
                Some(_) => break,
                None => break,
            }
        }
        start..end.max((start + 1).min(count))
    }

    fn mark_visible_read(
        &mut self,
        range: std::ops::Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !window.is_window_active()
            || self.read_pending
            || !self
                .channels
                .iter()
                .any(|c| c.id == self.channel && c.has_history())
        {
            return;
        }
        let Some(message_id) = range
            .filter_map(|index| self.messages.get(index).map(|m| m.id))
            .max()
        else {
            return;
        };
        if message_id <= self.read_sent {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let (server, channel, generation) = (self.server, self.channel, self.generation);
        let runtime = self.runtime.clone();
        self.read_pending = true;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.mark_read(server, channel, message_id))
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    s.read_pending = false;
                    if result.is_ok() {
                        s.read_sent = s.read_sent.max(message_id);
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
    fn refresh_history(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let (server, channel, generation) = (self.server, self.channel, self.generation);
        let events_version = self.events_version;
        if !self
            .channels
            .iter()
            .any(|c| c.id == channel && c.has_history())
        {
            return;
        }
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.messages_page(server, channel, 0))
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    if s.events_version != events_version {
                        s.refresh_history(cx);
                        return;
                    }
                    match result {
                        Ok(page) => {
                            if let Some(minimum) = page.messages.iter().map(|m| m.id).min() {
                                let deleted = &s.deleted_ids;
                                s.messages
                                    .retain(|m| m.id < minimum && !deleted.contains(&m.id));
                            } else {
                                s.messages.clear();
                            }
                            let deleted = &s.deleted_ids;
                            s.messages.extend(
                                page.messages
                                    .into_iter()
                                    .filter(|m| !deleted.contains(&m.id)),
                            );
                            s.messages.sort_by_key(|m| m.id);
                            s.messages.dedup_by_key(|m| m.id);
                            if s.messages.len() > 1000 {
                                let excess = s.messages.len() - 1000;
                                s.messages.drain(0..excess);
                            }
                        }
                        Err(e) => s.error = e,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
    fn load_older(&mut self, cx: &mut Context<Self>) {
        if self.loading_older || !self.has_more {
            return;
        }
        let (Some(c), Some(before)) = (self.client.clone(), self.before) else {
            return;
        };
        // Remember the top visible row so prepending does not move the reader.
        let anchor = {
            let offset = self.history_list.logical_scroll_top();
            self.messages
                .get(offset.item_ix)
                .map(|m| (m.id, offset.offset_in_item))
        };
        let (sid, cid, generation) = (self.server, self.channel, self.generation);
        let events_version = self.events_version;
        self.loading_older = true;
        cx.notify();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(c.messages_page(sid, cid, before))
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    s.loading_older = false;
                    if s.events_version != events_version {
                        s.load_older(cx);
                        return;
                    }
                    match result {
                        Ok(page) => {
                            s.before = page.pagination.next_before_id;
                            s.has_more = page.pagination.has_more;
                            let deleted = &s.deleted_ids;
                            s.messages.extend(
                                page.messages
                                    .into_iter()
                                    .filter(|m| !deleted.contains(&m.id)),
                            );
                            s.messages.sort_by_key(|m| m.id);
                            s.messages.dedup_by_key(|m| m.id);
                            if s.messages.len() > 1000 {
                                let excess = s.messages.len() - 1000;
                                s.messages.drain(0..excess);
                                s.has_more = false;
                            }
                            // Applied by `render` after the list splices its
                            // item count, so the scroll target is in range.
                            s.pending_anchor = anchor;
                        }
                        Err(error) => s.error = error,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.client.clone() else { return };
        if self.busy
            || !self
                .channels
                .iter()
                .any(|ch| ch.id == self.channel && ch.writable())
        {
            return;
        }
        let text = self.composer.read(cx).value().to_string();
        let has_files = self
            .attachments
            .as_ref()
            .is_some_and(|a| a.read(cx).has_files());
        if (text.trim().is_empty() && !has_files) || text.chars().count() > 5120 {
            return;
        }
        if has_files {
            if let Some(attachments) = &self.attachments {
                if attachments.read(cx).is_busy() {
                    return;
                }
                self.attachment_pending = Some((self.generation, text.clone()));
                attachments.update(cx, |a, cx| a.send(&text, 0, cx));
            }
            return;
        }
        let sid = self.server;
        let cid = self.channel;
        let generation = self.generation;
        self.busy = true;
        cx.notify();
        let sent_text = text.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(c.send_message(sid, cid, &text))
                .map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.busy = false;
                match result {
                    Ok(message) if s.generation == generation => {
                        if s.composer.read(cx).value().as_ref() == sent_text {
                            s.composer
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        if !s.messages.iter().any(|m| m.id == message.id) {
                            s.messages.push(message);
                        }
                        if s.messages.len() > 1000 {
                            s.messages.remove(0);
                        }
                    }
                    Ok(_) => {}
                    Err(e) if s.generation == generation => s.error = e,
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
}
/// `rgb` tokens are stored as `0xRRGGBB`; GPUI colors need alpha.
fn ink(value: u32) -> gpui::Rgba {
    rgba((value << 8) | 0xff)
}

impl Voxhold {
    fn save_shell(&self) {
        if let Some(path) = &self.shell_path {
            let _ = self.shell.save(path);
        }
    }

    fn apply_theme(&mut self, mode: theme::ThemeMode, cx: &mut Context<Self>) {
        self.theme_mode = mode;
        self.system_light = theme::system_light();
        self.palette = theme::Palette::resolve(mode, self.system_light);
        theme::save_mode(self.theme_path.as_deref(), mode);
        cx.notify();
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = self.theme_mode.toggled(self.system_light);
        self.apply_theme(mode, cx);
        window_chrome::apply(window, self.palette.light);
    }

    fn set_members_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.show_members = open;
        self.shell.members_open = open;
        self.save_shell();
        cx.notify();
    }

    fn auth_error_row(&self, rect: layout::Rect) -> AnyElement {
        let p = self.palette;
        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(8.))
            .bg(ink(p.error_bg))
            .border_1()
            .border_color(ink(p.error_border))
            .flex()
            .items_center()
            .gap_2()
            .px(px(12.))
            .child(
                div()
                    .text_size(px(16.))
                    .text_color(ink(p.error_text))
                    .child("×"),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .text_color(ink(p.error_text))
                    .child(self.auth.status.clone()),
            )
            .into_any_element()
    }

    fn auth_field(
        &mut self,
        entity: &Entity<InputState>,
        rect: layout::Rect,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .rounded(px(9.))
            .bg(ink(p.field_bg))
            .border_1()
            .border_color(ink(p.field_border))
            .capture_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, window, cx| {
                let key = &event.keystroke;
                if key.key == "enter"
                    && !key.modifiers.shift
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && !key.modifiers.platform
                {
                    // Enter belongs to the form: GPUI would otherwise insert a
                    // newline into a single-line field.
                    if s.auth.is_instance() {
                        s.submit_instance(window, cx);
                    } else {
                        s.submit_login(window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(
                TextInput::new(entity)
                    .text_size(px(if compact { 17. } else { 16. }))
                    .font_family("Inter")
                    .bordered(false)
                    .h_full(),
            )
            .into_any_element()
    }

    fn auth_story(&self, l: auth::StoryLayout, vw: f32, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let mut story = div()
            .absolute()
            .left(px(0.))
            .top(px(0.))
            .w(px(if vw > 980.0 { vw * 0.55 } else { vw / 2.0 }))
            .h_full()
            .child(
                div()
                    .absolute()
                    .left(px(l.brand.x))
                    .top(px(l.brand.y))
                    .w(px(39.))
                    .h(px(39.))
                    .rounded(px(11.))
                    .bg(ink(p.lime))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(20.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(0x10100f))
                    .child("V"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.brand.x + 50.0))
                    .top(px(l.brand.y))
                    .h(px(39.))
                    .flex()
                    .items_center()
                    .text_size(px(21.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.fg))
                    .child("voxhold"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.tagline.x - 16.0 + 3.0))
                    .top(px(l.tagline.y + 8.0 - 3.5))
                    .w(px(7.))
                    .h(px(7.))
                    .rounded_full()
                    .bg(ink(p.lime)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.tagline.x))
                    .top(px(l.tagline.y))
                    .h(px(l.tagline.h))
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.fg))
                    .child("Голос. Текст. Свои люди."),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.line1.x))
                    .top(px(l.line1.y))
                    .w(px(l.line1.w))
                    .text_size(px(l.font))
                    .line_height(px(l.font * 0.96))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.fg))
                    .child("Место, где"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.line2.x))
                    .top(px(l.line2.y))
                    .w(px(l.line2.w))
                    .text_size(px(l.font))
                    .line_height(px(l.font * 0.96))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.fg))
                    .child("разговоры"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.line3.x))
                    .top(px(l.line3.y))
                    .w(px(l.line3.w))
                    .text_size(px(l.font))
                    .line_height(px(l.font * 0.96))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.story_accent))
                    .child("живут."),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.paragraph.x))
                    .top(px(l.paragraph.y))
                    .w(px(l.paragraph.w))
                    .text_size(px(l.body))
                    .line_height(px(l.body * 1.65))
                    .text_color(ink(p.muted))
                    .child("Без шума и лишнего. Только ваши комнаты, сообщения и люди, с которыми хочется оставаться на связи."),
            )
            .child(
                div()
                    .absolute()
                    .left(px(l.footer.x))
                    .top(px(l.footer.y))
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("© 2026 Voxhold"),
            );
        if vw > 980.0 {
            story = story.child(self.auth_preview(l.preview, cx));
        }
        story.into_any_element()
    }

    /// Decorative workspace mock-up used by the wide auth story column. It is
    /// drawn from primitives so no bitmap asset is needed and it follows the
    /// active theme like the reference raster does.
    fn auth_preview(&self, r: layout::Rect, _cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let dark = !p.light;
        let panel = if dark { 0x121311 } else { 0xf7f7f2 };
        let rail = if dark { 0x0e0e0d } else { 0xefefe8 };
        let tile = if dark { 0x292925 } else { 0xe3e3da };
        let white = if dark { 0xf1efe8 } else { 0x20211d };
        let grey = if dark { 0xa6a49b } else { 0x62635b };
        let dim = if dark { 0x98978f } else { 0x8a8b81 };
        let violet = if p.light { 0x7c64df } else { 0xa993ff };
        let mut preview = div()
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .h(px(r.h))
            .rounded(px(16.))
            .bg(ink(panel))
            .border_1()
            .border_color(rgba(0xffffff24))
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(0.))
                    .w(px(47.))
                    .h_full()
                    .bg(ink(rail))
                    .border_r_1()
                    .border_color(rgba(0xffffff17)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(192.))
                    .top(px(0.))
                    .w(px(1.))
                    .h_full()
                    .bg(rgba(0xffffff17)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(9.))
                    .top(px(14.))
                    .w(px(30.))
                    .h(px(30.))
                    .rounded(px(9.))
                    .bg(ink(p.lime))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(16.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(0x111111))
                    .child("V"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(208.))
                    .top(px(22.))
                    .w(px(27.))
                    .h(px(27.))
                    .rounded(px(9.))
                    .bg(ink(p.lime)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(208.))
                    .top(px(71.))
                    .w(px(27.))
                    .h(px(27.))
                    .rounded(px(9.))
                    .bg(ink(violet)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(16.))
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(white))
                    .child("Design room"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(50.))
                    .text_size(px(9.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(dim))
                    .child("ТЕКСТОВЫЕ КАНАЛЫ"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(68.))
                    .text_size(px(12.))
                    .text_color(ink(white))
                    .child("# general"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(88.))
                    .text_size(px(12.))
                    .text_color(ink(grey))
                    .child("# inspiration"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(112.))
                    .text_size(px(9.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(dim))
                    .child("ГОЛОСОВЫЕ КАНАЛЫ"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(58.))
                    .top(px(132.))
                    .text_size(px(12.))
                    .text_color(ink(grey))
                    .child("● coffee break"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(245.))
                    .top(px(24.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(white))
                    .child("mira"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(276.))
                    .top(px(25.))
                    .text_size(px(10.))
                    .text_color(ink(dim))
                    .child("12:42"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(245.))
                    .top(px(43.))
                    .text_size(px(12.))
                    .text_color(ink(grey))
                    .child("Собираемся через пять?"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(245.))
                    .top(px(73.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(white))
                    .child("niko"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(276.))
                    .top(px(74.))
                    .text_size(px(10.))
                    .text_color(ink(dim))
                    .child("12:43"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(245.))
                    .top(px(92.))
                    .text_size(px(12.))
                    .text_color(ink(grey))
                    .child("Уже здесь. Заходите в coffee break"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(208.))
                    .top(px(126.))
                    .text_size(px(12.))
                    .text_color(ink(dim))
                    .child("mira печатает"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(290.))
                    .top(px(126.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.lime))
                    .child("..."),
            );
        for index in 0..3 {
            preview = preview.child(
                div()
                    .absolute()
                    .left(px(10.))
                    .top(px(56. + index as f32 * 39.))
                    .w(px(27.))
                    .h(px(27.))
                    .rounded(px(9.))
                    .bg(ink(tile)),
            );
        }
        preview.into_any_element()
    }

    /// Cached auth backdrop keyed by client area and theme, like the Go
    /// `pageBackground` raster. It is regenerated only when either changes.
    fn auth_background(&mut self, w: u32, h: u32) -> std::sync::Arc<gpui::RenderImage> {
        if let Some((cw, ch, light, image)) = &self.auth_background {
            if *cw == w && *ch == h && *light == self.palette.light {
                return image.clone();
            }
        }
        let rgba = background::page_background(w.max(1), h.max(1), self.palette.light);
        let image = std::sync::Arc::new(gpui::RenderImage::new(smallvec::smallvec![
            image::Frame::new(rgba)
        ]));
        self.auth_background = Some((w, h, self.palette.light, image.clone()));
        image
    }

    fn render_auth(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (vw, vh) = self.viewport;
        let p = self.palette;
        let instance = self.auth.is_instance();
        let l = auth::layout_auth(vw, vh, instance, !self.auth.status.is_empty());
        let compact = vw <= 760.0;
        let background = self.auth_background(vw.round() as u32, vh.round() as u32);
        let mut root = div()
            .relative()
            .size_full()
            .font_family("Inter")
            .text_color(ink(p.fg))
            .bg(ink(p.page_base))
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(0.))
                    .w(px(vw))
                    .h(px(vh))
                    .child(gpui::img(background).size_full()),
            );
        if l.story > 0.0 {
            let story = auth::story_layout(vw, vh);
            root = root.child(self.auth_story(story, vw, cx));
        }
        // Card.
        root = root.child(
            div()
                .absolute()
                .left(px(l.card.x))
                .top(px(l.card.y))
                .w(px(l.card.w))
                .h(px(l.card.h))
                .rounded(px(l.radius))
                .bg(ink(p.card))
                .border_1()
                .border_color(ink(p.card_border)),
        );
        // Theme toggle.
        root = root.child(
            div()
                .id("auth-theme")
                .absolute()
                .left(px(l.toggle.x))
                .top(px(l.toggle.y))
                .w(px(l.toggle.w))
                .h(px(l.toggle.h))
                .rounded(px(8.))
                .bg(ink(p.toggle_bg))
                .border_1()
                .border_color(ink(p.toggle_border))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_size(px(16.))
                .text_color(ink(p.toggle_ink))
                .on_click(cx.listener(|s, _, window, cx| s.toggle_theme(window, cx)))
                .child(if p.light {
                    div()
                        .relative()
                        .w(px(14.))
                        .h(px(14.))
                        .child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .top(px(0.))
                                .w(px(14.))
                                .h(px(14.))
                                .rounded_full()
                                .bg(ink(p.toggle_ink)),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(px(5.))
                                .top(px(-3.))
                                .w(px(14.))
                                .h(px(14.))
                                .rounded_full()
                                .bg(ink(p.toggle_bg)),
                        )
                } else {
                    div()
                        .w(px(11.))
                        .h(px(11.))
                        .rounded_full()
                        .border_2()
                        .border_color(ink(p.toggle_ink))
                }),
        );
        // Mobile brand above the card when the story column is hidden.
        if l.story == 0.0 {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.brand.x))
                    .top(px(l.brand.y))
                    .w(px(39.))
                    .h(px(39.))
                    .rounded(px(11.))
                    .bg(ink(p.lime))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(20.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(0x10100f))
                    .child("V"),
            );
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.brand.x + 50.0))
                    .top(px(l.brand.y))
                    .h(px(39.))
                    .flex()
                    .items_center()
                    .text_size(px(21.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("voxhold"),
            );
        }
        let inner = l.inner;
        if instance {
            root = root
                .child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y))
                        .w(px(inner.w))
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(p.story_accent))
                        .child("ПОДКЛЮЧЕНИЕ"),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y + 27.0))
                        .w(px(inner.w))
                        .text_size(px(35.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Адрес инстанса"),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y + 75.0))
                        .w(px(inner.w))
                        .text_size(px(15.))
                        .text_color(ink(p.muted))
                        .child("Укажите адрес сервера Voxhold."),
                );
        } else {
            let font = (vw * 0.03).clamp(32.0, 42.0);
            let mut card = div()
                .absolute()
                .left(px(inner.x))
                .top(px(inner.y + 4.0))
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(ink(p.story_accent))
                .child("ДОБРО ПОЖАЛОВАТЬ");
            card = card.child(
                div()
                    .absolute()
                    .left(px(inner.x))
                    .top(px(inner.y + 34.0))
                    .w(px(inner.w))
                    .text_size(px(font))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("С возвращением"),
            );
            if inner.w < 330.0 {
                card = card
                    .child(
                        div()
                            .absolute()
                            .left(px(inner.x))
                            .top(px(inner.y + 81.0))
                            .w(px(inner.w))
                            .text_size(px(15.))
                            .text_color(ink(p.muted))
                            .child("Войдите в существующий аккаунт"),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(inner.x))
                            .top(px(inner.y + 104.0))
                            .w(px(inner.w))
                            .text_size(px(15.))
                            .text_color(ink(p.muted))
                            .child("Voxhold."),
                    );
            } else {
                card = card.child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y + 81.0))
                        .w(px(inner.w))
                        .text_size(px(15.))
                        .text_color(ink(p.muted))
                        .child("Войдите в существующий аккаунт Voxhold."),
                );
            }
            card = card
                .child(
                    div()
                        .absolute()
                        .left(px(l.username_label.x))
                        .top(px(l.username_label.y))
                        .w(px(l.username_label.w))
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(p.label))
                        .child("Имя пользователя"),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(l.password_label.x))
                        .top(px(l.password_label.y))
                        .w(px(l.password_label.w))
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(p.label))
                        .child("Пароль"),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(l.terms.x))
                        .top(px(l.terms.y))
                        .w(px(l.terms.w))
                        .text_size(px(12.))
                        .line_height(px(18.6))
                        .text_color(ink(p.muted))
                        .child("Продолжая, вы принимаете правила сообщества и политику конфиденциальности."),
                );
            root = root.child(card);
        }
        if instance {
            let field = self.instance.clone();
            root = root.child(self.auth_field(&field, l.user, compact, cx));
        } else {
            let username = self.username.clone();
            let password = self.password.clone();
            root = root.child(self.auth_field(&username, l.user, compact, cx));
            root = root.child(self.auth_field(&password, l.password, compact, cx));
        }
        // Primary action: "Продолжить" on the address step, then sign-in.
        let busy = self.auth.busy;
        let accent = if busy {
            rgba((p.lime << 8) | 0x8c)
        } else {
            ink(p.lime)
        };
        root = root.child(
            div()
                .id("auth-primary")
                .absolute()
                .left(px(l.primary.x))
                .top(px(l.primary.y))
                .w(px(l.primary.w))
                .h(px(l.primary.h))
                .rounded(px(11.))
                .bg(accent)
                .text_size(px(16.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(ink(p.accent_button_ink))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(|s, _, window, cx| {
                    if s.auth.is_instance() {
                        s.submit_instance(window, cx);
                    } else {
                        s.submit_login(window, cx);
                    }
                }))
                .child(if busy {
                    "…".to_string()
                } else if instance {
                    "Продолжить".to_string()
                } else {
                    "Войти в Voxhold".to_string()
                }),
        );
        if !self.auth.status.is_empty() {
            root = root.child(self.auth_error_row(l.error));
        }
        root.into_any_element()
    }

    fn rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let mut rail = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .py(px(12.))
            .bg(ink(p.panel))
            .child(
                div()
                    .w(px(39.))
                    .h(px(39.))
                    .rounded(px(11.))
                    .bg(ink(p.lime))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(20.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(0x10100f))
                    .child("V"),
            )
            .child(
                div()
                    .id("inbox-open")
                    .w(px(48.))
                    .h(px(48.))
                    .rounded(px(14.))
                    .bg(ink(p.surface))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child("ЛС")
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_inbox = !s.show_inbox;
                        if s.show_inbox {
                            if let Some(inbox) = &s.inbox {
                                inbox.update(cx, |inbox, cx| inbox.load(window, cx));
                            }
                        }
                        cx.notify();
                    })),
            );
        for server in &self.servers {
            let id = server.id;
            let active = id == self.server;
            rail = rail.child(
                div()
                    .id(("server", id as u64))
                    .w(px(48.))
                    .h(px(48.))
                    .rounded(px(14.))
                    .cursor_pointer()
                    .bg(if active { ink(p.lime) } else { ink(p.surface) })
                    .text_color(if active { ink(0x10100f) } else { ink(p.fg) })
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(18.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .on_click(cx.listener(move |s, _, w, cx| s.select_server(id, w, cx)))
                    .child(server.name.chars().next().unwrap_or('?').to_string()),
            );
        }
        rail.child(div().flex_1())
            .child(
                div()
                    .id("settings-open")
                    .w(px(48.))
                    .h(px(48.))
                    .rounded(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(10.))
                    .text_color(ink(p.muted))
                    .child("Настройки")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.show_settings = !s.show_settings;
                        if s.show_settings {
                            if let Some(settings) = &s.settings {
                                settings.update(cx, |settings, cx| settings.load(cx));
                            }
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn channels_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let can_manage = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .is_some_and(|s| s.role == "owner" || s.role == "admin");
        let owner = self
            .servers
            .iter()
            .any(|s| s.id == self.server && s.role == "owner");
        let name = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Voxhold".into());
        let mut panel = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ink(p.surface))
            .border_r_1()
            .border_color(ink(p.line))
            .child(
                div()
                    .h(px(67.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(15.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(name)
                    .child(div().text_size(px(12.)).text_color(ink(p.muted)).child("‹")),
            );
        if owner {
            panel = panel.child(
                div()
                    .id("server-settings-open")
                    .px(px(16.))
                    .py(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Настройки сервера")
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_server_settings = true;
                        if let Some(settings) = &s.server_settings {
                            settings.update(cx, |settings, cx| settings.load(window, cx));
                        }
                        cx.notify();
                    })),
            );
        }
        if can_manage && self.channel != 0 {
            panel = panel.child(
                div()
                    .id("channel-settings-open")
                    .px(px(16.))
                    .py(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Настройки канала")
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_channel_settings = true;
                        if let Some(settings) = &s.channel_settings {
                            settings.update(cx, |settings, cx| settings.load(window, cx));
                        }
                        cx.notify();
                    })),
            );
        }
        for (kind, title) in [
            ("news", "НОВОСТНЫЕ КАНАЛЫ"),
            ("text", "ТЕКСТОВЫЕ КАНАЛЫ"),
            ("voice", "ГОЛОСОВЫЕ КАНАЛЫ"),
        ] {
            let items: Vec<Channel> = self
                .channels
                .iter()
                .filter(|c| c.kind == kind)
                .cloned()
                .collect();
            if items.is_empty() {
                continue;
            }
            panel = panel.child(
                div()
                    .px(px(16.))
                    .pt(px(10.))
                    .pb(px(4.))
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink(p.muted))
                    .child(title),
            );
            for channel in items {
                let id = channel.id;
                let active = id == self.channel;
                let locked = channel.kind == "news" && !channel.writable() || !channel.readable();
                panel = panel.child(
                    div()
                        .id(("channel", id as u64))
                        .h(px(35.))
                        .px(px(16.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .bg(if active {
                            ink(p.selection)
                        } else {
                            rgba(0x00000000)
                        })
                        .text_color(if active { ink(p.fg) } else { ink(p.muted) })
                        .child(format!(
                            "{} {}",
                            if channel.kind == "voice" {
                                "●"
                            } else if channel.kind == "news" {
                                "◆"
                            } else {
                                "#"
                            },
                            channel.name
                        ))
                        .when(locked, |row| row.text_color(ink(p.muted)))
                        .on_click(cx.listener(move |s, _, w, cx| s.select_channel(id, w, cx))),
                );
            }
        }
        panel = panel.child(div().flex_1());
        #[cfg(feature = "voxhold-media")]
        if let Some(voice) = &self.voice {
            if self.channel == self.voice_channel && self.voice_channel != 0 {
                panel = panel.child(div().h(px(260.)).child(voice.clone()));
            }
        }
        panel
            .child(
                div()
                    .h(px(58.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(ink(p.line))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .text_color(ink(p.fg))
                            .child(format!("@{}", self.auth.username)),
                    )
                    .child(
                        div()
                            .id("theme-toggle")
                            .w(px(28.))
                            .h(px(28.))
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, window, cx| s.toggle_theme(window, cx)))
                            .child(
                                div()
                                    .w(px(11.))
                                    .h(px(11.))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(ink(p.muted)),
                            ),
                    )
                    .child(
                        div()
                            .id("logout")
                            .w(px(28.))
                            .h(px(28.))
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, w, cx| s.logout(w, cx)))
                            .child(div().text_size(px(9.)).child("Выход")),
                    ),
            )
            .into_any_element()
    }

    fn members_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let mut panel = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ink(p.panel))
            .border_l_1()
            .border_color(ink(p.line))
            .child(
                div()
                    .h(px(67.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(14.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Участники")
                    .child(
                        div()
                            .id("members-close")
                            .cursor_pointer()
                            .text_size(px(13.))
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, _, cx| s.set_members_open(false, cx)))
                            .child("×"),
                    ),
            );
        let online: Vec<(i64, String)> = self
            .members
            .iter()
            .map(|m| (m.user_id, m.username.clone()))
            .collect();
        panel = panel.child(
            div()
                .px(px(16.))
                .pt(px(10.))
                .pb(px(4.))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(ink(p.muted))
                .child(format!("В СЕТИ — {}", online.len())),
        );
        for (id, name) in online {
            panel = panel.child(
                div()
                    .id(("member", id as u64))
                    .h(px(72.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(40.))
                            .h(px(40.))
                            .rounded_full()
                            .bg(ink(p.selection))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(15.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(name.chars().next().unwrap_or('?').to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(14.)).child(name))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(ink(p.muted))
                                    .child("В сети"),
                            ),
                    ),
            );
        }
        panel.child(div().flex_1()).into_any_element()
    }
}

impl Render for Voxhold {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        self.viewport = (viewport.width.into(), viewport.height.into());
        let (vw, vh) = self.viewport;
        let p = self.palette;
        let mut root = div()
            .relative()
            .size_full()
            .font_family("Inter")
            .bg(ink(p.bg))
            .text_color(ink(p.fg))
            .capture_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        s.show_search = false;
                        s.show_tools = false;
                        s.show_settings = false;
                        s.show_channel_settings = false;
                        s.show_server_settings = false;
                        s.show_inbox = false;
                        s.show_channels = false;
                        cx.notify();
                        cx.stop_propagation();
                    }
                    "f5" if s.client.is_some() => {
                        s.refresh_history(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .on_mouse_move(cx.listener(|s, event: &gpui::MouseMoveEvent, _, cx| {
                let Some(drag) = s.drag else { return };
                if event.pressed_button != Some(MouseButton::Left) {
                    s.drag = None;
                    s.save_shell();
                    cx.notify();
                    return;
                }
                let x: f32 = event.position.x.into();
                match drag.side {
                    SplitterSide::Channels => {
                        s.shell.channel_width =
                            layout::proposed_width(drag.side, x, drag.grab_offset);
                    }
                    SplitterSide::Members => {
                        s.shell.members_width =
                            layout::members_width_from_pointer(s.viewport.0, x, drag.grab_offset);
                    }
                }
                s.shell = s.shell.normalized();
                s.drag = Some(SplitterDrag {
                    last_width: match drag.side {
                        SplitterSide::Channels => s.shell.channel_width,
                        SplitterSide::Members => s.shell.members_width,
                    },
                    ..drag
                });
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|s, _, _, cx| {
                    if s.drag.take().is_some() {
                        s.save_shell();
                        cx.notify();
                    }
                }),
            );
        if self.client.is_none() {
            if !self.initial_focus_done {
                self.initial_focus_done = true;
                // Focus after the first frame so the field's focus handle is
                // mounted, matching the Go `RequestFocus(user)` call.
                let weak = cx.entity().downgrade();
                window.on_next_frame(move |window, cx| {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |s, cx| {
                            let entity = s.instance.clone();
                            entity.update(cx, |input, cx| input.focus(window, cx));
                        });
                    }
                });
            }
            return root.child(self.render_auth(cx)).into_any_element();
        }
        if !self.shell_initialized {
            self.shell_initialized = true;
            self.shell = self
                .shell_path
                .as_deref()
                .map(layout::Settings::load)
                .unwrap_or_default();
            // Go hides the members drawer on first creation for narrow windows;
            // the saved value is kept for the next wide start.
            self.show_members = self.shell.members_open && vw > layout::COMPACT_MAX_WIDTH;
        }
        let mobile = vw <= layout::MOBILE_MAX_WIDTH;
        let channels_open = !mobile || self.show_channels;
        let l = layout::compute_layout(vw, vh, self.shell, channels_open, self.show_members);
        if !mobile {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.rail.x))
                    .top(px(l.rail.y))
                    .w(px(l.rail.w))
                    .h(px(l.rail.h))
                    .child(self.rail(cx)),
            );
        }
        // Chat column: fixed header, history, composer, panels.
        let keys: Vec<_> = self
            .messages
            .iter()
            .map(|m| (m.id, m.revision, m.reactions_revision))
            .collect();
        if keys != self.history_keys {
            let prefix = self
                .history_keys
                .iter()
                .zip(&keys)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = self.history_keys[prefix..]
                .iter()
                .rev()
                .zip(keys[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            self.history_list.splice(
                prefix..self.history_keys.len() - suffix,
                keys.len() - prefix - suffix,
            );
            self.history_keys = keys;
        }
        // A prepend keeps `(message_id, offset_in_row)` under the pointer. The
        // list count was just spliced, so the target index is in range.
        if let Some((id, offset_in_item)) = self.pending_anchor.take() {
            if let Some(item_ix) = self.messages.iter().position(|m| m.id == id) {
                self.history_list.scroll_to(gpui::ListOffset {
                    item_ix,
                    offset_in_item,
                });
            }
        }
        let selected = self.channels.iter().find(|c| c.id == self.channel).cloned();
        let readable = selected.as_ref().is_some_and(|c| c.has_history());
        let writable = selected.as_ref().is_some_and(|c| c.writable());
        let role = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .map(|s| s.role.as_str())
            .unwrap_or("");
        let moderate = role == "owner"
            || role == "admin"
            || selected.as_ref().is_some_and(|c| c.permissions & 16 != 0);
        let news = selected.as_ref().is_some_and(|c| c.kind == "news");
        let rows: Vec<_> = self
            .messages
            .iter()
            .map(|m| {
                let own = self.self_user_id > 0 && self.self_user_id == m.author.user_id;
                let permissions = message_tools::MessagePermissions {
                    write: readable && writable && !news,
                    edit: readable && own && m.forward.is_none() && (!news || writable),
                    delete: readable && (own || moderate),
                    react: readable,
                    pin: readable && moderate,
                    forward: readable,
                    comments: readable
                        && m.thread_root_id.is_none()
                        && selected.as_ref().is_some_and(|c| {
                            c.kind == "text" || c.comments_enabled || m.comment_count > 0
                        }),
                };
                let status = if own {
                    if self
                        .reads
                        .iter()
                        .any(|(user, last)| *user != self.self_user_id && *last >= m.id)
                    {
                        "Прочитано"
                    } else {
                        "Отправлено"
                    }
                } else {
                    ""
                };
                (m.clone(), permissions, status)
            })
            .collect();
        let weak = cx.entity().downgrade();
        let history_list = list(self.history_list.clone(), move |index, _, _| {
            let (message, permissions, status) = rows[index].clone();
            let selected_message = message.clone();
            let weak = weak.clone();
            div()
                .id(("message", message.id as u64))
                .px_4()
                .py_2()
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |s, cx| {
                            if let Some(tools) = &s.message_tools {
                                tools.update(cx, |tools, cx| {
                                    tools.select(
                                        s.server,
                                        s.channel,
                                        Some(selected_message.clone()),
                                        permissions,
                                        cx,
                                    )
                                });
                                s.show_tools = true;
                            }
                            if let Some(attachments) = &s.attachments {
                                attachments
                                    .update(cx, |a, cx| a.set_message(Some(&selected_message), cx));
                            }
                            cx.notify();
                        });
                    }
                })
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink(p.lime))
                        .child(message.author.username),
                )
                .child(div().text_size(px(14.)).child(message.content))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(ink(p.muted))
                        .child(status),
                )
                .into_any_element()
        })
        .flex_1();
        let mut history = div().flex_1().flex().flex_col().min_h(px(0.));
        if self.has_more {
            history = history.child(
                div()
                    .id("older")
                    .p_2()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .on_click(cx.listener(|s, _, _, cx| s.load_older(cx)))
                    .child(if self.loading_older {
                        "Загрузка…"
                    } else {
                        "Загрузить предыдущие сообщения"
                    }),
            );
        }
        history = history.child(history_list);
        if !self.messages.is_empty() {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |s, cx| {
                        if s.history_list.viewport_bounds().size.height > px(0.) {
                            let range = s.visible_history_range();
                            s.mark_visible_read(range, window, cx);
                        }
                    });
                }
            });
        }
        let title = selected
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Выберите канал".into());
        let explanation = selected
            .as_ref()
            .map(|c| {
                if c.e2ee_required {
                    "Этот канал требует E2EE; сообщения недоступны."
                } else if c.kind == "voice" && !c.readable() {
                    "Нет доступа к голосовому каналу."
                } else {
                    ""
                }
            })
            .unwrap_or("");
        let mut chat = div().relative().size_full().flex().flex_col().bg(ink(p.bg));
        let header = div()
            .h(px(if mobile { 58. } else { 67. }))
            .px(px(16.))
            .items_center()
            .bg(ink(p.surface))
            .border_b_1()
            .border_color(ink(p.line))
            .flex()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(mobile, |row| {
                        row.child(
                            div()
                                .id("channels-toggle")
                                .cursor_pointer()
                                .text_color(ink(p.muted))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_channels = !s.show_channels;
                                    cx.notify();
                                }))
                                .child(div().text_size(px(11.)).child("Меню")),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(title),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_size(px(13.))
                    .text_color(ink(p.muted))
                    .child(
                        div()
                            .id("toggle-search")
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.show_search = !s.show_search;
                                cx.notify();
                            }))
                            .child("Поиск"),
                    )
                    .child(
                        div()
                            .id("members-toggle")
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                let open = !s.show_members;
                                s.set_members_open(open, cx);
                            }))
                            .child("Участники"),
                    ),
            );
        chat = chat.child(header);
        chat = chat.child(history);
        if !explanation.is_empty() {
            chat = chat.child(
                div()
                    .px(px(16.))
                    .pb(px(6.))
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(explanation),
            );
        }
        if !self.error.is_empty() {
            chat = chat.child(
                div()
                    .px(px(16.))
                    .pb(px(6.))
                    .text_size(px(12.))
                    .text_color(ink(p.danger))
                    .child(self.error.clone()),
            );
        }
        if let Some(attachments) = &self.attachments {
            chat = chat.child(attachments.clone());
        }
        if writable {
            chat = chat.child(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(47.))
                            .capture_key_down(cx.listener(
                                |s, event: &gpui::KeyDownEvent, window, cx| {
                                    let key = &event.keystroke;
                                    if key.key == "enter"
                                        && !key.modifiers.shift
                                        && !key.modifiers.control
                                        && !key.modifiers.alt
                                        && !key.modifiers.platform
                                    {
                                        s.send(window, cx);
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .child(
                                TextInput::new(&self.composer)
                                    .font_family("Inter")
                                    .text_size(px(14.))
                                    .h_full(),
                            ),
                    )
                    .child(
                        div()
                            .id("send")
                            .w(px(98.))
                            .h(px(40.))
                            .rounded(px(11.))
                            .bg(ink(p.lime))
                            .text_color(ink(0x10100f))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, w, cx| s.send(w, cx)))
                            .child("Отправить"),
                    ),
            );
        }
        if self.show_search {
            if let Some(search) = &self.search {
                chat = chat.child(div().h(px(300.)).child(search.clone()));
            }
        }
        if self.show_tools {
            if let Some(tools) = &self.message_tools {
                chat = chat.child(
                    div()
                        .h(px(360.))
                        .child(
                            div()
                                .id("close-message-tools")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_tools = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(tools.clone()),
                );
            }
        }
        #[cfg(feature = "voxhold-media")]
        if self.channel == self.voice_channel && self.voice_channel != 0 {
            if let Some(voice) = &self.voice {
                chat = chat.child(div().h(px(400.)).child(voice.clone()));
            }
        }
        if self.show_inbox {
            if let Some(inbox) = &self.inbox {
                chat = chat.child(
                    div()
                        .h(px(500.))
                        .child(
                            div()
                                .id("inbox-close")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_inbox = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(inbox.clone()),
                );
            }
        }
        if self.show_server_settings {
            if let Some(settings) = &self.server_settings {
                chat = chat.child(
                    div()
                        .h(px(500.))
                        .child(
                            div()
                                .id("server-settings-close")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_server_settings = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(settings.clone()),
                );
            }
        }
        if self.show_channel_settings {
            if let Some(settings) = &self.channel_settings {
                chat = chat.child(
                    div()
                        .h(px(500.))
                        .child(
                            div()
                                .id("channel-settings-close")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_channel_settings = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(settings.clone()),
                );
            }
        }
        if self.show_settings {
            if let Some(settings) = &self.settings {
                chat = chat.child(
                    div()
                        .h(px(500.))
                        .child(
                            div()
                                .id("settings-close")
                                .cursor_pointer()
                                .text_size(px(12.))
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_settings = false;
                                    cx.notify();
                                }))
                                .child("Закрыть"),
                        )
                        .child(settings.clone()),
                );
            }
        }
        root = root.child(
            div()
                .absolute()
                .left(px(l.chat.x))
                .top(px(l.chat.y))
                .w(px(l.chat.w))
                .h(px(l.chat.h))
                .child(chat),
        );
        if mobile {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.bottom.x))
                    .top(px(l.bottom.y))
                    .w(px(l.bottom.w))
                    .h(px(l.bottom.h))
                    .bg(ink(p.surface))
                    .border_t_1()
                    .border_color(ink(p.line))
                    .flex()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child(
                        div()
                            .id("bottom-channels")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.show_channels = !s.show_channels;
                                cx.notify();
                            }))
                            .child("Каналы"),
                    )
                    .child(
                        div()
                            .id("bottom-members")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                let open = !s.show_members;
                                s.set_members_open(open, cx);
                            }))
                            .child("Участники"),
                    )
                    .child(
                        div()
                            .id("bottom-settings")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.show_settings = !s.show_settings;
                                if s.show_settings {
                                    if let Some(settings) = &s.settings {
                                        settings.update(cx, |settings, cx| settings.load(cx));
                                    }
                                }
                                cx.notify();
                            }))
                            .child("Настройки"),
                    )
                    .child(
                        div()
                            .id("bottom-logout")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, w, cx| s.logout(w, cx)))
                            .child("Выйти"),
                    ),
            );
        }
        if l.is_channels_open() {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.channels.x))
                    .top(px(l.channels.y))
                    .w(px(l.channels.w))
                    .h(px(l.channels.h))
                    .shadow_lg()
                    .child(self.channels_panel(cx)),
            );
        }
        if l.is_members_open() {
            root = root.child(
                div()
                    .absolute()
                    .left(px(l.members.x))
                    .top(px(l.members.y))
                    .w(px(l.members.w))
                    .h(px(l.members.h))
                    .child(self.members_panel(cx)),
            );
        }
        // Splitter handles sit 6 px before the panel edge with a 13 px hitbox.
        if l.is_channels_open() && !mobile {
            root = root.child(
                div()
                    .id("channel-splitter")
                    .absolute()
                    .left(px(l.channel_handle.x))
                    .top(px(l.channel_handle.y))
                    .w(px(l.channel_handle.w))
                    .h(px(l.channel_handle.h))
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|s, event: &gpui::MouseDownEvent, _, cx| {
                            let x: f32 = event.position.x.into();
                            s.drag = Some(SplitterDrag {
                                side: SplitterSide::Channels,
                                grab_offset: x - (layout::RAIL_WIDTH + s.shell.channel_width),
                                last_width: s.shell.channel_width,
                            });
                            cx.notify();
                        }),
                    ),
            );
        }
        if l.is_members_open() {
            root = root.child(
                div()
                    .id("members-splitter")
                    .absolute()
                    .left(px(l.member_handle.x))
                    .top(px(l.member_handle.y))
                    .w(px(l.member_handle.w))
                    .h(px(l.member_handle.h))
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|s, event: &gpui::MouseDownEvent, _, cx| {
                            let x: f32 = event.position.x.into();
                            s.drag = Some(SplitterDrag {
                                side: SplitterSide::Members,
                                grab_offset: x - (s.viewport.0 - s.shell.members_width),
                                last_width: s.shell.members_width,
                            });
                            cx.notify();
                        }),
                    ),
            );
        }
        root.into_any_element()
    }
}

fn parse_args() -> (String, Option<std::path::PathBuf>, f32, f32) {
    let mut server = String::new();
    let mut layout_path = layout::Settings::default_path();
    let mut width = 1280.0f32;
    let mut height = 900.0f32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-server" | "--server" => {
                if let Some(value) = args.next() {
                    server = value;
                }
            }
            "-layout" | "--layout" => {
                if let Some(value) = args.next() {
                    layout_path = Some(std::path::PathBuf::from(value));
                }
            }
            "-width" | "--width" => {
                if let Some(value) = args.next().and_then(|v| v.parse::<i32>().ok()) {
                    width = (value as f32).max(320.0);
                }
            }
            "-height" | "--height" => {
                if let Some(value) = args.next().and_then(|v| v.parse::<i32>().ok()) {
                    height = (value as f32).max(360.0);
                }
            }
            _ => {}
        }
    }
    (server, layout_path, width, height)
}

fn main() {
    let (server, layout_path, width, height) = parse_args();
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let fonts: Vec<std::borrow::Cow<'static, [u8]>> = vec![
            std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Regular.ttf")),
            std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Bold.ttf")),
        ];
        let _ = cx.text_system().add_fonts(fonts);
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        let prefill = server.clone();
        let layout_path = layout_path.clone();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Voxhold".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                window_min_size: Some(size(px(320.), px(360.))),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| Voxhold::new(window, cx, &prefill, layout_path.clone()));
                cx.new(|cx| Root::new(view.into(), window, cx))
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    /// Build the real root view in a test window without touching the user's
    /// preferences: the layout file points at a fresh temporary path. The window
    /// root must be `gpui_component::Root`, exactly like the shipped window.
    fn add_auth_window(cx: &mut TestAppContext) -> (gpui::WindowHandle<Root>, Entity<Voxhold>) {
        let shell = std::env::temp_dir().join(format!(
            "voxhold-ui-test-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let slot: std::rc::Rc<std::cell::RefCell<Option<Entity<Voxhold>>>> = Default::default();
        let captured = slot.clone();
        let window = cx.add_window(move |window, cx| {
            let view = cx.new(|cx| Voxhold::new(window, cx, "", Some(shell)));
            *captured.borrow_mut() = Some(view.clone());
            Root::new(view.into(), window, cx)
        });
        let view = slot.borrow().clone().expect("view");
        (window, view)
    }

    fn focus_field(
        cx: &mut VisualTestContext,
        view: &Entity<Voxhold>,
        pick: fn(&Voxhold) -> Entity<InputState>,
    ) {
        let view = view.clone();
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                let field = pick(v);
                field.update(cx, |input, cx| input.focus(window, cx));
            });
        });
    }

    #[gpui::test]
    async fn instance_validation_keeps_the_screen_and_the_typed_value(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        let (window, view) = add_auth_window(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        focus_field(&mut cx, &view, |v| v.instance.clone());
        cx.simulate_input("http://example.com");
        let raw = view.read_with(&*cx, |v, cx| v.instance.read(cx).text().to_string());
        assert_eq!(raw, "http://example.com", "raw input text");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let (screen, status, value) = view.read_with(&*cx, |v, cx| {
            (
                v.auth.screen,
                v.auth.status.clone(),
                v.instance.read(cx).value().to_string(),
            )
        });
        assert_eq!(screen, auth::AuthScreen::Instance);
        assert_eq!(status, "Введите корректный HTTPS-адрес инстанса");
        assert_eq!(value, "http://example.com");
    }

    #[gpui::test]
    async fn loopback_advances_to_login_and_types_into_the_right_fields(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        let (window, view) = add_auth_window(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        focus_field(&mut cx, &view, |v| v.instance.clone());
        cx.simulate_input("http://127.0.0.1:1");
        cx.simulate_keystrokes("enter");

        let (screen, origin) = view.read_with(&*cx, |v, _| (v.auth.screen, v.auth.origin.clone()));
        assert_eq!(screen, auth::AuthScreen::Login);
        assert_eq!(origin, "http://127.0.0.1:1");

        // Enter must hand focus to the user name; otherwise these characters
        // would land in the address field behind the new screen.
        cx.simulate_input("neva");
        let (username, address) = view.read_with(&*cx, |v, cx| {
            (
                v.username.read(cx).value().to_string(),
                v.instance.read(cx).value().to_string(),
            )
        });
        assert_eq!(username, "neva");
        assert_eq!(address, "http://127.0.0.1:1");

        focus_field(&mut cx, &view, |v| v.password.clone());
        cx.simulate_input("password123");
        let password = view.read_with(&*cx, |v, cx| v.password.read(cx).unmask_value().to_string());
        assert_eq!(password, "password123");

        cx.simulate_keystrokes("enter");
        let (first, password_after) = view.read_with(&*cx, |v, cx| {
            (
                (v.auth.busy, v.auth.login_attempt(), v.auth.status.clone()),
                v.password.read(cx).unmask_value().to_string(),
            )
        });
        assert_eq!(first.1, 1, "Enter must start exactly one attempt");
        assert_eq!(
            password_after, "password123",
            "Enter must not reach the field"
        );
        // Nothing listens on 127.0.0.1:1, so the attempt either is still pending
        // or already finished with the Go network message.
        assert!(
            first.0 || first.2 == "Не удалось связаться с сервером",
            "unexpected state after Enter: {first:?}"
        );

        // After the failure the form stays on the same screen and a repeated
        // Enter starts a fresh attempt with the same confirmed origin.
        let (failed_attempt, screen) =
            view.read_with(&*cx, |v, _| (v.auth.login_attempt(), v.auth.screen));
        assert_eq!(screen, auth::AuthScreen::Login);
        if !first.0 {
            cx.simulate_keystrokes("enter");
            let retried = view.read_with(&*cx, |v, _| v.auth.login_attempt());
            assert_eq!(retried, failed_attempt + 1);
        }
    }

    fn message_json(id: i64, revision: i64, content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "channel_id": 7,
            "conversation_id": 0,
            "content": content,
            "author": {"user_id": 1, "username": "neva"},
            "created_at": 1,
            "revision": revision,
            "comment_count": 0,
            "client_message_id": "",
            "reactions": [],
            "reactions_revision": 0,
            "attachments": [],
            "poll": null
        })
    }

    /// Regression for the R02 merge policy: a stale revision cannot overwrite a
    /// confirmed row, and a deleted id never comes back from a late event.
    #[gpui::test]
    async fn realtime_merge_keeps_revisions_and_tombstones(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        let (_window, view) = add_auth_window(cx);
        cx.update(|app| {
            view.update(app, |v, _| {
                v.channel = 7;
                v.apply_realtime_event("message.created", &message_json(2, 5, "confirmed"));
                v.apply_realtime_event("message.created", &message_json(2, 4, "stale"));
                assert_eq!(v.messages.len(), 1);
                assert_eq!(v.messages[0].content, "confirmed");

                v.apply_realtime_event("message.updated", &message_json(2, 6, "edited"));
                assert_eq!(v.messages[0].content, "edited");

                v.apply_realtime_event(
                    "message.deleted",
                    &serde_json::json!({"channel_id": 7, "message_id": 2}),
                );
                assert!(v.messages.is_empty());
                assert!(v.deleted_ids.contains(&2));

                v.apply_realtime_event("message.created", &message_json(2, 7, "resurrected"));
                assert!(v.messages.is_empty(), "a tombstone must win");

                v.apply_realtime_event("message.created", &message_json(3, 1, "next"));
                assert_eq!(v.messages.len(), 1);
                assert_eq!(v.messages[0].id, 3);
            });
        });
    }
}
