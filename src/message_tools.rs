use gpui::{div, prelude::*, px, rgb, Context, Entity, Window};
use gpui_component::input::{InputState, TextInput};
use std::sync::Arc;
use voxhold_api::{Client, Message, ReactionsUpdate};

use crate::reactions::{self, ReactionPicker, SharedRecents};

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
    picker: Entity<ReactionPicker>,
    recents: SharedRecents,
    on_reaction: Option<Box<dyn Fn(ReactionsUpdate, &mut gpui::App) + 'static>>,
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
        recents: SharedRecents,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| ReactionPicker::new(recents.clone(), window, cx));
        let weak = cx.entity().downgrade();
        picker.update(cx, |picker, _| {
            picker.on_pick = Some(Box::new(move |value, cx| {
                if let Some(tools) = weak.upgrade() {
                    tools.update(cx, |tools, cx| tools.react_with(value, cx));
                }
            }));
        });
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
            picker,
            recents,
            on_reaction: None,
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
        let mine = self
            .message
            .as_ref()
            .map(|message| {
                message
                    .reactions
                    .iter()
                    .filter(|reaction| reaction.me)
                    .map(|reaction| reaction.emoji.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.picker
            .update(cx, |picker, cx| picker.reset_for_message(mine, cx));
        cx.notify();
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn selected_id(&self) -> Option<i64> {
        self.message.as_ref().map(|message| message.id)
    }
    pub(crate) fn set_reaction_handler(
        &mut self,
        handler: Option<Box<dyn Fn(ReactionsUpdate, &mut gpui::App) + 'static>>,
    ) {
        self.on_reaction = handler;
    }
    /// Keep the open panel aligned with a row confirmed by history or a
    /// realtime event. Revision rules stay in the same place as the feed.
    pub fn sync_message(&mut self, message: Option<&Message>, cx: &mut Context<Self>) {
        let Some(current) = self.message.as_ref() else {
            return;
        };
        let id = current.id;
        match message.filter(|message| message.id == id) {
            Some(incoming) => {
                let before = (current.revision, current.reactions_revision);
                let mut merged = current.clone();
                crate::merge_revision(&mut merged, incoming.clone());
                let changed = before != (merged.revision, merged.reactions_revision);
                self.message = Some(merged);
                self.sync_picker(cx);
                if changed {
                    cx.notify();
                }
            }
            None => {
                self.message = None;
                self.picker
                    .update(cx, |picker, cx| picker.set_mine(Vec::new(), cx));
                cx.notify();
            }
        }
    }
    fn sync_picker(&mut self, cx: &mut Context<Self>) {
        let mine = self
            .message
            .as_ref()
            .map(|message| {
                message
                    .reactions
                    .iter()
                    .filter(|reaction| reaction.me)
                    .map(|reaction| reaction.emoji.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.picker
            .update(cx, |picker, cx| picker.set_mine(mine, cx));
    }
    fn reaction_is_active(&self, emoji: &str) -> bool {
        self.message.as_ref().is_some_and(|message| {
            message
                .reactions
                .iter()
                .any(|reaction| reaction.emoji == emoji && reaction.me)
        })
    }
    /// `true` means the next request is PUT, `false` means DELETE.
    pub(crate) fn reaction_request_active(&self, emoji: &str) -> bool {
        !self.reaction_is_active(emoji)
    }
    /// Apply the confirmed server update; a stale or wrong-row answer keeps the
    /// last confirmed state. Returns whether the panel changed.
    fn apply_reaction_update(&mut self, update: ReactionsUpdate, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.message.as_mut() else {
            return false;
        };
        if update.message_id != message.id {
            return false;
        }
        let before = message.reactions_revision;
        crate::apply_reactions(message, update);
        if message.reactions_revision == before {
            return false;
        }
        self.sync_picker(cx);
        cx.notify();
        true
    }
    /// A confirmed reaction applies to the panel and to the shared history row,
    /// but it is not a history edit and must not trigger a full reload.
    pub(crate) fn handle_reaction_result(
        &mut self,
        update: ReactionsUpdate,
        emoji: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let parent_update = update.clone();
        if !self.apply_reaction_update(update, cx) {
            return false;
        }
        if self.reaction_is_active(emoji) {
            reactions::touch_recent(&mut self.recents.borrow_mut(), emoji);
        }
        if let Some(handler) = &self.on_reaction {
            handler(parent_update, cx);
        }
        true
    }
    pub fn react_with(&mut self, value: &str, cx: &mut Context<Self>) {
        self.run_with(Action::React, Some(value.to_string()), cx);
    }
    fn run(&mut self, action: Action, cx: &mut Context<Self>) {
        self.run_with(action, None, cx);
    }
    fn run_with(&mut self, action: Action, emoji_override: Option<String>, cx: &mut Context<Self>) {
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
        let emoji = emoji_override.unwrap_or_else(|| self.emoji.read(cx).value().to_string());
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
        let react_value = emoji.clone();
        let react_active = self.reaction_request_active(&emoji);
        // A stable random client key is generated before invoking idempotent endpoints.
        let key = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let result: voxhold_api::Result<ActionResult> = match action {
                    Action::Reply => client
                        .reply_message(server, channel, message.id, &text)
                        .await
                        .map(|_| ActionResult::Done),
                    Action::Edit => client
                        .edit_message(server, channel, message.id, &text)
                        .await
                        .map(|_| ActionResult::Done),
                    Action::Delete => client
                        .delete_message(server, channel, message.id)
                        .await
                        .map(|_| ActionResult::Done),
                    Action::Favorite => client
                        .save_favorite(message.id, &key)
                        .await
                        .map(|_| ActionResult::Done),
                    Action::Forward => client
                        .forward_message(server, channel, message.id, target, &key)
                        .await
                        .map(|_| ActionResult::Done),
                    Action::React => client
                        .set_reaction(server, channel, message.id, &emoji, react_active)
                        .await
                        .map(ActionResult::Reactions),
                    Action::Pin => client
                        .set_message_pin(server, channel, message.id, true)
                        .await
                        .map(|_| ActionResult::Done),
                    Action::Unpin => client
                        .set_message_pin(server, channel, message.id, false)
                        .await
                        .map(|_| ActionResult::Done),
                    Action::Comments => client
                        .thread_messages(server, channel, message.id, 0)
                        .await
                        .map(|page| ActionResult::Comments(page.messages)),
                    Action::Comment => client
                        .send_thread_message(server, channel, message.id, &text, &key)
                        .await
                        .map(|_| ActionResult::Done),
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
                        Ok(ActionResult::Comments(comments)) => s.comments = comments,
                        Ok(ActionResult::Reactions(update)) => {
                            // Keep the selected row and picker; do not bump
                            // `revision`, which reloads the newest page.
                            s.handle_reaction_result(update, &react_value, cx);
                        }
                        Ok(ActionResult::Done) => s.revision += 1,
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

enum ActionResult {
    Done,
    Comments(Vec<Message>),
    Reactions(ReactionsUpdate),
}
impl Render for MessageTools {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut view = div()
            .id("message-tools")
            .w(px(320.))
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .overflow_y_scroll()
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
            view = view
                .child(div().text_size(px(12.)).child("Быстрые и все реакции"))
                .child(self.picker.clone())
                .child(TextInput::new(&self.emoji))
                .child(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use voxhold_api::{Author, Reaction};

    fn message(id: i64) -> Message {
        Message {
            id,
            channel_id: 7,
            content: "row".into(),
            author: Author {
                user_id: 1,
                username: "neva".into(),
            },
            ..Default::default()
        }
    }

    fn update(message_id: i64, revision: i64, me: bool) -> ReactionsUpdate {
        ReactionsUpdate {
            channel_id: 7,
            message_id,
            revision,
            reactions: if me {
                vec![Reaction {
                    emoji: "🔥".into(),
                    count: 1,
                    me: true,
                }]
            } else {
                Vec::new()
            },
        }
    }

    #[gpui::test]
    async fn reaction_clicks_use_the_last_confirmed_server_state(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        let slot: Rc<RefCell<Option<Entity<MessageTools>>>> = Rc::new(RefCell::new(None));
        let captured = slot.clone();
        let window = cx.add_window(move |window, cx| {
            let client = Client::new("http://127.0.0.1:1").unwrap();
            let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
            let recents = Rc::new(RefCell::new(Vec::new()));
            let tools = cx.new(|cx| MessageTools::new(client, runtime, recents, window, cx));
            tools.update(cx, |tools, cx| {
                tools.select(
                    1,
                    7,
                    Some(message(40)),
                    MessagePermissions {
                        react: true,
                        ..Default::default()
                    },
                    cx,
                );
            });
            *captured.borrow_mut() = Some(tools.clone());
            gpui_component::Root::new(tools.into(), window, cx)
        });
        let tools = slot.borrow().clone().expect("tools");
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|_, app| {
            tools.update(app, |tools, cx| {
                // First click would be PUT, then the server confirms `me`.
                assert!(tools.reaction_request_active("🔥"));
                assert!(tools.apply_reaction_update(update(40, 1, true), cx));
                assert!(tools.reaction_is_active("🔥"));
                assert!(tools.picker.read(cx).mine.contains("🔥"));

                // Second click would be DELETE; stale and wrong-row answers
                // must keep the last confirmed state.
                assert!(!tools.reaction_request_active("🔥"));
                assert!(!tools.apply_reaction_update(update(40, 1, false), cx));
                assert!(!tools.apply_reaction_update(update(41, 9, false), cx));
                assert!(tools.reaction_is_active("🔥"));
                assert!(tools.picker.read(cx).mine.contains("🔥"));

                assert!(tools.apply_reaction_update(update(40, 2, false), cx));
                assert!(!tools.reaction_is_active("🔥"));
                assert!(!tools.picker.read(cx).mine.contains("🔥"));

                // A WS/history row with a newer reaction revision updates the
                // panel without changing the selection.
                let mut incoming = message(40);
                incoming.reactions = update(40, 3, true).reactions;
                incoming.reactions_revision = 3;
                tools.sync_message(Some(&incoming), cx);
                assert!(tools.reaction_is_active("🔥"));
                assert!(tools.picker.read(cx).mine.contains("🔥"));
            });
        });
    }
}
