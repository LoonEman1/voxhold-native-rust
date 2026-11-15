use super::*;

impl Voxhold {
    pub(super) fn save_to_favorites(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(message) = self.message_snapshot(id) else {
            return;
        };
        if !self.permissions_for_row(&message).save {
            return;
        }
        let Some(request) = self.begin_action(id, "save") else {
            return;
        };
        let Some(client) = self.client.clone() else {
            self.finish_action(id, "save", &request);
            return;
        };
        let kind = if self.in_conversation() {
            "conversation"
        } else {
            "channel"
        };
        let key = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime.block_on(client.save_favorite_source(kind, id, &key))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.accepts_action(id, "save", &request) {
                        return;
                    }
                    s.finish_action(id, "save", &request);
                    match result {
                        Ok(()) => {
                            s.action_error = None;
                            s.action_notice = "╨б╨╛╤Е╤А╨░╨╜╨╡╨╜╨╛ ╨▓ ╨╕╨╖╨▒╤А╨░╨╜╨╜╨╛╨╡".into();
                        }
                        Err(error) => s.action_error = Some((id, error.to_string())),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn block_author(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(message) = self.message_snapshot(id) else {
            return;
        };
        if !self.permissions_for_row(&message).block {
            return;
        }
        let user = message.author.user_id;
        let Some(request) = self.begin_action(id, "block") else {
            return;
        };
        let Some(client) = self.client.clone() else {
            self.finish_action(id, "block", &request);
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task =
            cx.background_spawn(async move { runtime.block_on(client.block_user(user, true)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.accepts_action(id, "block", &request) {
                        return;
                    }
                    s.finish_action(id, "block", &request);
                    match result {
                        Ok(()) => {
                            s.action_error = None;
                            s.action_notice =
                                "╨Я╨╛╨╗╤М╨╖╨╛╨▓╨░╤В╨╡╨╗╤М ╨╖╨░╨▒╨╗╨╛╨║╨╕╤А╨╛╨▓╨░╨╜".into();
                        }
                        Err(error) => s.action_error = Some((id, error.to_string())),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Copy the selected rows in id order, or the clicked row when nothing is
    /// selected. Empty bodies are skipped like Go `clipboardText`.
    pub(super) fn copy_messages(&mut self, id: i64, cx: &mut Context<Self>) {
        let rows: Vec<&Message> = if self.selected_messages.is_empty() {
            self.messages.iter().filter(|row| row.id == id).collect()
        } else {
            let selected: std::collections::HashSet<i64> =
                self.selected_messages.iter().copied().collect();
            self.messages
                .iter()
                .filter(|row| selected.contains(&row.id))
                .collect()
        };
        let text = message_tools::copy_text(rows);
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.action_error = None;
        self.action_notice = "╨б╨║╨╛╨┐╨╕╤А╨╛╨▓╨░╨╜╨╛".into();
        cx.notify();
    }

    pub(super) fn toggle_selected(&mut self, id: i64, additive: bool, cx: &mut Context<Self>) {
        if !additive {
            if !self.selected_messages.is_empty() {
                self.selected_messages.clear();
                cx.notify();
            }
            return;
        }
        if let Some(index) = self.selected_messages.iter().position(|row| *row == id) {
            self.selected_messages.remove(index);
        } else {
            self.selected_messages.push(id);
            self.selected_messages.sort_unstable();
        }
        cx.notify();
    }

    pub(super) fn open_forward(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(message) = self.message_snapshot(id) else {
            return;
        };
        if !self.permissions_for_row(&message).forward {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        let source = self.chat_key();
        self.forward = Some(ForwardState {
            message_id: id,
            source,
            source_kind: if self.in_conversation() {
                "conversation"
            } else {
                "channel"
            },
            request,
            destinations: Vec::new(),
            loading: true,
            sending: false,
            error: String::new(),
        });
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task =
            cx.background_spawn(async move { runtime.block_on(client.forward_destinations()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session
                        || s.forward
                            .as_ref()
                            .is_none_or(|state| state.request != request || state.source != source)
                    {
                        return;
                    }
                    if let Some(state) = s.forward.as_mut() {
                        state.loading = false;
                        match result {
                            Ok(values) => state.destinations = values,
                            Err(error) => state.error = error.to_string(),
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn forward_to(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.forward.clone() else {
            return;
        };
        let Some(destination) = state.destinations.get(index).cloned() else {
            return;
        };
        if state.sending {
            return;
        }
        // The destination list may be older than the row. Re-read the row and
        // its ACL before starting the request.
        let Some(message) = self.message_snapshot(state.message_id) else {
            self.forward = None;
            cx.notify();
            return;
        };
        if !self.permissions_for_row(&message).forward {
            if let Some(current) = self.forward.as_mut() {
                current.error = "╨Э╨╡╨┤╨╛╤Б╤В╨░╤В╨╛╤З╨╜╨╛ ╨┐╤А╨░╨▓ ╨┤╨╛╤Б╤В╤Г╨┐╨░.".into();
                current.sending = false;
            }
            cx.notify();
            return;
        }
        let Some(request) = self.begin_action(state.message_id, "forward") else {
            return;
        };
        let Some(client) = self.client.clone() else {
            self.finish_action(state.message_id, "forward", &request);
            return;
        };
        if let Some(current) = self.forward.as_mut() {
            current.sending = true;
            current.error.clear();
        }
        let message_id = state.message_id;
        let source = state.source;
        let source_kind = state.source_kind;
        let destination_id = destination.id;
        let destination_kind = destination.kind.clone();
        let destination_server = destination.server_id;
        let key = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime.block_on(client.forward_message_to(
                source_kind,
                source.channel,
                &destination_kind,
                destination_id,
                &key,
            ))
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session
                    || !s.accepts_action(message_id, "forward", &request)
                {
                    return;
                }
                s.finish_action(message_id, "forward", &request);
                match result {
                    Ok(message) => {
                        s.forward = None;
                        s.action_error = None;
                        s.action_notice = "╨Я╨╡╤А╨╡╤Б╨╗╨░╨╜╨╛".into();
                        let active = s.chat_key() == source;
                        if active
                            && !s.in_conversation()
                            && message.channel_id == s.channel
                            && destination_server == s.server
                        {
                            s.merge_message(true, message);
                            s.trim_history(false);
                        } else if active
                            && s.in_conversation()
                            && message.conversation_id == s.channel
                        {
                            s.merge_message(true, message);
                            s.trim_history(false);
                        }
                    }
                    Err(error) => {
                        if let Some(state) = s.forward.as_mut() {
                            state.sending = false;
                            state.error = error.to_string();
                        }
                        if error.status() == Some(403) {
                            s.recheck_channel_snapshot(window, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
