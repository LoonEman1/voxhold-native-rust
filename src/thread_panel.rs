//! Thread timeline and its own composer. Ported from Go `thread.go`: a comment
//! never enters the main channel window, the root has its own read cursor, and
//! the panel owns its draft under `ChatKey::thread`.

use gpui::{div, prelude::*, px, Context, Entity, Render, ScrollHandle, Window};
use gpui_component::input::{InputEvent, InputState, TextInput};
use std::sync::Arc;
use voxhold_api::{Client, Message};

use crate::draft::{ChatKey, Shared as SharedDrafts};
use crate::history;

gpui::actions!(voxhold, [ThreadSend]);

pub struct ThreadPanel {
    client: Client,
    runtime: Arc<tokio::runtime::Runtime>,
    server: i64,
    channel: i64,
    root: Message,
    key: ChatKey,
    writable: bool,
    access: u64,
    generation: u64,
    request: u64,
    messages: Vec<Message>,
    before: Option<i64>,
    has_more: bool,
    loading: bool,
    error: String,
    initial: bool,
    anchored: bool,
    target: Option<i64>,
    scroll_to: Option<i64>,
    composer: Entity<InputState>,
    drafts: SharedDrafts,
    sending: bool,
    send_error: String,
    focus: bool,
    read: i64,
    read_wanted: i64,
    read_pending: bool,
    read_retry_at: Option<std::time::Instant>,
    scroll: ScrollHandle,
    on_close: Option<Box<dyn Fn(&mut gpui::App) + 'static>>,
    _subscription: gpui::Subscription,
}

