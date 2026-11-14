mod attachments;
mod channel_settings;
mod inbox;
mod message_tools;
mod personal_settings;
mod search;
mod server_settings;
#[cfg(feature = "voxhold-media")]
mod voice_panel;
use gpui::{
    div, list, prelude::*, px, rgb, size, App, Application, Bounds, Context, Entity, ListAlignment,
    ListState, Window, WindowBounds, WindowOptions,
};
use gpui_component::StyledExt;
use gpui_component::{
    input::{InputState, TextInput},
    Root,
};
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
}
impl Voxhold {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let history_list = ListState::new(0, ListAlignment::Bottom, px(400.));
        let weak = cx.entity().downgrade();
        history_list.set_scroll_handler(move |event, window, cx| {
            if let Some(view) = weak.upgrade() {
                view.update(cx, |s, cx| {
                    s.mark_visible_read(event.visible_range.clone(), window, cx)
                });
            }
        });
        Self {
            runtime: std::sync::Arc::new(tokio::runtime::Runtime::new().expect("Tokio runtime")),
            instance: cx
                .new(|cx| InputState::new(window, cx).placeholder("https://voxhold.example.org")),
            username: cx.new(|cx| InputState::new(window, cx).placeholder("Имя пользователя")),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Пароль")
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
        }
    }
    fn login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let origin = self.instance.read(cx).value().to_string();
        let user = self.username.read(cx).value().to_string();
        let pass = self.password.read(cx).value().to_string();
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.busy = true;
        self.error.clear();
        cx.notify();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                let mut c = Client::new(&origin).map_err(|e| e.to_string())?;
                c.login(&user, &pass).await.map_err(|e| e.to_string())?;
                let servers = c.servers().await.map_err(|e| e.to_string())?;
                let self_user_id = c.profile().await.map(|p| p.user_id).unwrap_or(0);
                Ok::<_, String>((c, servers, self_user_id))
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.busy = false;
                match result {
                    Ok((c, servers, self_user_id)) => {
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
                                            s.composer.update(cx, |input, cx| {
                                                input.set_value("", window, cx)
                                            });
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
                            personal_settings::PersonalSettings::new(
                                settings_client,
                                runtime,
                                window,
                                cx,
                            )
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
                            channel_settings::ChannelSettings::new(
                                admin_client,
                                runtime,
                                window,
                                cx,
                            )
                        }));
                        let runtime = s.runtime.clone();
                        let inbox_client = c.clone();
                        s.inbox =
                            Some(cx.new(|cx| inbox::Inbox::new(inbox_client, runtime, window, cx)));
                        s.self_user_id = self_user_id;
                        s.client = Some(c);
                        s.servers = servers;
                    }
                    Err(e) => s.error = e,
                }
                cx.notify();
            });
        })
        .detach();
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
                        match event.kind.as_str() {
                            "channel.subscribed" | "channel.sync_required" => resync = true,
                            "channel.read_snapshot" => {
                                if event.data["server_id"].as_i64() == Some(s.server)
                                    && event.data["channel_id"].as_i64() == Some(s.channel)
                                {
                                    if let Some(reads) = event.data["reads"].as_array() {
                                        for value in reads {
                                            if let Ok(read) =
                                                serde_json::from_value::<voxhold_api::ChannelRead>(
                                                    value.clone(),
                                                )
                                            {
                                                let slot = s.reads.entry(read.user_id).or_default();
                                                *slot = (*slot).max(read.last_read_message_id);
                                            }
                                        }
                                    }
                                }
                            }
                            "channel.read" => {
                                if let Ok(read) =
                                    serde_json::from_value::<voxhold_api::ChannelRead>(event.data)
                                {
                                    if read.server_id == s.server && read.channel_id == s.channel {
                                        let slot = s.reads.entry(read.user_id).or_default();
                                        *slot = (*slot).max(read.last_read_message_id);
                                    }
                                }
                            }
                            "message.created" | "message.updated" => {
                                if let Ok(message) = serde_json::from_value::<Message>(event.data) {
                                    if message.channel_id == s.channel && message.id > 0 {
                                        if let Some(existing) =
                                            s.messages.iter_mut().find(|m| m.id == message.id)
                                        {
                                            *existing = message;
                                        } else if event.kind == "message.created" {
                                            s.messages.push(message);
                                        }
                                        s.messages.sort_by_key(|m| m.id);
                                        if s.messages.len() > 1000 {
                                            s.messages.remove(0);
                                        }
                                    }
                                }
                            }
                            "message.deleted" => {
                                if event.data["channel_id"].as_i64() == Some(s.channel) {
                                    if let Some(id) = event.data["message_id"].as_i64() {
                                        s.messages.retain(|m| m.id != id);
                                    }
                                }
                            }
                            "message.reactions_changed" => {
                                if let Ok(update) = serde_json::from_value::<
                                    voxhold_api::ReactionsUpdate,
                                >(event.data)
                                {
                                    if update.channel_id == s.channel {
                                        if let Some(message) = s
                                            .messages
                                            .iter_mut()
                                            .find(|m| m.id == update.message_id)
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
                                s.messages.retain(|m| m.id < minimum);
                            } else {
                                s.messages.clear();
                            }
                            s.messages.extend(page.messages);
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
                            s.messages.extend(page.messages);
                            s.messages.sort_by_key(|m| m.id);
                            s.messages.dedup_by_key(|m| m.id);
                            if s.messages.len() > 1000 {
                                let excess = s.messages.len() - 1000;
                                s.messages.drain(0..excess);
                                s.has_more = false;
                            }
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
impl Render for Voxhold {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = window.viewport_size().width < px(760.);
        let narrow = window.viewport_size().width < px(1180.);
        let mut root = div()
            .size_full()
            .bg(rgb(0x10100f))
            .text_color(rgb(0xf2f0e9))
            .capture_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        s.show_search = false;
                        s.show_tools = false;
                        s.show_settings = false;
                        s.show_channel_settings = false;
                        s.show_server_settings = false;
                        s.show_inbox = false;
                        s.show_members = false;
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
            }));
        if self.client.is_none() {
            return root
                .flex()
                .when(!compact, |root| root.child(div().flex_1().h_full().p_12().flex().flex_col().bg(rgb(0x181916))
                    .child(div().text_2xl().font_bold().child("V  VOXHOLD"))
                    .child(div().mt(px(100.)).text_color(rgb(0xe7ff5b)).child("●  Голос. Текст. Свои люди."))
                    .child(div().mt_6().text_size(px(54.)).child("Место, где разговоры"))
                    .child(div().text_size(px(54.)).text_color(rgb(0xe7ff5b)).child("живут."))
                    .child(div().mt_6().text_color(rgb(0x9c9b93)).child("Без шума и лишнего. Только ваши комнаты, сообщения и люди, с которыми хочется оставаться на связи."))
                    .child(div().flex_1())
                    .child(div().text_color(rgb(0x63635d)).child("© 2026 Voxhold · Приватность по умолчанию"))))
                .child(div().flex_1().h_full().flex().items_center().justify_center().child(
                    div()
                        .w(px(420.))
                        .p_8()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .rounded_lg()
                        .bg(rgb(0x10100f))
                        .child(div().text_color(rgb(0xe7ff5b)).child("ДОБРО ПОЖАЛОВАТЬ"))
                        .child(div().text_2xl().child("С возвращением"))
                        .child(div().text_color(rgb(0x9c9b93)).child("Войдите в существующий аккаунт Voxhold."))
                        .child("Адрес сервера")
                        .child(TextInput::new(&self.instance))
                        .child("Имя пользователя")
                        .child(TextInput::new(&self.username))
                        .child("Пароль")
                        .child(TextInput::new(&self.password))
                        .child(
                            div()
                                .id("login")
                                .p_3()
                                .rounded_md()
                                .bg(rgb(0xe7ff5b))
                                .text_color(rgb(0x10100f))
                                .cursor_pointer()
                                .on_click(cx.listener(|s, _, w, cx| s.login(w, cx)))
                                .child(if self.busy {
                                    "Вход…"
                                } else {
                                    "Войти в Voxhold"
                                }),
                        )
                        .child(self.error.clone()),
                ))
                .into_any_element();
        }
        let mut servers = div()
            .w(px(72.))
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x151513))
            .child("V")
            .child(
                div()
                    .id("inbox-open")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_inbox = !s.show_inbox;
                        if s.show_inbox {
                            if let Some(inbox) = &s.inbox {
                                inbox.update(cx, |inbox, cx| inbox.load(window, cx));
                            }
                        }
                        cx.notify();
                    }))
                    .child("ЛС"),
            )
            .child(
                div()
                    .id("settings-open")
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
                    .child("⚙"),
            );
        for server in &self.servers {
            let id = server.id;
            servers = servers.child(
                div()
                    .id(("server", id as u64))
                    .p_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, w, cx| s.select_server(id, w, cx)))
                    .rounded_md()
                    .bg(if id == self.server {
                        rgb(0xe7ff5b)
                    } else {
                        rgb(0x292925)
                    })
                    .text_color(if id == self.server {
                        rgb(0x10100f)
                    } else {
                        rgb(0xf2f0e9)
                    })
                    .child(server.name.chars().next().unwrap_or('?').to_string()),
            );
        }
        let can_manage = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .is_some_and(|s| s.role == "owner" || s.role == "admin");
        let mut channels = div()
            .w(px(252.))
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x191917))
            .child("Каналы");
        if self
            .servers
            .iter()
            .any(|s| s.id == self.server && s.role == "owner")
        {
            channels = channels.child(
                div()
                    .id("server-settings-open")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_server_settings = true;
                        if let Some(settings) = &s.server_settings {
                            settings.update(cx, |settings, cx| settings.load(window, cx));
                        }
                        cx.notify();
                    }))
                    .child("Настройки сервера"),
            );
        }
        if can_manage && self.channel != 0 {
            channels = channels.child(
                div()
                    .id("channel-settings-open")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, window, cx| {
                        s.show_channel_settings = true;
                        if let Some(settings) = &s.channel_settings {
                            settings.update(cx, |settings, cx| settings.load(window, cx));
                        }
                        cx.notify();
                    }))
                    .child("Настройки канала"),
            );
        }
        for channel in &self.channels {
            let id = channel.id;
            channels = channels.child(
                div()
                    .id(("channel", id as u64))
                    .p_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, w, cx| s.select_channel(id, w, cx)))
                    .rounded_md()
                    .bg(if id == self.channel {
                        rgb(0x2a2a26)
                    } else {
                        rgb(0x191917)
                    })
                    .child(format!(
                        "{} {}",
                        if channel.kind == "voice" { "🔊" } else { "#" },
                        channel.name
                    )),
            );
        }
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
        let selected = self.channels.iter().find(|c| c.id == self.channel);
        let readable = selected.is_some_and(|c| c.has_history());
        let writable = selected.is_some_and(|c| c.writable());
        let role = self
            .servers
            .iter()
            .find(|s| s.id == self.server)
            .map(|s| s.role.as_str())
            .unwrap_or("");
        let moderate =
            role == "owner" || role == "admin" || selected.is_some_and(|c| c.permissions & 16 != 0);
        let news = selected.is_some_and(|c| c.kind == "news");
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
                        && selected.is_some_and(|c| {
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
                        .text_color(rgb(0xe7ff5b))
                        .child(message.author.username),
                )
                .child(message.content)
                .child(div().text_color(rgb(0x777770)).child(status))
                .into_any_element()
        })
        .flex_1();
        let mut history = div().flex_1().flex().flex_col();
        if self.has_more {
            history = history.child(
                div()
                    .id("older")
                    .p_2()
                    .cursor_pointer()
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
                            let index = s.history_list.logical_scroll_top().item_ix;
                            s.mark_visible_read(index..index.saturating_add(1), window, cx);
                        }
                    });
                }
            });
        }
        let selected = self.channels.iter().find(|c| c.id == self.channel);
        let title = selected
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Выберите канал".into());
        let explanation = selected
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
        let mut chat = div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(67.))
                    .px_4()
                    .items_center()
                    .bg(rgb(0x191917))
                    .flex()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("channels-toggle")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        s.show_channels = !s.show_channels;
                                        cx.notify();
                                    }))
                                    .child("☰"),
                            )
                            .child(title),
                    )
                    .child(
                        div()
                            .id("toggle-search")
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.show_search = !s.show_search;
                                cx.notify();
                            }))
                            .child("Поиск и закрепления"),
                    )
                    .child(
                        div()
                            .id("members-toggle")
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.show_members = !s.show_members;
                                cx.notify();
                            }))
                            .child("Участники"),
                    ),
            )
            .child(history)
            .child(div().px_4().child(explanation))
            .child(
                div()
                    .px_4()
                    .text_color(rgb(0xff7777))
                    .child(self.error.clone()),
            );
        if let Some(attachments) = &self.attachments {
            chat = chat.child(attachments.clone());
        }
        if selected.is_some_and(|c| c.writable()) {
            chat = chat.child(
                div()
                    .p_4()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
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
                            .child(TextInput::new(&self.composer)),
                    )
                    .child(
                        div()
                            .id("send")
                            .p_3()
                            .bg(rgb(0xe7ff5b))
                            .text_color(rgb(0x10100f))
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, w, cx| s.send(w, cx)))
                            .child("Отправить"),
                    ),
            );
        }
        let mut members = div()
            .w(px(238.))
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x151513))
            .child("Участники");
        for m in &self.members {
            members = members.child(div().child(m.username.clone()));
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
        if self.channel == self.voice_channel {
            if let Some(voice) = &self.voice {
                chat = chat.child(voice.clone());
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
        root = root.flex().child(servers);
        if !compact || self.show_channels {
            root = root.child(channels);
        }
        root = root.child(chat);
        if !narrow || self.show_members {
            root = root.child(members);
        }
        root.into_any_element()
    }
}
fn main() {
    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| Voxhold::new(window, cx));
                cx.new(|cx| Root::new(view.into(), window, cx))
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
