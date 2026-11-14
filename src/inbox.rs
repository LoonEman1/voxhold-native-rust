use gpui::{div, prelude::*, px, rgb, Context, Entity, Window};
use gpui_component::input::{InputState, TextInput};
use std::sync::Arc;
use voxhold_api::{Client, Conversation, Message};

pub struct Inbox {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
    composer: Entity<InputState>,
    conversations: Vec<Conversation>,
    selected: i64,
    messages: Vec<Message>,
    busy: bool,
    generation: u64,
    error: String,
}
impl Inbox {
    pub fn new(
        client: Client,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            client,
            runtime,
            composer: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line()
                    .placeholder("Сообщение")
            }),
            conversations: vec![],
            selected: 0,
            messages: vec![],
            busy: false,
            generation: 0,
            error: String::new(),
        }
    }
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run(Operation::List, window, cx);
    }
    fn select(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = id;
        self.messages.clear();
        self.generation += 1;
        self.busy = false;
        self.run(Operation::History, window, cx);
    }
    fn run(&mut self, op: Operation, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let text = self.composer.read(cx).value().to_string();
        if matches!(op, Operation::Send)
            && (self.selected <= 0 || text.trim().is_empty() || text.chars().count() > 5120)
        {
            self.error = "Введите сообщение длиной до 5120 символов.".into();
            cx.notify();
            return;
        }
        self.busy = true;
        self.error.clear();
        let generation = self.generation;
        let selected = self.selected;
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let key = uuid::Uuid::new_v4().simple().to_string();
        cx.notify();
        let sent_text = text.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                match op {
                    Operation::List => client.recent_conversations().await.map(Response::List),
                    Operation::History => client
                        .conversation_messages(selected, 0)
                        .await
                        .map(|p| Response::History(p.messages)),
                    Operation::Send => client
                        .create_conversation_message(selected, &text, &key, &[])
                        .await
                        .map(Response::Sent),
                }
                .map_err(|e| e.to_string())
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.generation != generation {
                    return;
                }
                s.busy = false;
                match result {
                    Ok(Response::List(v)) => s.conversations = v,
                    Ok(Response::History(mut v)) => {
                        v.sort_by_key(|m| m.id);
                        s.messages = v
                    }
                    Ok(Response::Sent(m)) => {
                        if s.composer.read(cx).value().as_ref() == sent_text {
                            s.composer
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        s.messages.push(m);
                        if s.messages.len() > 200 {
                            s.messages.remove(0);
                        }
                    }
                    Err(e) => s.error = e,
                };
                cx.notify();
            });
        })
        .detach();
    }
}
#[derive(Clone, Copy)]
enum Operation {
    List,
    History,
    Send,
}
enum Response {
    List(Vec<Conversation>),
    History(Vec<Message>),
    Sent(Message),
}
impl Render for Inbox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_2().child("Последние диалоги");
        for c in &self.conversations {
            let id = c.id;
            let title = if c.name.is_empty() {
                c.members
                    .iter()
                    .map(|m| m.username.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                c.name.clone()
            };
            list = list.child(
                div()
                    .id(("conversation", id as u64))
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, window, cx| s.select(id, window, cx)))
                    .child(format!("{} ({})", title, c.unread_count)),
            );
        }
        let mut history = div()
            .id("inbox-history")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2();
        for m in &self.messages {
            history = history.child(div().child(format!("{}: {}", m.author.username, m.content)));
        }
        let mut root = div()
            .w(px(380.))
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x202323))
            .text_color(rgb(0xf3f4eb))
            .child(
                div()
                    .id("inbox-refresh")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, window, cx| s.load(window, cx)))
                    .child("Обновить диалоги"),
            )
            .child(list)
            .child(history);
        if self.selected > 0 {
            root = root.child(TextInput::new(&self.composer)).child(
                div()
                    .id("inbox-send")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, window, cx| s.run(Operation::Send, window, cx)))
                    .child("Отправить"),
            );
        }
        root.child(if self.busy { "Загрузка…" } else { "" })
            .child(div().text_color(rgb(0xff7777)).child(self.error.clone()))
    }
}
