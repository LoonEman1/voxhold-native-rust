use super::*;

impl Voxhold {
    /// Go `openThread`: only a readable channel row can open a thread, and the
    /// panel gets its own timeline, anchor, composer and draft key.
    pub(super) fn open_thread(
        &mut self,
        root: Message,
        target: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.in_conversation() || self.access_pending {
            return;
        }
        if !self
            .current_channel()
            .is_some_and(|channel| channel.has_history())
        {
            return;
        }
        if root.thread_root_id.is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.show_search = false;
        self.message_menu = None;
        self.forward = None;
        self.editing = None;
        self.confirm_delete = None;
        self.close_thread(window, cx);
        let writable = self.thread_writable();
        let news = self
            .current_channel()
            .is_some_and(|channel| channel.kind == "news");
        let access = self.access_generation;
        let session = self.session_generation;
        let (server, channel_id) = (self.server, self.channel);
        let runtime = self.runtime.clone();
        let drafts = self.drafts.clone();
        let panel = cx.new(|cx| {
            thread_panel::ThreadPanel::new(
                client, runtime, server, channel_id, root, writable, access, drafts, window, cx,
            )
        });
        let weak = cx.entity().downgrade();
        let owner = cx.entity().downgrade();
        panel.update(cx, |panel, _| {
            panel.set_context(news, owner, session);
            panel.set_close_handler(Box::new(move |cx| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |view, cx| {
                        view.show_thread = false;
                        cx.notify();
                    });
                }
            }));
            panel.set_target(target);
        });
        let key = panel.read(cx).key();
        if let Some(pending) = self.pending_sends.get(&key) {
            let attempt = pending.attempt;
            panel.update(cx, |panel, _| panel.set_pending(attempt));
        }
        panel.update(cx, |panel, cx| panel.load_initial(cx));
        self.thread = Some(panel);
        self.show_thread = true;
        cx.notify();
    }

    pub(super) fn close_thread(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.thread.take() {
            panel.update(cx, |panel, cx| panel.save_draft(cx));
        }
        self.show_thread = false;
        cx.notify();
    }

    pub(super) fn thread_writable(&self) -> bool {
        // A news comment needs the base Publish bit, not `can_publish` (which
        // only gates creating publications). The backend checks the same pair:
        // comments enabled and the basic publish permission.
        !self.in_conversation()
            && self.current_channel().is_some_and(|channel| {
                channel.has_history()
                    && channel.permissions & 2 != 0
                    && (channel.kind == "text"
                        || (channel.kind == "news" && channel.comments_enabled))
            })
    }

    /// Start one idempotent thread POST. The attempt lives in `pending_sends`
    /// under `ChatKey::thread`, so reopening the panel sees it and a logout
    /// can abort it like a main-composer send.
    pub(crate) fn start_thread_send(
        &mut self,
        plan: draft::SendPlan,
        news: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        if let Some(pending) = self.pending_sends.get(&plan.key) {
            return Some(pending.attempt);
        }
        let client = self.client.clone()?;
        self.send_attempt = self.send_attempt.wrapping_add(1);
        let attempt = self.send_attempt;
        let session = self.session_generation;
        let access = self.access_generation;
        let (server, channel, root) = (plan.key.server, plan.key.channel, plan.key.root);
        let runtime = self.runtime.clone();
        let content = plan.content.clone();
        let send_key = plan.send_key.clone();
        let task = runtime.spawn(async move {
            client
                .send_thread_message(server, channel, root, &content, &send_key, news)
                .await
        });
        let abort = task.abort_handle();
        self.pending_sends.insert(
            plan.key,
            PendingSend {
                attempt,
                abort,
                plan: plan.clone(),
            },
        );
        cx.spawn_in(window, async move |this, window| {
            let result = match task.await {
                Ok(result) => result,
                Err(_) => Err(voxhold_api::Error::Network),
            };
            let _ = this.update_in(window, |s, window, cx| {
                s.finish_thread_send(plan, attempt, access, session, result, window, cx);
            });
        })
        .detach();
        Some(attempt)
    }

    /// Confirmation for a thread POST owned by the workspace. Only the attempt
    /// that still owns `pending_sends` may change the draft or retry key. The
    /// panel for the same key settles even after a channel switch changed the
    /// access generation; only the POST row itself is gated by that generation.
    pub(crate) fn finish_thread_send(
        &mut self,
        plan: draft::SendPlan,
        attempt: u64,
        access: u64,
        session: u64,
        result: Result<Message, voxhold_api::Error>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session_generation != session {
            return;
        }
        let owns = self
            .pending_sends
            .get(&plan.key)
            .is_some_and(|pending| pending.attempt == attempt);
        if !owns {
            return;
        }
        self.pending_sends.remove(&plan.key);
        let panel_open = self
            .thread
            .as_ref()
            .is_some_and(|panel| panel.read(cx).key() == plan.key);
        let apply_message = self.access_generation == access && !self.access_pending;
        let mut clear = false;
        if result.is_ok() {
            let composer_now = if panel_open {
                self.thread
                    .as_ref()
                    .map(|panel| panel.read(cx).composer_text(cx))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let finished = self.drafts.borrow_mut().finish_send(
                plan.key,
                plan.revision,
                &plan.composer_text,
                &[],
                panel_open.then_some(composer_now.as_str()),
            );
            clear = panel_open && finished == draft::Finish::Cleared;
        }
        if panel_open {
            if let Some(panel) = &self.thread {
                panel.update(cx, |panel, cx| {
                    panel.apply_send_result(
                        attempt,
                        plan.key,
                        &result,
                        clear,
                        apply_message,
                        window,
                        cx,
                    )
                });
            }
        }
        cx.notify();
    }

    /// The root may have been edited by the main row action. The panel keeps
    /// its own snapshot and revision rule.
    pub(super) fn sync_thread_root(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.sync_open_views(cx);
    }
}
