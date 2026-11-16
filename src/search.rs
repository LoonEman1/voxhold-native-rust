//! Search and pins popup, ported from Go `search_pins.go`.
//!
//! Every request is scoped by `(server, channel, generation)` and a monotonic
//! request id. A new query or a channel switch invalidates the previous answer,
//! so a stale page cannot repaint the popup.

use gpui::{div, prelude::*, px, App, Context, Entity, Render, Window};
use gpui_component::input::{InputEvent, InputState, TextInput};
use std::collections::HashSet;
use std::sync::Arc;
use voxhold_api::{Client, Message, SearchPage, SearchResult};

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
    allowed_channels: HashSet<i64>,
    /// No query or result may paint until the channel ACL snapshot used by the
    /// popup is confirmed by the current access generation.
    permissions_ready: bool,
    next: Option<i64>,
    has_more: bool,
    loading: bool,
    loading_more: bool,
    error: String,
    request: u64,
    pins_request: u64,
    pins_loading: bool,
    pins_error: String,
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
            allowed_channels: HashSet::new(),
            permissions_ready: false,
            next: None,
            has_more: false,
            loading: false,
            loading_more: false,
            error: String::new(),
            request: 0,
            pins_request: 0,
            pins_loading: false,
            pins_error: String::new(),
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

    /// Replace the permission snapshot used to disclose result rows. Pagination
    /// keeps the cursor of the original API page, so a fully filtered page can
    /// still be followed by the next one. A changed snapshot invalidates every
    /// in-flight search answer and existing row.
    pub fn set_permissions(&mut self, allowed: HashSet<i64>, ready: bool, cx: &mut Context<Self>) {
        let allowed = if ready { allowed } else { HashSet::new() };
        if self.permissions_ready == ready && self.allowed_channels == allowed {
            return;
        }
        self.permissions_ready = ready;
        self.allowed_channels = allowed;
        self.request = self.request.wrapping_add(1);
        self.pins_request = self.pins_request.wrapping_add(1);
        self.results.clear();
        self.pins.clear();
        self.next = None;
        self.has_more = false;
        self.loading = false;
        self.loading_more = false;
        self.error.clear();
        self.pins_loading = false;
        self.pins_error.clear();
        cx.notify();
    }

    pub fn set_allowed_channels(&mut self, allowed: HashSet<i64>, cx: &mut Context<Self>) {
        self.set_permissions(allowed, true, cx);
    }

    fn filter_results(&self, results: Vec<SearchResult>) -> Vec<SearchResult> {
        results
            .into_iter()
            .filter(|hit| {
                hit.message.id > 0 && self.allowed_channels.contains(&hit.message.channel_id)
            })
            .collect()
    }

    fn apply_search_page(&mut self, page: SearchPage, append: bool) {
        let next = page.pagination.next_before_id;
        let has_more = page.pagination.has_more;
        let mut filtered = self.filter_results(page.messages);
        if filtered.len() > 200 || append {
            let mut results = if append {
                std::mem::take(&mut self.results)
            } else {
                Vec::new()
            };
            results.append(&mut filtered);
            results.truncate(200);
            self.results = results;
        } else {
            self.results = filtered;
        }
        self.next = next;
        self.has_more = has_more && self.next.is_some_and(|id| id > 0) && self.results.len() < 200;
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
        self.pins_request = self.pins_request.wrapping_add(1);
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
        self.pins_loading = false;
        self.error.clear();
        self.pins_error.clear();
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
        self.pins_request = self.pins_request.wrapping_add(1);
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.submitted.clear();
        self.results.clear();
        self.pins.clear();
        self.next = None;
        self.has_more = false;
        self.loading = false;
        self.loading_more = false;
        self.pins_loading = false;
        self.error.clear();
        self.pins_error.clear();
        self.permissions_ready = false;
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

    fn accepts_pins(&self, request: u64, scope: u64) -> bool {
        request == self.pins_request && scope == self.scope_generation
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_string();
        if !self.permissions_ready || query.is_empty() || self.server <= 0 {
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
        let task = runtime.spawn(async move {
            client
                .search_messages(server, &query, 0)
                .await
                .map_err(|error| error.to_string())
        });
        let expected = submitted.clone();
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Запрос поиска прерван.".to_string()));
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if !s.accepts(request, scope, &expected) {
                        return;
                    }
                    s.loading = false;
                    match result {
                        Ok(page) => s.apply_search_page(page, false),
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
        if !self.permissions_ready || self.loading || self.loading_more || !self.has_more {
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
        let task = runtime.spawn(async move {
            client
                .search_messages(server, &submitted, before)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Запрос поиска прерван.".to_string()));
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if !s.accepts(request, scope, &query) {
                        return;
                    }
                    s.loading_more = false;
                    match result {
                        Ok(page) => s.apply_search_page(page, true),
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
        if !self.permissions_ready || self.server <= 0 || self.channel <= 0 {
            return;
        }
        self.pins_request = self.pins_request.wrapping_add(1);
        let request = self.pins_request;
        let scope = self.scope_generation;
        self.pins.clear();
        self.pins_loading = true;
        self.pins_error.clear();
        cx.notify();
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let (server, channel) = (self.server, self.channel);
        let task = runtime.spawn(async move {
            client
                .pinned_message_list(server, channel)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Запрос закрепов прерван.".to_string()));
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if !s.accepts_pins(request, scope) {
                        return;
                    }
                    s.pins_loading = false;
                    match result {
                        Ok(pins) => {
                            s.pins = pins
                                .into_iter()
                                .map(|pin| pin.message)
                                .filter(|message| {
                                    message.id > 0
                                        && message.channel_id == channel
                                        && s.allowed_channels.contains(&channel)
                                })
                                .take(200)
                                .collect();
                            if let Some(handler) = &s.on_pins {
                                handler(s.pins.clone(), cx);
                            }
                        }
                        Err(error) => {
                            s.pins.clear();
                            s.pins_error = error;
                        }
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

    #[cfg(test)]
    pub(crate) fn select_for_test(
        &mut self,
        channel: i64,
        message: i64,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        self.select(channel, message, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn results(&self) -> &[SearchResult] {
        &self.results
    }

    #[cfg(test)]
    pub(crate) fn pins(&self) -> &[Message] {
        &self.pins
    }

    #[cfg(test)]
    pub(crate) fn search_loading(&self) -> bool {
        self.loading
    }

    #[cfg(test)]
    pub(crate) fn pins_loading(&self) -> bool {
        self.pins_loading
    }

    #[cfg(test)]
    pub(crate) fn set_query_for_test(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = text.to_string();
        self.query
            .update(cx, move |input, cx| input.set_value(text, window, cx));
    }

    #[cfg(test)]
    pub(crate) fn submit_for_test(&mut self, cx: &mut Context<Self>) {
        self.submit(cx);
    }

    #[cfg(test)]
    pub(crate) fn load_more_for_test(&mut self, cx: &mut Context<Self>) {
        self.load_more(cx);
    }

    #[cfg(test)]
    pub(crate) fn has_more(&self) -> bool {
        self.has_more
    }

    #[cfg(test)]
    pub(crate) fn permissions_ready(&self) -> bool {
        self.permissions_ready
    }

    #[cfg(test)]
    pub(crate) fn allowed_for_test(&self) -> &HashSet<i64> {
        &self.allowed_channels
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
        if !self.permissions_ready {
            list = list.child(
                div()
                    .text_size(px(12.))
                    .text_color(crate::ink(p.muted))
                    .child("Проверка доступа…"),
            );
        } else if self.mode == Mode::Pins {
            if self.pins_loading {
                list = list.child(
                    div()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.muted))
                        .child("Загрузка…"),
                );
            } else if !self.pins_error.is_empty() {
                list = list.child(
                    div()
                        .id("pins-retry")
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.danger))
                        .on_click(cx.listener(|s, _, _, cx| s.load_pins(cx)))
                        .child(format!("{} · Повторить", self.pins_error)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Gate, Reply, TestServer};
    use gpui::{TestAppContext, VisualTestContext};
    use std::time::Duration;

    fn search_page(rows: serde_json::Value, next: Option<i64>, has_more: bool) -> String {
        serde_json::json!({
            "messages": rows,
            "pagination": {"next_before_id": next, "has_more": has_more},
        })
        .to_string()
    }

    fn hit(id: i64, channel: i64, name: &str, content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "channel_id": channel,
            "conversation_id": 0,
            "content": content,
            "author": {"user_id": 1, "username": "neva"},
            "created_at": 1,
            "revision": 1,
            "comment_count": 0,
            "client_message_id": "",
            "reactions": [],
            "reactions_revision": 0,
            "attachments": [],
            "poll": null,
            "channel_name": name,
        })
    }

    fn panel(
        cx: &mut TestAppContext,
        client: Client,
    ) -> (
        gpui::WindowHandle<SearchPanel>,
        Entity<SearchPanel>,
        VisualTestContext,
    ) {
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let window = cx.add_window(|window, cx| SearchPanel::new(client, runtime, window, cx));
        let view = window.root(cx).unwrap();
        let visual = VisualTestContext::from_window(window.into(), cx);
        (window, view, visual)
    }

    fn wait_panel(
        visual: &mut VisualTestContext,
        view: &Entity<SearchPanel>,
        mut done: impl FnMut(&SearchPanel) -> bool,
    ) {
        for _ in 0..600 {
            visual.run_until_parked();
            if view.read_with(&*visual, |panel, _| done(panel)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the search operation did not finish");
    }

    #[gpui::test]
    async fn search_filters_both_pages_and_keeps_the_source_cursor(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        let first = search_page(
            serde_json::json!([
                hit(10, 7, "open", "visible"),
                hit(9, 8, "closed", "SECRET-ONE"),
            ]),
            Some(9),
            true,
        );
        let second = search_page(
            serde_json::json!([hit(8, 8, "closed", "SECRET-TWO")]),
            None,
            false,
        );
        let server = TestServer::spawn(vec![Reply::new(200, &first), Reply::new(200, &second)]);
        let client = Client::new(&server.base).unwrap();
        let (_window, view, mut visual) = panel(cx, client);
        visual.update(|window, app| {
            view.update(app, |panel, cx| {
                panel.set_allowed_channels(HashSet::from([7]), cx);
                panel.open(1, 7, Mode::Search, cx);
                panel
                    .query
                    .update(cx, |input, cx| input.set_value("query", window, cx));
            });
        });
        // Let the input's Change event settle before submitting; in the UI it
        // always precedes the Enter that starts the search.
        visual.run_until_parked();
        visual.update(|_window, app| {
            view.update(app, |panel, cx| panel.submit_for_test(cx));
        });
        wait_panel(&mut visual, &view, |panel| !panel.search_loading());
        view.read_with(&*visual, |panel, _| {
            assert_eq!(panel.results().len(), 1);
            assert_eq!(panel.results()[0].message.content, "visible");
            assert!(panel.has_more());
            assert!(panel
                .results()
                .iter()
                .all(|hit| hit.message.channel_id == 7));
        });
        visual.update(|_window, app| {
            view.update(app, |panel, cx| panel.load_more_for_test(cx));
        });
        wait_panel(&mut visual, &view, |panel| !panel.search_loading());
        view.read_with(&*visual, |panel, _| {
            assert_eq!(
                panel.results().len(),
                1,
                "a fully filtered page adds no row"
            );
            assert!(panel
                .results()
                .iter()
                .all(|hit| !hit.message.content.contains("SECRET")));
            assert!(panel
                .results()
                .iter()
                .all(|hit| hit.channel_name != "closed"));
        });
        // Re-reading the channel list clears previously painted rows before
        // the new snapshot is confirmed.
        visual.update(|_window, app| {
            view.update(app, |panel, cx| {
                panel.set_permissions(HashSet::new(), false, cx)
            });
        });
        view.read_with(&*visual, |panel, _| {
            assert!(!panel.permissions_ready());
            assert!(panel.results().is_empty());
        });
    }

    #[gpui::test]
    async fn a_revoked_permission_drops_an_in_flight_search_page(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        let gate = Gate::new();
        gate.arm();
        let page = search_page(
            serde_json::json!([hit(10, 7, "open", "SECRET-REVOKED")]),
            None,
            false,
        );
        let server =
            TestServer::spawn_concurrent(vec![Reply::new(200, &page).with_gate(gate.clone())]);
        let client = Client::new(&server.base).unwrap();
        let (_window, view, mut visual) = panel(cx, client);
        visual.update(|window, app| {
            view.update(app, |panel, cx| {
                panel.set_allowed_channels(HashSet::from([7]), cx);
                panel.open(1, 7, Mode::Search, cx);
                panel
                    .query
                    .update(cx, |input, cx| input.set_value("query", window, cx));
            });
        });
        visual.run_until_parked();
        visual.update(|_window, app| {
            view.update(app, |panel, cx| panel.submit_for_test(cx));
        });
        assert!(gate.wait_arrived(Duration::from_secs(3)));
        visual.update(|_window, app| {
            view.update(app, |panel, cx| {
                panel.set_permissions(HashSet::new(), false, cx);
                panel.submit_for_test(cx);
            });
        });
        view.read_with(&*visual, |panel, _| {
            assert!(!panel.permissions_ready());
            assert!(
                !panel.search_loading(),
                "an unconfirmed search cannot start"
            );
            assert!(panel.results().is_empty());
        });
        gate.release();
        wait_panel(&mut visual, &view, |panel| !panel.search_loading());
        view.read_with(&*visual, |panel, _| {
            assert!(panel.results().is_empty());
            assert!(panel
                .results()
                .iter()
                .all(|hit| !hit.message.content.contains("SECRET")));
        });
    }

    #[gpui::test]
    async fn pins_refresh_does_not_cancel_a_search_page(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        let gate = Gate::new();
        gate.arm();
        let search = search_page(
            serde_json::json!([hit(10, 7, "open", "found")]),
            None,
            false,
        );
        let pins = serde_json::json!([{
            "message": hit(10, 7, "open", "pinned"),
            "pinned_by": {"user_id": 1, "username": "neva"},
            "pinned_at": 1,
        }])
        .to_string();
        let server = TestServer::spawn_concurrent(vec![
            Reply::new(200, &search).with_gate(gate.clone()),
            Reply::new(200, &pins),
        ]);
        let client = Client::new(&server.base).unwrap();
        let (_window, view, mut visual) = panel(cx, client);
        visual.update(|window, app| {
            view.update(app, |panel, cx| {
                panel.set_allowed_channels(HashSet::from([7]), cx);
                panel.open(1, 7, Mode::Search, cx);
                panel
                    .query
                    .update(cx, |input, cx| input.set_value("query", window, cx));
            });
        });
        visual.run_until_parked();
        visual.update(|_window, app| {
            view.update(app, |panel, cx| panel.submit_for_test(cx));
        });
        assert!(gate.wait_arrived(Duration::from_secs(3)));
        visual.update(|_window, app| {
            view.update(app, |panel, cx| panel.reload_pins(cx));
        });
        wait_panel(&mut visual, &view, |panel| !panel.pins_loading());
        gate.release();
        wait_panel(&mut visual, &view, |panel| !panel.search_loading());
        view.read_with(&*visual, |panel, _| {
            assert_eq!(panel.results().len(), 1, "the search page survived pins");
            assert_eq!(panel.pins().len(), 1, "the pin page was applied separately");
        });
    }
}
