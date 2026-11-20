use super::*;
use crate::messages::{self, Creator};

impl Voxhold {
    pub(super) fn open_conversation_creator(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.client.is_none() {
            return;
        }
        if !self.personal_mode {
            self.enter_personal_mode(window, cx);
        }
        if self.messaging.creator.is_some() {
            return;
        }
        if self.conversation.is_some() {
            self.clear_active_conversation(None, window, cx);
        }
        self.messaging.clear_search();
        self.messaging.clear_leave();
        let creator = Creator::new(window, cx);
        let name = creator.name.clone();
        let search = creator.search.clone();
        self.messaging.creator = Some(creator);
        self._creator_subscriptions.clear();
        self._creator_subscriptions.push(cx.subscribe_in(
            &name,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.submit_conversation_creator(window, cx),
                _ => {}
            },
        ));
        self._creator_subscriptions
            .push(cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()));
        name.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(super) fn close_conversation_creator(&mut self, cx: &mut Context<Self>) {
        self.messaging.creator = None;
        self._creator_subscriptions.clear();
        cx.notify();
    }

    pub(super) fn toggle_creator_member(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(creator) = self.messaging.creator.as_mut() else {
            return;
        };
        if creator.busy {
            return;
        }
        if !messages::toggle_selected(&mut creator.selected, id) {
            creator.error = format!(
                "Можно добавить не более {} участников.",
                voxhold_api::MAX_USERS
            );
        } else {
            creator.error.clear();
        }
        cx.notify();
    }

    pub(super) fn submit_conversation_creator(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(creator) = self.messaging.creator.as_ref() else {
            return;
        };
        if creator.busy {
            return;
        }
        let ids = creator.selected.clone();
        let typed = creator.name.read(cx).value().trim().to_string();
        // A direct pair follows the web contract: no custom group name.
        let name = if ids.len() > 1 { typed } else { String::new() };
        if let Err(error) = messages::validate_create(&ids, &name) {
            if let Some(creator) = self.messaging.creator.as_mut() {
                creator.error = error;
            }
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        // The creator entity is replaced on close/reopen, so a local counter
        // restarts at 1 and an older answer could match the new form. The
        // workspace-wide action sequence (used by leave/search) never repeats.
        self.action_seq = self.action_seq.wrapping_add(1);
        let request = self.action_seq;
        if let Some(creator) = self.messaging.creator.as_mut() {
            creator.busy = true;
            creator.request = request;
            creator.error.clear();
        }
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(async {
                let created = client.create_conversation(&ids, &name).await?;
                if created.members.is_empty() {
                    client.conversation(created.id).await
                } else {
                    Ok(created)
                }
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                let active = s
                    .messaging
                    .creator
                    .as_ref()
                    .is_some_and(|creator| creator.request == request);
                if !active || s.session_generation != session {
                    return;
                }
                match result {
                    Ok(row) => {
                        s.conversations.retain(|current| current.id != row.id);
                        s.conversations.insert(0, row.clone());
                        s.messaging.creator = None;
                        s._creator_subscriptions.clear();
                        s.select_conversation(row, window, cx);
                    }
                    Err(error) => {
                        if let Some(creator) = s.messaging.creator.as_mut() {
                            creator.busy = false;
                            creator.error = error.to_string();
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}
