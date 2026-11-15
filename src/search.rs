//! Search and pins popup, ported from Go `search_pins.go`.
//!
//! Every request is scoped by `(server, channel, generation)` and a monotonic
//! request id. A new query or a channel switch invalidates the previous answer,
//! so a stale page cannot repaint the popup.

use gpui::{div, prelude::*, px, App, Context, Entity, Render, Window};
use gpui_component::input::{InputEvent, InputState, TextInput};
use std::sync::Arc;
use voxhold_api::{Client, Message, SearchResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Search,
    Pins,
}

pub type SelectHandler = Box<dyn Fn(i64, i64, &mut Window, &mut App) + 'static>;
pub type PinsHandler = Box<dyn Fn(Vec<Message>, &mut App) + 'static>;

/// A reply may paint only while its request id, scope generation and submitted
/// query all still match. Extracted so the stale-page rule is testable without
/// a window.
pub fn accepts_page(
    request: u64,
    current_request: u64,
    scope: u64,
    current_scope: u64,
    submitted: &str,
    current_submitted: &str,
) -> bool {
    request == current_request
        && scope == current_scope
        && (submitted.is_empty() || submitted == current_submitted)
}

pub struct SearchPanel {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
    server: i64,
    channel: i64,
    mode: Mode,
    query: Entity<InputState>,
    submitted: String,
    results: Vec<SearchResult>,
    pins: Vec<Message>,
    next: Option<i64>,
    has_more: bool,
    loading: bool,
    loading_more: bool,
    error: String,
    request: u64,
    scope_generation: u64,
    on_select: Option<SelectHandler>,
    on_pins: Option<PinsHandler>,
    _subscription: gpui::Subscription,
}

