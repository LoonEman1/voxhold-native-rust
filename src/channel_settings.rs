use gpui::{div, prelude::*, px, rgb, Context, Entity, Window};
use gpui_component::input::{InputState, TextInput};
use std::sync::Arc;
use voxhold_api::{ChannelAccess, Client, NewsSettings};

/// Administrative channel settings. Parent must derive `allowed` from the
/// current server role and clear the scope on navigation.
pub struct ChannelSettings {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
    server: i64,
    channel: i64,
    kind: String,
    allowed: bool,
    name: Entity<InputState>,
    slow: Entity<InputState>,
    users: Entity<InputState>,
    roles: Entity<InputState>,
    publishers: Entity<InputState>,
    restricted: bool,
    comments: bool,
    encryption: bool,
    loaded: bool,
    busy: bool,
    confirm_delete: bool,
    generation: u64,
    error: String,
    notice: String,
}
impl ChannelSettings {
    pub fn new(
        client: Client,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            client,
            runtime,
            server: 0,
            channel: 0,
            kind: String::new(),
            allowed: false,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Название канала")),
            slow: cx.new(|cx| InputState::new(window, cx).placeholder("Медленный режим, секунд")),
            users: cx.new(|cx| {
                InputState::new(window, cx).placeholder("ID пользователей через запятую")
            }),
            roles: cx.new(|cx| InputState::new(window, cx).placeholder("Роли через запятую")),
            publishers: cx.new(|cx| {
                InputState::new(window, cx).placeholder("ID авторов новостей через запятую")
            }),
            restricted: false,
            comments: false,
            encryption: false,
            loaded: false,
            busy: false,
            confirm_delete: false,
            generation: 0,
            error: String::new(),
            notice: String::new(),
        }
    }
    pub fn set_scope(
        &mut self,
        server: i64,
        channel: i64,
        kind: &str,
        allowed: bool,
        cx: &mut Context<Self>,
    ) {
        self.server = server;
        self.channel = channel;
        self.kind = kind.to_owned();
        self.allowed = allowed;
        self.loaded = false;
        self.generation += 1;
        self.busy = false;
        self.confirm_delete = false;
        self.error.clear();
        self.notice.clear();
        cx.notify();
    }
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.allowed || self.server <= 0 || self.channel <= 0 || self.busy {
            return;
        }
        self.busy = true;
        self.error.clear();
        let generation = self.generation;
        let server = self.server;
        let channel = self.channel;
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let kind = self.kind.clone();
        cx.notify();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(async move {
                    let access = client.channel_access(server, channel).await?;
                    let news = if kind == "news" {
                        client.news_settings(server, channel).await?
                    } else {
                        NewsSettings::default()
                    };
                    let slow = if kind == "text" {
                        client.slowmode(server, channel).await?
                    } else {
                        0
                    };
                    let encryption = if kind == "voice" {
                        client.voice_encryption(server, channel).await?
                    } else {
                        false
                    };
                    Ok::<_, voxhold_api::Error>((access, news, slow, encryption))
                })
                .map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.generation != generation {
                    return;
                }
                s.busy = false;
                match result {
                    Ok((access, news, slow, encryption)) => {
                        s.restricted = access.restricted;
                        s.comments = news.comments_enabled;
                        s.encryption = encryption;
                        s.users.update(cx, |v, cx| {
                            v.set_value(
                                access
                                    .user_ids
                                    .iter()
                                    .map(i64::to_string)
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                window,
                                cx,
                            )
                        });
                        s.roles
                            .update(cx, |v, cx| v.set_value(access.roles.join(", "), window, cx));
                        s.publishers.update(cx, |v, cx| {
                            v.set_value(
                                news.publisher_ids
                                    .iter()
                                    .map(i64::to_string)
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                window,
                                cx,
                            )
                        });
                        s.slow
                            .update(cx, |v, cx| v.set_value(slow.to_string(), window, cx));
                        s.loaded = true;
                    }
                    Err(e) => s.error = e,
                };
                cx.notify();
            });
        })
        .detach();
    }
    fn action(&mut self, op: Op, cx: &mut Context<Self>) {
        if !self.allowed || self.server <= 0 || self.channel <= 0 || self.busy || !self.loaded {
            return;
        }
        if (matches!(op, Op::Slow) && self.kind != "text")
            || (matches!(op, Op::News) && self.kind != "news")
            || (matches!(op, Op::Encryption) && self.kind != "voice")
        {
            return;
        }
        if matches!(op, Op::Delete) && !self.confirm_delete {
            self.confirm_delete = true;
            cx.notify();
            return;
        }
        let name = self
            .name
            .read(cx)
            .value()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .to_lowercase();
        if matches!(op, Op::Name) && name.trim().is_empty() {
            self.error = "Укажите название канала".into();
            cx.notify();
            return;
        }
        let slow = self.slow.read(cx).value().to_string();
        let users = self.users.read(cx).value().to_string();
        let roles = self.roles.read(cx).value().to_string();
        let publishers = self.publishers.read(cx).value().to_string();
        let parse_ids = |s: &str| -> Option<Vec<i64>> {
            if s.trim().is_empty() {
                return Some(vec![]);
            }
            s.split(',')
                .map(|x| x.trim().parse::<i64>().ok().filter(|n| *n > 0))
                .collect()
        };
        let ids = match parse_ids(&users) {
            Some(v) => v,
            None => {
                self.error = "Некорректные ID пользователей".into();
                cx.notify();
                return;
            }
        };
        let pubs = match parse_ids(&publishers) {
            Some(v) => v,
            None => {
                self.error = "Некорректные ID авторов".into();
                cx.notify();
                return;
            }
        };
        let seconds = slow.trim().parse::<i64>().unwrap_or(-1);
        if matches!(op, Op::Slow) && !(0..=21600).contains(&seconds) {
            self.error = "Укажите число секунд от 0 до 21600".into();
            cx.notify();
            return;
        }
        let access = ChannelAccess {
            restricted: self.restricted,
            user_ids: ids,
            roles: roles
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .collect(),
        };
        let news = NewsSettings {
            publisher_ids: pubs,
            comments_enabled: self.comments,
        };
        let encryption = self.encryption;
        self.busy = true;
        self.confirm_delete = false;
        self.error.clear();
        self.notice.clear();
        let generation = self.generation;
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let server = self.server;
        let channel = self.channel;
        cx.notify();
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                match op {
                    Op::Name => client.edit_channel(server, channel, &name).await,
                    Op::Slow => client.set_slowmode(server, channel, seconds).await,
                    Op::Access => client.set_channel_access(server, channel, &access).await,
                    Op::News => client.set_news_settings(server, channel, &news).await,
                    Op::Encryption => {
                        client
                            .set_voice_encryption(server, channel, encryption)
                            .await
                    }
                    Op::Delete => client.delete_channel(server, channel).await,
                }
                .map_err(|e| e.to_string())
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    s.busy = false;
                    match result {
                        Ok(()) => s.notice = "Сохранено".into(),
                        Err(e) => s.error = e,
                    };
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}
#[derive(Clone, Copy)]
enum Op {
    Name,
    Slow,
    Access,
    News,
    Encryption,
    Delete,
}
impl Render for ChannelSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .id("channel-settings")
            .w(px(400.))
            .h_full()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x202323))
            .text_color(rgb(0xf3f4eb))
            .child("Настройки канала");
        if !self.allowed {
            return root.child("Недостаточно прав").into_any_element();
        }
        if !self.loaded {
            return root
                .child(
                    div()
                        .id("channel-settings-load")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, window, cx| s.load(window, cx)))
                        .child("Загрузить настройки"),
                )
                .child(self.error.clone())
                .into_any_element();
        }
        root = root.child(TextInput::new(&self.name)).child(
            div()
                .id("channel-name")
                .cursor_pointer()
                .on_click(cx.listener(|s, _, _, cx| s.action(Op::Name, cx)))
                .child("Переименовать"),
        );
        if self.kind == "text" {
            root = root.child(TextInput::new(&self.slow)).child(
                div()
                    .id("channel-slow")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.action(Op::Slow, cx)))
                    .child("Сохранить медленный режим"),
            );
        }
        root = root
            .child(
                div()
                    .id("restricted")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.restricted = !s.restricted;
                        cx.notify();
                    }))
                    .child(format!(
                        "Ограниченный доступ: {}",
                        if self.restricted { "да" } else { "нет" }
                    )),
            )
            .child(TextInput::new(&self.users))
            .child(TextInput::new(&self.roles))
            .child(
                div()
                    .id("channel-access")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.action(Op::Access, cx)))
                    .child("Сохранить доступ"),
            );
        if self.kind == "news" {
            root = root
                .child(TextInput::new(&self.publishers))
                .child(
                    div()
                        .id("comments-toggle")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.comments = !s.comments;
                            cx.notify();
                        }))
                        .child(format!(
                            "Комментарии: {}",
                            if self.comments { "да" } else { "нет" }
                        )),
                )
                .child(
                    div()
                        .id("channel-news")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| s.action(Op::News, cx)))
                        .child("Сохранить настройки новостей"),
                );
        }
        if self.kind == "voice" {
            root = root
                .child(
                    div()
                        .id("e2ee-toggle")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.encryption = !s.encryption;
                            cx.notify();
                        }))
                        .child(format!(
                            "Требовать шифрование: {}",
                            if self.encryption { "да" } else { "нет" }
                        )),
                )
                .child(
                    div()
                        .id("channel-e2ee")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| s.action(Op::Encryption, cx)))
                        .child("Сохранить шифрование"),
                );
        }
        root = root.child(
            div()
                .id("channel-delete")
                .cursor_pointer()
                .on_click(cx.listener(|s, _, _, cx| s.action(Op::Delete, cx)))
                .child(if self.confirm_delete {
                    "Подтвердить удаление канала"
                } else {
                    "Удалить канал"
                }),
        );
        root.child(if self.busy { "Загрузка…" } else { "" })
            .child(self.notice.clone())
            .child(div().text_color(rgb(0xff7777)).child(self.error.clone()))
            .into_any_element()
    }
}
