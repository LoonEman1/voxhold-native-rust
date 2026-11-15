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

    /// Confirmation for a thread POST owned by the workspace. The draft is
    /// finished even when the panel was closed while the request was in
    /// flight; the panel is repainted only for the same key and attempt.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_thread_send(
        &mut self,
        plan: draft::SendPlan,
        attempt: u64,
        generation: u64,
        access: u64,
        session: u64,
        result: Result<Message, voxhold_api::Error>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session_generation != session {
            return;
        }
        let active = self
            .thread
            .as_ref()
            .is_some_and(|panel| panel.read(cx).key() == plan.key);
        let mut clear = false;
        if result.is_ok() {
            let composer_now = if active {
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
                active.then_some(composer_now.as_str()),
            );
            clear = active && finished == draft::Finish::Cleared;
        }
        if active {
            if let Some(panel) = &self.thread {
                panel.update(cx, |panel, cx| {
                    panel.apply_send_result(
                        attempt, generation, access, plan.key, &result, clear, window, cx,
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