impl SearchPanel {
    pub fn new(
        client: Client,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Поиск сообщений"));
        let subscription = cx.subscribe(&query, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                // Changing the text owns the next answer: the old request id
                // can no longer paint results.
                this.request = this.request.wrapping_add(1);
                this.results.clear();
                this.next = None;
                this.has_more = false;
                this.loading = false;
                this.loading_more = false;
                this.error.clear();
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.submit(cx),
            _ => {}
        });
        Self {
            client,
            runtime,
            server: 0,
            channel: 0,
            mode: Mode::Search,
            query,
            submitted: String::new(),
            results: Vec::new(),
            pins: Vec::new(),
            next: None,
            has_more: false,
            loading: false,
            loading_more: false,
            error: String::new(),
            request: 0,
            scope_generation: 0,
            on_select: None,
            on_pins: None,
            _subscription: subscription,
        }
    }

    pub fn set_select_handler(&mut self, handler: SelectHandler) {
        self.on_select = Some(handler);
    }

    pub fn set_pins_handler(&mut self, handler: PinsHandler) {
        self.on_pins = Some(handler);
    }

    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::Search {
            self.query.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    /// A new header click starts a fresh popup. The old request is invalidated
    /// even when the scope did not change.
    pub fn open(&mut self, server: i64, channel: i64, mode: Mode, cx: &mut Context<Self>) {
        self.request = self.request.wrapping_add(1);
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.mode = mode;
        self.server = server;
        self.channel = channel;
        self.submitted.clear();
        self.results.clear();
        self.pins.clear();
        self.next = None;
        self.has_more = false;
        self.loading = false;
        self.loading_more = false;
        self.error.clear();
        if mode == Mode::Pins {
            self.load_pins(cx);
        }
        cx.notify();
    }

    /// Navigation change: drop every scope-bound answer and selection.
    pub fn set_scope(&mut self, server: i64, channel: i64, cx: &mut Context<Self>) {
        if self.server == server && self.channel == channel {
            return;
        }
        self.server = server;
        self.channel = channel;
        self.request = self.request.wrapping_add(1);
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.submitted.clear();
        self.results.clear();
        self.pins.clear();
        self.next = None;
        self.has_more = false;
        self.loading = false;
        self.loading_more = false;
        self.error.clear();
        cx.notify();
    }

    fn accepts(&self, request: u64, scope: u64, submitted: &str) -> bool {
        accepts_page(
            request,
            self.request,
            scope,
            self.scope_generation,
            submitted,
            &self.submitted,
        )
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_string();
        if query.is_empty() || self.server <= 0 {
            return;
        }
        self.request = self.request.wrapping_add(1);
        let request = self.request;
        let scope = self.scope_generation;
        let submitted = query.clone();
        self.submitted = query.clone();
        self.results.clear();
        self.next = None;
        self.has_more = false;
        self.loading = true;
        self.loading_more = false;
        self.error.clear();
        cx.notify();
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let server = self.server;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.search_messages(server, &query, 0))
                .map_err(|error| error.to_string())
        });
        let expected = submitted.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if !s.accepts(request, scope, &expected) {
                        return;
                    }
                    s.loading = false;
                    match result {
                        Ok(page) => {
                            s.results = page.messages;
                            s.next = page.pagination.next_before_id;
                            s.has_more = page.pagination.has_more
                                && s.next.is_some_and(|id| id > 0)
                                && s.results.len() < 200;
                            s.results.truncate(200);
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

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.loading_more || !self.has_more {
            return;
        }
        let Some(before) = self.next.filter(|id| *id > 0) else {
            return;
        };
        self.request = self.request.wrapping_add(1);
        let request = self.request;
        let scope = self.scope_generation;
        let submitted = self.submitted.clone();
        self.loading_more = true;
        self.error.clear();
        cx.notify();
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let server = self.server;
        let query = submitted.clone();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.search_messages(server, &submitted, before))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if !s.accepts(request, scope, &query) {
                        return;
                    }
                    s.loading_more = false;
                    match result {
                        Ok(page) => {
                            s.results.extend(page.messages);
                            s.results.truncate(200);
                            s.next = page.pagination.next_before_id;
                            s.has_more = page.pagination.has_more
                                && s.next.is_some_and(|id| id > 0)
                                && s.results.len() < 200;
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

    pub fn reload_pins(&mut self, cx: &mut Context<Self>) {
        if self.server <= 0 || self.channel <= 0 {
            return;
        }
        self.load_pins(cx);
    }

    fn load_pins(&mut self, cx: &mut Context<Self>) {
        if self.server <= 0 || self.channel <= 0 {
            return;
        }
        self.request = self.request.wrapping_add(1);
        let request = self.request;
        let scope = self.scope_generation;
        self.pins.clear();
        self.loading = true;
        self.error.clear();
        cx.notify();
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let (server, channel) = (self.server, self.channel);
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.pinned_message_list(server, channel))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if !s.accepts(request, scope, "") {
                        return;
                    }
                    s.loading = false;
                    match result {
                        Ok(pins) => {
                            s.pins = pins
                                .into_iter()
                                .map(|pin| pin.message)
                                .filter(|message| message.id > 0 && message.channel_id == channel)
                                .take(200)
                                .collect();
                            if let Some(handler) = &s.on_pins {
                                handler(s.pins.clone(), cx);
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

    fn select(&mut self, channel: i64, message: i64, window: &mut Window, cx: &mut App) {
        if let Some(handler) = &self.on_select {
            handler(channel, message, window, cx);
        }
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::theme::dark_palette();
        let title = if self.mode == Mode::Pins {
            "Закреплённые сообщения"
        } else {
            "Поиск по серверу"
        };
        let channel = self.channel;
        let mut popup = div()
            .id("search-popup")
            .w(px(360.))
            .max_h(px(420.))
            .occlude()
            .rounded(px(10.))
            .shadow_lg()
            .border_1()
            .border_color(crate::ink(p.line))
            .bg(crate::ink(p.surface))
            .text_color(crate::ink(p.fg))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title),
            );
        if self.mode == Mode::Search {
            popup = popup
                .child(
                    div()
                        .capture_key_down(cx.listener(|s, event: &gpui::KeyDownEvent, _, cx| {
                            let key = &event.keystroke;
                            if key.key == "enter"
                                && !key.modifiers.shift
                                && !key.modifiers.control
                                && !key.modifiers.alt
                                && !key.modifiers.platform
                            {
                                s.submit(cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(TextInput::new(&self.query)),
                )
                .child(
                    div()
                        .id("search-submit")
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(7.))
                        .bg(crate::ink(p.selection))
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| s.submit(cx)))
                        .child("Найти"),
                );
        }
        let mut list = div()
            .id("search-results")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        if self.mode == Mode::Pins {
            if self.loading {
                list = list.child(
                    div()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.muted))
                        .child("Загрузка…"),
                );
            } else if !self.error.is_empty() {
                list = list.child(
                    div()
                        .id("pins-retry")
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.danger))
                        .on_click(cx.listener(|s, _, _, cx| s.load_pins(cx)))
                        .child(format!("{} · Повторить", self.error)),
                );
            } else if self.pins.is_empty() {
                list = list.child(
                    div()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.muted))
                        .child("Нет закреплённых сообщений"),
                );
            }
            for message in &self.pins {
                let id = message.id;
                let label = if message.content.is_empty() {
                    format!("{}: вложение", message.author.username)
                } else {
                    format!(
                        "{}: {}",
                        message.author.username,
                        message.content.replace('\n', " ")
                    )
                };
                list = list.child(
                    div()
                        .id(("pin-row", id as u64))
                        .cursor_pointer()
                        .rounded(px(6.))
                        .px(px(6.))
                        .py(px(5.))
                        .hover(|row| row.bg(crate::ink(p.selection)))
                        .text_size(px(12.))
                        .truncate()
                        .on_click(
                            cx.listener(move |s, _, window, cx| s.select(channel, id, window, cx)),
                        )
                        .child(label),
                );
            }
        } else if self.loading && self.results.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(crate::ink(p.muted))
                    .child("Загрузка…"),
            );
        } else if !self.error.is_empty() {
            list = list.child(
                div()
                    .id("search-retry")
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(crate::ink(p.danger))
                    .on_click(cx.listener(|s, _, _, cx| s.submit(cx)))
                    .child(format!("{} · Повторить", self.error)),
            );
        } else if self.results.is_empty() && !self.submitted.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(crate::ink(p.muted))
                    .child("Ничего не найдено"),
            );
        } else if self.results.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(crate::ink(p.muted))
                    .child("Введите запрос и нажмите Enter"),
            );
        }
        if self.mode == Mode::Search {
            for hit in &self.results {
                let channel_id = hit.message.channel_id;
                let id = hit.message.id;
                let label = format!(
                    "#{} · {}: {}",
                    hit.channel_name,
                    hit.message.author.username,
                    hit.message.content.replace('\n', " ")
                );
                list =
                    list.child(
                        div()
                            .id(("search-hit", id as u64))
                            .cursor_pointer()
                            .rounded(px(6.))
                            .px(px(6.))
                            .py(px(5.))
                            .hover(|row| row.bg(crate::ink(p.selection)))
                            .text_size(px(12.))
                            .truncate()
                            .on_click(cx.listener(move |s, _, window, cx| {
                                s.select(channel_id, id, window, cx)
                            }))
                            .child(label),
                    );
            }
            if self.has_more {
                list = list.child(
                    div()
                        .id("search-more")
                        .cursor_pointer()
                        .py(px(4.))
                        .text_size(px(12.))
                        .text_color(crate::ink(p.lime))
                        .on_click(cx.listener(|s, _, _, cx| s.load_more(cx)))
                        .child(if self.loading_more {
                            "Загрузка…"
                        } else {
                            "Показать ещё"
                        }),
                );
            }
        }
        popup.child(list)
    }
}
