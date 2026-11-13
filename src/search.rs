use gpui::{div, prelude::*, px, rgb, Context, Entity, Window};
use gpui_component::input::{InputState, TextInput};
use voxhold_api::{Client, Message, SearchResult};

/// Search server messages and inspect pins for the selected channel.
pub struct SearchPanel {
    client: Client,
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
    server: i64,
    channel: i64,
    query: Entity<InputState>,
    results: Vec<SearchResult>,
    pins: Vec<Message>,
    next: Option<i64>,
    has_more: bool,
    loading: bool,
    error: String,
    generation: u64,
}
impl SearchPanel {
    pub fn new(
        client: Client,
        runtime: std::sync::Arc<tokio::runtime::Runtime>,
        server: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            client,
            runtime,
            server,
            channel: 0,
            query: cx.new(|cx| InputState::new(window, cx).placeholder("Поиск сообщений")),
            results: vec![],
            pins: vec![],
            next: None,
            has_more: false,
            loading: false,
            error: String::new(),
            generation: 0,
        }
    }
    /// Call whenever navigation changes; stale requests cannot overwrite the new view.
    pub fn set_scope(&mut self, server: i64, channel: i64, cx: &mut Context<Self>) {
        if self.server == server && self.channel == channel {
            return;
        }
        self.server = server;
        self.channel = channel;
        self.results.clear();
        self.pins.clear();
        self.next = None;
        self.has_more = false;
        self.loading = false;
        self.error.clear();
        self.generation += 1;
        cx.notify();
    }
    fn search(&mut self, more: bool, cx: &mut Context<Self>) {
        if self.loading || self.server <= 0 {
            return;
        }
        let query = self.query.read(cx).value().to_string();
        if query.trim().is_empty() {
            return;
        }
        let before = if more { self.next.unwrap_or(0) } else { 0 };
        if more && before <= 0 {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        self.error.clear();
        cx.notify();
        let client = self.client.clone();
        let rt = self.runtime.clone();
        let server = self.server;
        let task = cx.background_spawn(async move {
            rt.block_on(client.search_messages(server, &query, before))
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    s.loading = false;
                    match result {
                        Ok(page) => {
                            if !more {
                                s.results.clear();
                            }
                            s.results.extend(page.messages);
                            s.results.truncate(200);
                            s.next = page.pagination.next_before_id;
                            s.has_more = page.pagination.has_more
                                && s.next.is_some()
                                && s.results.len() < 200;
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
    fn load_pins(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.channel <= 0 {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        self.error.clear();
        cx.notify();
        let client = self.client.clone();
        let rt = self.runtime.clone();
        let server = self.server;
        let channel = self.channel;
        let task = cx.background_spawn(async move {
            rt.block_on(client.pinned_message_list(server, channel))
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.generation != generation {
                        return;
                    }
                    s.loading = false;
                    match result {
                        Ok(pins) => {
                            s.pins = pins.into_iter().map(|p| p.message).take(200).collect()
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
}
impl Render for SearchPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut results = div()
            .id("search-results")
            .overflow_y_scroll()
            .flex_1()
            .flex()
            .flex_col()
            .gap_3();
        for result in &self.results {
            results = results.child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_color(rgb(0xb5cf16)).child(format!(
                        "#{} · {}",
                        result.channel_name, result.message.author.username
                    )))
                    .child(result.message.content.clone()),
            );
        }
        if self.has_more {
            results = results.child(
                div()
                    .id("search-more")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.search(true, cx)))
                    .child("Показать ещё"),
            );
        }
        let mut pins = div().flex().flex_col().gap_2();
        for message in &self.pins {
            pins = pins.child(
                div()
                    .flex()
                    .flex_col()
                    .child(message.author.username.clone())
                    .child(message.content.clone()),
            );
        }
        div()
            .w(px(300.))
            .h_full()
            .p_3()
            .bg(rgb(0x191c1c))
            .text_color(rgb(0xf3f4eb))
            .flex()
            .flex_col()
            .gap_3()
            .child("Поиск по серверу")
            .child(TextInput::new(&self.query))
            .child(
                div()
                    .id("search-submit")
                    .p_2()
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.search(false, cx)))
                    .child("Найти"),
            )
            .child(
                div()
                    .id("pins-load")
                    .p_2()
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.load_pins(cx)))
                    .child("Закреплённые в канале"),
            )
            .child(if self.loading {
                "Загрузка…"
            } else {
                ""
            })
            .child(div().text_color(rgb(0xff7777)).child(self.error.clone()))
            .child(pins)
            .child(results)
    }
}
