use super::*;

impl Voxhold {
    pub(super) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.client.clone() else { return };
        if self.busy
            || self.access_pending
            || !self
                .channels
                .iter()
                .any(|ch| ch.id == self.channel && ch.writable())
        {
            return;
        }
        let text = self.composer.read(cx).value().to_string();
        let has_files = self
            .attachments
            .as_ref()
            .is_some_and(|a| a.read(cx).has_files());
        if (text.trim().is_empty() && !has_files) || text.chars().count() > 5120 {
            return;
        }
        if has_files {
            if let Some(attachments) = &self.attachments {
                if attachments.read(cx).is_busy() {
                    return;
                }
                self.attachment_pending = Some((self.access_generation, text.clone()));
                attachments.update(cx, |a, cx| a.send(&text, 0, cx));
            }
            return;
        }
        let sid = self.server;
        let cid = self.channel;
        let access = self.access_generation;
        self.busy = true;
        cx.notify();
        let sent_text = text.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(c.send_message(sid, cid, &text))
                .map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.access_generation != access || s.access_pending {
                    return;
                }
                s.busy = false;
                match result {
                    Ok(message)
                        if s.server == sid
                            && s.channel == cid
                            && !s.deleted_ids.contains(&message.id) =>
                    {
                        if s.composer.read(cx).value().as_ref() == sent_text {
                            s.composer
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        s.merge_message(true, message);
                        s.trim_history(false);
                    }
                    Ok(_) => {}
                    Err(e) => s.error = e,
                }
                cx.notify();
            });
        })
        .detach();
    }
}
