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
        let access = self.access_generation;
        let (server, channel_id) = (self.server, self.channel);
        let runtime = self.runtime.clone();
        let drafts = self.drafts.clone();
        let panel = cx.new(|cx| {
            thread_panel::ThreadPanel::new(
                client, runtime, server, channel_id, root, writable, access, drafts, window, cx,
            )
        });
        let weak = cx.entity().downgrade();
        panel.update(cx, |panel, _| {
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
        !self.in_conversation()
            && self.current_channel().is_some_and(|channel| {
                channel.writable() && (channel.kind == "text" || channel.comments_enabled)
            })
    }

    /// The root may have been edited by the main row action. The panel keeps
    /// its own snapshot and revision rule.
    pub(super) fn sync_thread_root(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.sync_open_views(cx);
    }
}
