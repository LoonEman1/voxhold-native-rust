use gpui::{div, prelude::*, px, rgb, Context, Entity, Window};
use gpui_component::input::{InputState, TextInput};
use std::sync::Arc;
use voxhold_api::{Client, Message};

/// Actions for a selected message. Parent supplies the current selection and
/// reloads history after an action by observing `revision()`.
pub struct MessageTools {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
    server: i64,
    channel: i64,
    message: Option<Message>,
    text: Entity<InputState>,
    emoji: Entity<InputState>,
    destination: Entity<InputState>,
    comments: Vec<Message>,
    error: String,
    busy: bool,
    generation: u64,
    revision: u64,
    permissions: MessagePermissions,
    confirming_delete: bool,
}
impl MessageTools {
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
            message: None,
            text: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line()
                    .placeholder("Текст ответа или изменения")
            }),
            emoji: cx.new(|cx| InputState::new(window, cx).placeholder("Эмодзи")),
            destination: cx
                .new(|cx| InputState::new(window, cx).placeholder("ID канала для пересылки")),
            comments: Vec::new(),
            error: String::new(),
            busy: false,
            generation: 0,
            revision: 0,
            permissions: MessagePermissions::default(),
            confirming_delete: false,
        }
    }
    pub fn select(
        &mut self,
        server: i64,
        channel: i64,
        message: Option<Message>,
        permissions: MessagePermissions,
        cx: &mut Context<Self>,
    ) {
        self.server = server;
        self.channel = channel;
        self.message = message;
        self.permissions = permissions;
        self.confirming_delete = false;
        self.comments.clear();
        self.error.clear();
        self.busy = false;
        self.generation += 1;
        cx.notify();
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    fn run(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.busy || !self.permissions.allows(action) {
            return;
        }
        if matches!(action, Action::Delete) && !self.confirming_delete {
            self.confirming_delete = true;
            cx.notify();
            return;
        }
        let Some(message) = self.message.clone() else {
            return;
        };
        let text = self.text.read(cx).value().to_string();
        let emoji = self.emoji.read(cx).value().to_string();
        let destination = self.destination.read(cx).value().to_string();
        if matches!(action, Action::Reply | Action::Edit | Action::Comment)
            && (text.trim().is_empty() || text.chars().count() > 5120)
        {
            self.error = "Введите текст сообщения длиной не более 5120 символов.".into();
            cx.notify();
            return;
        }
        if matches!(action, Action::React) && emoji.trim().is_empty() {
            self.error = "Введите эмодзи.".into();
            cx.notify();
            return;
        }
        let target = if matches!(action, Action::Forward) {
            match destination.trim().parse::<i64>() {
                Ok(id) if id > 0 => id,
                _ => {
                    self.error = "Укажите ID канала.".into();
                    cx.notify();
                    return;
                }
            }
        } else {
            0
        };
        self.busy = true;
        self.confirming_delete = false;
        self.error.clear();
        let generation = self.generation;
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let server = self.server;
        let channel = self.channel;
        // A stable random client key is generated before invoking idempotent endpoints.
        let key = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let result: voxhold_api::Result<Option<Vec<Message>>> = match action {
                    Action::Reply => client
                        .reply_message(server, channel, message.id, &text)
                        .await
                        .map(|_| None),
                    Action::Edit => client
                        .edit_message(server, channel, message.id, &text)
                        .await
                        .map(|_| None),
                    Action::Delete => client
                        .delete_message(server, channel, message.id)
                        .await
                        .map(|_| None),
                    Action::Favorite => client.save_favorite(message.id, &key).await.map(|_| None),
                    Action::Forward => client
                        .forward_message(server, channel, message.id, target, &key)
                        .await
                        .map(|_| None),
                    Action::React => {
                        let active = !message.reactions.iter().any(|r| r.emoji == emoji && r.me);
                        client
                            .set_reaction(server, channel, message.id, &emoji, active)
                            .await
                            .map(|_| None)
                    }
                    Action::Pin => client
                        .set_message_pin(server, channel, message.id, true)
                        .await
                        .map(|_| None),
                    Action::Unpin => client
                        .set_message_pin(server, channel, message.id, false)
                        .await
                        .map(|_| None),
                    Action::Comments => client
                        .thread_messages(server, channel, message.id, 0)
                        .await
                        .map(|p| Some(p.messages)),
                    Action::Comment => client
                        .send_thread_message(server, channel, message.id, &text, &key)
                        .await
                        .map(|_| None),
                };
                result.map_err(|e| e.to_string())
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
                        Ok(Some(comments)) => s.comments = comments,
                        Ok(None) => s.revision += 1,
                        Err(e) => s.error = e,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}
/// Permissions derived from the selected channel, effective server role and current user.
/// Defaults deny every operation until the caller has established those facts.
#[derive(Clone, Copy, Default)]
pub struct MessagePermissions {
    pub write: bool,
    pub edit: bool,
    pub delete: bool,
    pub react: bool,
    pub pin: bool,
    pub forward: bool,
    pub comments: bool,
}
impl MessagePermissions {
    fn allows(self, action: Action) -> bool {
        match action {
            Action::Reply => self.write,
            Action::Edit => self.edit,
            Action::Delete => self.delete,
            Action::Forward | Action::Favorite => self.forward,
            Action::React => self.react,
            Action::Pin | Action::Unpin => self.pin,
            Action::Comments => self.comments,
            Action::Comment => self.comments && self.write,
        }
    }
}
#[derive(Clone, Copy)]
enum Action {
    Reply,
    Edit,
    Delete,
    Forward,
    Favorite,
    React,
    Pin,
    Unpin,
    Comments,
    Comment,
}
impl Render for MessageTools {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut view = div()
            .w(px(320.))
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x202323))
            .text_color(rgb(0xf3f4eb));
        let Some(message) = &self.message else {
            return view.child("Выберите сообщение").into_any_element();
        };
        let copied = message.content.clone();
        view = view
            .child(format!("{} · #{}", message.author.username, message.id))
            .child(message.content.clone())
            .child(
                div()
                    .id("copy-message")
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone()))
                    })
                    .child("Копировать текст"),
            )
            .child(TextInput::new(&self.text));
        for (id, label, action) in [
            ("reply", "Ответить", Action::Reply),
            ("edit", "Изменить", Action::Edit),
            ("delete", "Удалить", Action::Delete),
            ("favorite", "В избранное", Action::Favorite),
            ("comments", "Открыть обсуждение", Action::Comments),
            ("comment", "Отправить комментарий", Action::Comment),
            ("pin", "Закрепить", Action::Pin),
            ("unpin", "Открепить", Action::Unpin),
        ] {
            if !self.permissions.allows(action) {
                continue;
            }
            let label = if matches!(action, Action::Delete) && self.confirming_delete {
                "Подтвердить удаление"
            } else {
                label
            };
            view = view.child(
                div()
                    .id(id)
                    .p_1()
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, _, cx| s.run(action, cx)))
                    .child(label),
            );
        }
        if self.permissions.react {
            view = view.child(TextInput::new(&self.emoji)).child(
                div()
                    .id("react")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.run(Action::React, cx)))
                    .child("Добавить / убрать реакцию"),
            );
        }
        if self.permissions.forward {
            view = view.child(TextInput::new(&self.destination)).child(
                div()
                    .id("forward")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.run(Action::Forward, cx)))
                    .child("Переслать"),
            );
        }
        let mut comments = div()
            .id("thread-comments")
            .overflow_y_scroll()
            .flex_1()
            .flex()
            .flex_col()
            .gap_2();
        for m in &self.comments {
            comments = comments.child(div().child(format!("{}: {}", m.author.username, m.content)));
        }
        view.child(if self.busy { "Загрузка…" } else { "" })
            .child(div().text_color(rgb(0xff7777)).child(self.error.clone()))
            .child(comments)
            .into_any_element()
    }
}