impl ThreadPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: Client,
        runtime: Arc<tokio::runtime::Runtime>,
        server: i64,
        channel: i64,
        root: Message,
        writable: bool,
        access: u64,
        drafts: SharedDrafts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = ChatKey::thread(server, channel, root.id);
        let text = drafts.borrow().text(key);
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line()
                .placeholder("Ответить в обсуждении")
        });
        composer.update(cx, |input, cx| {
            input.set_value(text, window, cx);
        });
        let subscription = cx.subscribe(&composer, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = this.composer.read(cx).value().to_string();
                this.drafts.borrow_mut().set_text(this.key, &text);
                this.send_error.clear();
                cx.notify();
            }
        });
        Self {
            client,
            runtime,
            server,
            channel,
            root,
            key,
            writable,
            access,
            generation: 0,
            request: 0,
            messages: Vec::new(),
            before: None,
            has_more: false,
            loading: false,
            error: String::new(),
            initial: false,
            anchored: false,
            target: None,
            scroll_to: None,
            composer,
            drafts,
            sending: false,
            send_error: String::new(),
            focus: true,
            read: 0,
            read_wanted: 0,
            read_pending: false,
            read_retry_at: None,
            scroll: ScrollHandle::new(),
            on_close: None,
            _subscription: subscription,
        }
    }

    pub fn set_close_handler(&mut self, handler: Box<dyn Fn(&mut gpui::App) + 'static>) {
        self.on_close = Some(handler);
    }

    pub fn root_id(&self) -> i64 {
        self.root.id
    }

    #[cfg(test)]
    pub fn root(&self) -> &Message {
        &self.root
    }

    #[cfg(test)]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn load_initial(&mut self, cx: &mut Context<Self>) {
        self.load(0, cx);
    }

    pub fn set_target(&mut self, target: i64) {
        if target > 0 {
            self.target = Some(target);
            self.focus = false;
        }
    }

    fn accepts(&self, request: u64, generation: u64, access: u64) -> bool {
        self.request == request && self.generation == generation && self.access == access
    }

    fn load(&mut self, before: i64, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error.clear();
        self.request = self.request.wrapping_add(1);
        let request = self.request;
        let (server, channel, root) = (self.server, self.channel, self.root.id);
        let target = self.target.filter(|_| before == 0);
        let generation = self.generation;
        let access = self.access;
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(async move {
                let page = client
                    .thread_messages(server, channel, root, before)
                    .await?;
                if before == 0 {
                    if let (Some(target), Some(first)) = (target, page.messages.first()) {
                        if first.id > target {
                            return client
                                .thread_messages(server, channel, root, target + 1)
                                .await
                                .map(|page| (page, true));
                        }
                    }
                }
                Ok::<_, voxhold_api::Error>((page, false))
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if !s.accepts(request, generation, access) {
                        return;
                    }
                    s.loading = false;
                    match result {
                        Ok((page, anchored)) => {
                            s.apply_page(page, before, target, anchored);
                            s.mark_visible(s.messages.last().map_or(0, |message| message.id), cx);
                        }
                        Err(error) => s.error = error.to_string(),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn apply_page(
        &mut self,
        page: voxhold_api::MessagePage,
        before: i64,
        target: Option<i64>,
        anchored: bool,
    ) {
        if before > 0 {
            for row in page.messages {
                self.apply_change(row);
            }
        } else if target.is_none() && self.initial && !self.anchored && !page.messages.is_empty() {
            let first = page.messages.first().map_or(0, |message| message.id);
            self.messages.retain(|message| message.id < first);
            for row in page.messages {
                self.apply_change(row);
            }
        } else {
            self.messages.clear();
            for row in page.messages {
                self.apply_change(row);
            }
        }
        if before == 0 {
            self.anchored = anchored;
            self.target = None;
            if let Some(target) = target.filter(|id| self.messages.iter().any(|row| row.id == *id))
            {
                self.scroll_to = Some(target);
            }
        }
        self.initial = true;
        self.before = page.pagination.next_before_id;
        self.has_more = page.pagination.has_more;
    }

    fn apply_change(&mut self, message: Message) {
        if message.id <= 0 {
            return;
        }
        if let Some(existing) = self.messages.iter_mut().find(|row| row.id == message.id) {
            if existing.revision > message.revision {
                return;
            }
            history::merge_revision(existing, message);
            return;
        }
        let index = self
            .messages
            .binary_search_by_key(&message.id, |row| row.id)
            .unwrap_or_else(|index| index);
        self.messages.insert(index, message);
    }

    pub fn remove_message(&mut self, id: i64, cx: &mut Context<Self>) {
        self.messages.retain(|message| message.id != id);
        cx.notify();
    }

    pub fn sync_from_history(&mut self, messages: &[Message], cx: &mut Context<Self>) {
        if let Some(root) = messages.iter().find(|message| message.id == self.root.id) {
            if root.revision >= self.root.revision {
                self.root = root.clone();
                cx.notify();
            }
        }
    }

    pub fn handle_event(
        &mut self,
        kind: &str,
        data: &serde_json::Value,
        access: u64,
        cx: &mut Context<Self>,
    ) {
        if access != self.access {
            return;
        }
        match kind {
            "channel.subscribed" | "channel.sync_required" => {
                if data["channel_id"].as_i64() == Some(self.channel) {
                    self.refresh(cx);
                }
            }
            "message.created" | "message.updated" => {
                let Ok(message) = serde_json::from_value::<Message>(data.clone()) else {
                    return;
                };
                if message.channel_id != self.channel {
                    return;
                }
                if message.id == self.root.id {
                    if message.revision >= self.root.revision {
                        self.root = message;
                        cx.notify();
                    }
                    return;
                }
                if message.thread_root_id != Some(self.root.id) {
                    return;
                }
                self.apply_change(message);
                cx.notify();
            }
            "message.deleted" => {
                if data["channel_id"].as_i64() != Some(self.channel) {
                    return;
                }
                let Some(id) = data["message_id"].as_i64() else {
                    return;
                };
                if id == self.root.id {
                    self.close(cx);
                } else {
                    self.messages.retain(|message| message.id != id);
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        if !self.anchored {
            self.load(0, cx);
        }
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.save_draft(cx);
        if let Some(handler) = &self.on_close {
            handler(cx);
        }
    }

    /// Persist the thread draft without notifying the parent. The parent uses
    /// this while it already owns a mutable update of the workspace.
    pub fn save_draft(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        self.drafts.borrow_mut().set_text(self.key, &text);
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending || !self.writable {
            return;
        }
        let text = self.composer.read(cx).value().to_string();
        let plan = match self.drafts.borrow_mut().begin_send(self.key, &text) {
            Ok(plan) => plan,
            Err(error) => {
                self.send_error = error;
                cx.notify();
                return;
            }
        };
        let client = self.client.clone();
        let (server, channel, root) = (self.server, self.channel, self.root.id);
        let runtime = self.runtime.clone();
        let request = self.request;
        let generation = self.generation;
        let access = self.access;
        self.sending = true;
        self.send_error.clear();
        let sent_text = plan.composer_text.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(client.send_thread_message(
                server,
                channel,
                root,
                &plan.content,
                &plan.send_key,
            ))
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if !s.accepts(request, generation, access) {
                    return;
                }
                s.sending = false;
                match result {
                    Ok(message) => {
                        let now = s.composer.read(cx).value().to_string();
                        s.drafts.borrow_mut().finish_send(
                            plan.key,
                            plan.revision,
                            &plan.composer_text,
                            &[],
                            Some(&now),
                        );
                        if now == sent_text {
                            s.composer
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        s.apply_change(message);
                        s.mark_visible(s.messages.last().map_or(0, |row| row.id), cx);
                    }
                    Err(error) => s.send_error = error.to_string(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn mark_visible(&mut self, id: i64, cx: &mut Context<Self>) {
        if id <= 0 {
            return;
        }
        self.read_wanted = self.read_wanted.max(id);
        if self.read_pending
            || self.read_wanted <= self.read
            || self
                .read_retry_at
                .is_some_and(|at| at > std::time::Instant::now())
        {
            return;
        }
        let client = self.client.clone();
        let (channel, root) = (self.channel, self.root.id);
        let id = self.read_wanted;
        let access = self.access;
        let generation = self.generation;
        let runtime = self.runtime.clone();
        self.read_pending = true;
        let task = cx.background_spawn(async move {
            runtime.block_on(client.mark_thread_read(channel, root, id))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if access != s.access || generation != s.generation {
                        return;
                    }
                    s.read_pending = false;
                    match result {
                        Ok(()) => {
                            s.read = id;
                            if s.read_wanted > s.read {
                                s.mark_visible(s.read_wanted, cx);
                            }
                        }
                        Err(_) => {
                            s.read_retry_at =
                                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn jump_inside(&mut self, id: i64, cx: &mut Context<Self>) {
        if id == self.root.id {
            self.close(cx);
            return;
        }
        if self.messages.iter().any(|message| message.id == id) {
            self.scroll_to = Some(id);
            cx.notify();
        } else if !self.loading {
            self.target = Some(id);
            self.load(0, cx);
        }
    }
}

impl Render for ThreadPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::theme::dark_palette();
        let root = self.root.clone();
        let summary = root
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut panel = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(crate::ink(p.bg))
            .text_color(crate::ink(p.fg))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(18.))
                    .pt(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("Обсуждение"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(crate::ink(p.muted))
                                    .truncate()
                                    .child(format!("{} · {}", root.author.username, summary)),
                            ),
                    )
                    .child(
                        div()
                            .id("thread-close")
                            .cursor_pointer()
                            .text_size(px(12.))
                            .on_click(cx.listener(|s, _, _, cx| s.close(cx)))
                            .child("Закрыть"),
                    ),
            );
        if self.before.is_some() || self.anchored || !self.error.is_empty() {
            let mut bar = div().flex().items_center().gap_2().px(px(18.)).py(px(6.));
            if self.has_more {
                bar = bar.child(
                    div()
                        .id("thread-older")
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.muted))
                        .on_click(cx.listener(|s, _, _, cx| {
                            if let Some(before) = s.before {
                                s.load(before, cx);
                            }
                        }))
                        .child(if self.loading {
                            "Загрузка…"
                        } else {
                            "Предыдущие ответы"
                        }),
                );
            }
            if self.anchored {
                bar = bar.child(
                    div()
                        .id("thread-latest")
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.lime))
                        .on_click(cx.listener(|s, _, _, cx| s.load(0, cx)))
                        .child("К последним"),
                );
            }
            panel = panel.child(bar);
        }
        if !self.error.is_empty() {
            panel = panel.child(
                div()
                    .px(px(18.))
                    .text_size(px(12.))
                    .text_color(crate::ink(p.danger))
                    .child(self.error.clone()),
            );
        }
        let mut list = div()
            .id("thread-history")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap_2()
            .px(px(18.))
            .py(px(8.));
        if self.messages.is_empty() && !self.loading && self.error.is_empty() {
            list = list.child(
                div()
                    .text_size(px(13.))
                    .text_color(crate::ink(p.muted))
                    .child("Ответов пока нет"),
            );
        }
        for message in &self.messages {
            let id = message.id;
            let own = message.author.user_id > 0;
            let quote = history::quote_labels(message);
            let quote_target = crate::message_tools::quote_target(message);
            let mut row = div()
                .id(("thread-row", id as u64))
                .py(px(4.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(message.author.username.clone())
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::NORMAL)
                                .text_color(crate::ink(p.muted))
                                .child(history::format_time(message.created_at)),
                        ),
                );
            if let Some(target) = quote_target {
                row = row.child(
                    div()
                        .id(("thread-quote", id as u64))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.muted))
                        .on_click(cx.listener(move |s, _, _, cx| s.jump_inside(target, cx)))
                        .child(format!("{}: {}", quote.0, quote.1)),
                );
            }
            row = row.child(
                div()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .child(message.content.clone()),
            );
            for file in &message.attachments {
                if file.deleted {
                    continue;
                }
                row = row.child(
                    div()
                        .text_size(px(11.))
                        .text_color(crate::ink(p.muted))
                        .child(format!("📎 {}", file.filename)),
                );
            }
            if own {
                row = row.child(
                    div()
                        .text_size(px(11.))
                        .text_color(crate::ink(p.muted))
                        .child(format!("#{}", message.id)),
                );
            }
            list = list.child(row);
        }
        if let Some(id) = self.scroll_to.take() {
            if let Some(index) = self.messages.iter().position(|message| message.id == id) {
                self.scroll.scroll_to_item(index);
            }
        }
        panel = panel.child(list);
        if self.writable {
            let mut composer = div()
                .px(px(18.))
                .py(px(10.))
                .flex()
                .items_end()
                .gap_2()
                .key_context("ThreadComposer")
                .on_action(cx.listener(|s, _: &ThreadSend, window, cx| s.send(window, cx)));
            if !self.send_error.is_empty() {
                composer = composer.child(
                    div()
                        .text_size(px(12.))
                        .text_color(crate::ink(p.danger))
                        .child(self.send_error.clone()),
                );
            }
            composer = composer
                .child(
                    div()
                        .flex_1()
                        .min_h(px(44.))
                        .child(TextInput::new(&self.composer)),
                )
                .child(
                    div()
                        .id("thread-send")
                        .w(px(96.))
                        .h(px(38.))
                        .rounded(px(10.))
                        .bg(crate::ink(p.lime))
                        .text_color(crate::ink(0x10100f))
                        .font_weight(gpui::FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, window, cx| s.send(window, cx)))
                        .child(if self.sending {
                            "Отправка…"
                        } else {
                            "Ответить"
                        }),
                );
            panel = panel.child(composer);
            if self.focus {
                self.focus = false;
                self.composer
                    .update(cx, |input, cx| input.focus(window, cx));
            }
        } else {
            panel = panel.child(
                div()
                    .px(px(18.))
                    .py(px(8.))
                    .text_size(px(13.))
                    .text_color(crate::ink(p.muted))
                    .child("Обсуждение доступно для чтения"),
            );
        }
        panel.into_any_element()
    }
}
