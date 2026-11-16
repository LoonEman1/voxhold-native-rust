//! Channel administration forms (text/news/voice) and the per-channel
//! notification dialog. The async operations live in `src/app/admin_ops.rs`.

use std::collections::HashSet;

use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::InputState;
use voxhold_api::{ChannelAccess, NewsSettings};

use crate::settings::{Dialog, Scope, Tab};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Retry(usize),
    SaveName,
    SaveSlow,
    ToggleRestricted,
    ToggleMemberRole,
    ToggleUser(i64),
    SaveAccess,
    ToggleComments,
    TogglePublisher(i64),
    SaveNews,
    ToggleEncryption,
    Delete,
    ConfirmDelete,
    CancelDelete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifAction {
    Retry,
    Choose(usize),
    Save,
    ToggleNotifications,
}

pub fn tabs_for(kind: &str) -> Vec<Tab> {
    let mut tabs = vec![Tab::new("Основное", "settings"), Tab::new("Доступ", "lock")];
    if kind == "news" {
        tabs.push(Tab::new("Новости", "news"));
    }
    tabs
}

pub struct ChannelSettings {
    pub dialog: Dialog,
    pub server: i64,
    pub channel: i64,
    pub kind: String,
    pub name: String,
    pub allowed: bool,
    pub name_input: Entity<InputState>,
    pub slow_input: Entity<InputState>,
    pub access: Option<ChannelAccess>,
    pub news: Option<NewsSettings>,
    pub restricted: bool,
    pub member_role: bool,
    pub members: Vec<voxhold_api::Member>,
    pub user_ids: HashSet<i64>,
    pub publishers: HashSet<i64>,
    pub comments: bool,
    pub encryption: bool,
    pub confirm_delete: bool,
    pub section_error: String,
}

impl ChannelSettings {
    pub fn new(window: &mut Window, cx: &mut Context<crate::app::Voxhold>) -> Self {
        Self {
            dialog: Dialog::new("Изменить канал", Scope::default(), tabs_for("text")),
            server: 0,
            channel: 0,
            kind: String::new(),
            name: String::new(),
            allowed: false,
            name_input: cx.new(|cx| InputState::new(window, cx).placeholder("Название")),
            slow_input: cx.new(|cx| InputState::new(window, cx).placeholder("Секунды")),
            access: None,
            news: None,
            restricted: false,
            member_role: false,
            members: Vec::new(),
            user_ids: HashSet::new(),
            publishers: HashSet::new(),
            comments: false,
            encryption: false,
            confirm_delete: false,
            section_error: String::new(),
        }
    }

    pub fn reopen(
        &mut self,
        server: i64,
        channel: i64,
        kind: &str,
        name: &str,
        allowed: bool,
        window: &mut Window,
        cx: &mut Context<crate::app::Voxhold>,
    ) {
        self.dialog = Dialog::new(
            "Изменить канал",
            Scope::channel(server, channel),
            tabs_for(kind),
        );
        self.server = server;
        self.channel = channel;
        self.kind = kind.to_owned();
        self.name = name.to_owned();
        self.allowed = allowed;
        self.access = None;
        self.news = None;
        self.restricted = false;
        self.member_role = false;
        self.user_ids.clear();
        self.publishers.clear();
        self.comments = false;
        self.encryption = false;
        self.confirm_delete = false;
        self.section_error.clear();
        self.name_input
            .update(cx, |input, cx| input.set_value(name.to_owned(), window, cx));
        self.slow_input
            .update(cx, |input, cx| input.set_value(String::new(), window, cx));
    }

    pub fn set_members(&mut self, members: &[voxhold_api::Member]) {
        self.members = members
            .iter()
            .filter(|member| member.role == "member")
            .cloned()
            .collect();
    }

    /// Go normalizes a channel name to lower-case words joined by `-`.
    pub fn normalized_name(&self, cx: &gpui::App) -> String {
        self.name_input
            .read(cx)
            .value()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .to_lowercase()
    }

    pub fn apply_access(&mut self) {
        if let Some(access) = &self.access {
            self.restricted = access.restricted;
            self.member_role = access.roles.iter().any(|role| role == "member");
            self.user_ids = access.user_ids.iter().copied().collect();
        }
    }

    pub fn apply_news(&mut self) {
        if let Some(news) = &self.news {
            self.comments = news.comments_enabled;
            self.publishers = news.publisher_ids.iter().copied().collect();
        }
    }

    pub fn access_value(&self) -> ChannelAccess {
        let mut roles = Vec::new();
        if self.member_role {
            roles.push("member".to_owned());
        }
        let mut user_ids: Vec<i64> = self.user_ids.iter().copied().collect();
        user_ids.sort_unstable();
        ChannelAccess {
            restricted: self.restricted,
            user_ids,
            roles,
        }
    }

    pub fn news_value(&self) -> NewsSettings {
        let mut publisher_ids: Vec<i64> = self.publishers.iter().copied().collect();
        publisher_ids.sort_unstable();
        NewsSettings {
            publisher_ids,
            comments_enabled: self.comments,
        }
    }
}

pub struct ChannelNotifications {
    pub dialog: Dialog,
    pub server: i64,
    pub channel: i64,
    pub name: String,
    pub mode: String,
    pub saved: String,
    pub loaded: bool,
    pub error: String,
}

impl ChannelNotifications {
    pub fn new() -> Self {
        Self {
            dialog: Dialog::new("Уведомления", Scope::default(), vec![Tab::new("", "bell")]),
            server: 0,
            channel: 0,
            name: String::new(),
            mode: String::new(),
            saved: String::new(),
            loaded: false,
            error: String::new(),
        }
    }

    pub fn reopen(&mut self, server: i64, channel: i64, name: &str) {
        self.dialog = Dialog::new(
            "Уведомления",
            Scope::channel(server, channel),
            vec![Tab::new("", "bell")],
        );
        self.server = server;
        self.channel = channel;
        self.name = name.to_owned();
        self.mode.clear();
        self.saved.clear();
        self.loaded = false;
        self.error.clear();
    }
}
