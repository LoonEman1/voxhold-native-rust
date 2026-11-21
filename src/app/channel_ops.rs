//! Channel settings and channel notification operations.

use super::*;
use crate::settings::{self, RequestToken};
impl Voxhold {
    fn channel_token(&mut self) -> Option<RequestToken> {
        self.channel_settings
            .as_mut()
            .map(|panel| panel.dialog.begin())
    }
    fn channel_settle(&mut self, token: RequestToken) -> bool {
        self.channel_settings
            .as_mut()
            .is_some_and(|panel| panel.dialog.settle(token))
    }
    fn channel_error(&mut self, message: String) {
        if let Some(panel) = &mut self.channel_settings {
            panel.section_error = message;
        }
    }
    pub(super) fn load_channel_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = &self.channel_settings else {
            return;
        };
        let kind = panel.kind.clone();
        let (server, channel) = (panel.server, panel.channel);
        let Some(token) = self.channel_token() else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        match index {
            0 if kind == "text" => {
                let task = cx.background_spawn(async move {
                    runtime
                        .block_on(client.slowmode(server, channel))
                        .map_err(|error| error.to_string())
                });
                cx.spawn_in(window, async move |this, window| {
                    let result = task.await;
                    let _ = this.update_in(window, |s, window, cx| {
                        if s.session_generation != session || !s.channel_settle(token) {
                            return;
                        }
                        match result {
                            Ok(seconds) => {
                                if let Some(panel) = &mut s.channel_settings {
                                    panel.slow_input.update(cx, |input, cx| {
                                        input.set_value(seconds.to_string(), window, cx)
                                    });
                                }
                            }
                            Err(error) => s.channel_error(error),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            0 if kind == "voice" => {
                let task = cx.background_spawn(async move {
                    runtime
                        .block_on(client.voice_encryption(server, channel))
                        .map_err(|error| error.to_string())
                });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |s, cx| {
                            if s.session_generation != session || !s.channel_settle(token) {
                                return;
                            }
                            match result {
                                Ok(required) => {
                                    if let Some(panel) = &mut s.channel_settings {
                                        panel.encryption = required;
                                    }
                                }
                                Err(error) => s.channel_error(error),
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            1 => {
                let task = cx.background_spawn(async move {
                    runtime.block_on(async move {
                        client
                            .channel_access(server, channel)
                            .await
                            .map_err(|error| error.to_string())
                    })
                });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |s, cx| {
                            if s.session_generation != session || !s.channel_settle(token) {
                                return;
                            }
                            match result {
                                Ok(access) => {
                                    if let Some(panel) = &mut s.channel_settings {
                                        panel.access = Some(access);
                                        panel.apply_access();
                                    }
                                }
                                Err(error) => s.channel_error(error),
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            2 if kind == "news" => {
                let task = cx.background_spawn(async move {
                    runtime
                        .block_on(client.news_settings(server, channel))
                        .map_err(|error| error.to_string())
                });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |s, cx| {
                            if s.session_generation != session || !s.channel_settle(token) {
                                return;
                            }
                            match result {
                                Ok(news) => {
                                    if let Some(panel) = &mut s.channel_settings {
                                        panel.news = Some(news);
                                        panel.apply_news();
                                    }
                                }
                                Err(error) => s.channel_error(error),
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            _ => {}
        }
        cx.notify();
    }
    pub(super) fn can_manage_channel_now(&self) -> bool {
        self.servers
            .iter()
            .find(|server| server.id == self.server)
            .is_some_and(|server| crate::settings::Role::parse(&server.role).can_manage_channels())
    }

    pub(super) fn channel_action(
        &mut self,
        action: channel_settings::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use channel_settings::Action;
        if !self.can_manage_channel_now() {
            self.close_settings(false, window, cx);
            return;
        }
        match action {
            Action::Retry(tab) => self.load_channel_tab(tab, window, cx),
            Action::SaveName => self.save_channel_name(cx),
            Action::SaveSlow => self.save_slowmode(cx),
            Action::ToggleRestricted => {
                if let Some(panel) = &mut self.channel_settings {
                    panel.restricted = !panel.restricted;
                }
                cx.notify();
            }
            Action::ToggleMemberRole => {
                if let Some(panel) = &mut self.channel_settings {
                    panel.member_role = !panel.member_role;
                }
                cx.notify();
            }
            Action::ToggleUser(id) => {
                if let Some(panel) = &mut self.channel_settings {
                    if !panel.user_ids.remove(&id) {
                        panel.user_ids.insert(id);
                    }
                }
                cx.notify();
            }
            Action::SaveAccess => self.save_channel_access(cx),
            Action::ToggleComments => {
                if let Some(panel) = &mut self.channel_settings {
                    panel.comments = !panel.comments;
                }
                cx.notify();
            }
            Action::TogglePublisher(id) => {
                if let Some(panel) = &mut self.channel_settings {
                    if !panel.publishers.remove(&id) {
                        panel.publishers.insert(id);
                    }
                }
                cx.notify();
            }
            Action::SaveNews => self.save_news(cx),
            Action::ToggleEncryption => self.toggle_encryption(cx),
            Action::Delete => {
                if let Some(panel) = &mut self.channel_settings {
                    panel.confirm_delete = true;
                }
                cx.notify();
            }
            Action::CancelDelete => {
                if let Some(panel) = &mut self.channel_settings {
                    panel.confirm_delete = false;
                }
                cx.notify();
            }
            Action::ConfirmDelete => self.delete_channel(window, cx),
        }
    }
    fn save_channel_name(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.channel_settings else {
            return;
        };
        let name = panel.normalized_name(cx);
        if name.is_empty() {
            return;
        }
        let (server, channel) = (panel.server, panel.channel);
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.channel_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let patch_name = name.clone();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.edit_channel(server, channel, &patch_name))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.channel_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.channel_settings {
                                panel.name = name.clone();
                            }
                            if let Some(row) = s.channels.iter_mut().find(|row| row.id == channel) {
                                row.name = name;
                            }
                        }
                        Err(error) => s.channel_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn save_slowmode(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.channel_settings else {
            return;
        };
        let value = panel.slow_input.read(cx).value().to_string();
        let seconds = match settings::validate_slowmode(&value) {
            Ok(seconds) => seconds,
            Err(message) => {
                self.channel_error(message.into());
                cx.notify();
                return;
            }
        };
        let (server, channel) = (panel.server, panel.channel);
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.channel_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_slowmode(server, channel, seconds))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.channel_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.channel_settings {
                                panel.section_error.clear();
                            }
                        }
                        Err(error) => s.channel_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn save_channel_access(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.channel_settings else {
            return;
        };
        // The form is only editable after its own GET answered; this also
        // keeps a stale editor from posting defaults to a newer scope.
        if panel.access.is_none() {
            return;
        }
        let (server, channel) = (panel.server, panel.channel);
        let access = panel.access_value();
        let saved_access = access.clone();
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.channel_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_channel_access(server, channel, &access))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.channel_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.channel_settings {
                                panel.access = Some(saved_access.clone());
                                panel.section_error.clear();
                            }
                        }
                        Err(error) => s.channel_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn save_news(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.channel_settings else {
            return;
        };
        let (server, channel) = (panel.server, panel.channel);
        let news = panel.news_value();
        let saved_news = news.clone();
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.channel_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_news_settings(server, channel, &news))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.channel_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.channel_settings {
                                panel.news = Some(saved_news.clone());
                                panel.section_error.clear();
                            }
                        }
                        Err(error) => s.channel_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn toggle_encryption(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.channel_settings else {
            return;
        };
        let (server, channel) = (panel.server, panel.channel);
        let required = !panel.encryption;
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.channel_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_voice_encryption(server, channel, required))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.channel_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.channel_settings {
                                panel.encryption = required;
                                panel.section_error.clear();
                            }
                        }
                        Err(error) => s.channel_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn delete_channel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.channel_settings else {
            return;
        };
        let (server, channel) = (panel.server, panel.channel);
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.channel_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.delete_channel(server, channel))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session {
                    return;
                }
                let current = s
                    .channel_settings
                    .as_mut()
                    .is_some_and(|panel| panel.dialog.settle(token));
                if !current {
                    return;
                }
                match result {
                    Ok(()) => {
                        s.close_settings(false, window, cx);
                        s.channels.retain(|row| row.id != channel);
                        if s.channel == channel {
                            let next = s.channels.first().map(|row| row.id).unwrap_or(0);
                            s.select_channel(next, window, cx);
                        }
                    }
                    Err(error) => s.channel_error(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ---- channel notifications -------------------------------------------

    fn notifications_token(&mut self) -> Option<RequestToken> {
        self.channel_notifications
            .as_mut()
            .map(|panel| panel.dialog.begin())
    }
    fn notifications_settle(&mut self, token: RequestToken) -> bool {
        self.channel_notifications
            .as_mut()
            .is_some_and(|panel| panel.dialog.settle(token))
    }
    pub(super) fn load_channel_notifications(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.channel_notifications else {
            return;
        };
        let (server, channel) = (panel.server, panel.channel);
        let epoch = panel.epoch;
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.notifications_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.channel_notifications(server, channel))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.notifications_settle(token) {
                        return;
                    }
                    if let Some(panel) = &mut s.channel_notifications {
                        // A realtime setting arrived after this GET started.
                        // Its value is newer than the snapshot on the wire.
                        if panel.epoch == epoch {
                            match result {
                                Ok(mode) => {
                                    panel.mode = mode.clone();
                                    panel.saved = mode;
                                    panel.loaded = true;
                                    panel.error.clear();
                                }
                                Err(error) => panel.error = error,
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    pub(super) fn notifications_action(
        &mut self,
        action: channel_settings::NotifAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use channel_settings::NotifAction;
        match action {
            NotifAction::Retry => self.load_channel_notifications(cx),
            NotifAction::Choose(index) => {
                let key = self
                    .channel_notifications
                    .as_ref()
                    .map(|panel| (panel.server, panel.channel));
                if key.is_some_and(|key| self.notification_saves.contains_key(&key)) {
                    // The PUT owns the confirmed value until it answers; a
                    // local change now could be overwritten by that answer.
                    return;
                }
                if let Some(panel) = &mut self.channel_notifications {
                    if let Some(mode) = settings::NOTIFICATION_MODES.get(index) {
                        panel.mode = (*mode).to_owned();
                    }
                }
                cx.notify();
            }
            NotifAction::ToggleNotifications => {
                let enabled = !self.notifications.enabled;
                let changed = self.notifications.set_enabled(enabled);
                if changed && !enabled {
                    self.notification_tray
                        .clear(super::personal_ops::hwnd(window));
                }
                if !changed {
                    if let Some(panel) = &mut self.channel_notifications {
                        panel.error = self.notifications.error.clone();
                    }
                }
                cx.notify();
            }
            NotifAction::Save => self.save_channel_notifications(window, cx),
        }
    }
    pub(super) fn save_channel_notifications(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = &self.channel_notifications else {
            return;
        };
        let (server, channel, mode, saved) = (
            panel.server,
            panel.channel,
            panel.mode.clone(),
            panel.saved.clone(),
        );
        if mode == saved || self.notification_saves.contains_key(&(server, channel)) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.notifications_token() else {
            return;
        };
        let request = self
            .channel_notifications
            .as_mut()
            .map(|panel| {
                panel.error.clear();
                panel.save_request = panel.save_request.wrapping_add(1);
                panel.save_request
            })
            .unwrap_or_default();
        // Session-level lock for this exact channel. A reopened dialog must
        // not be able to send a second PUT while this one is pending.
        self.notification_saves.insert((server, channel), request);
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let request_mode = mode.clone();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_channel_notifications(server, channel, &request_mode))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session {
                    return;
                }
                if s.notification_saves.get(&(server, channel)) == Some(&request) {
                    s.notification_saves.remove(&(server, channel));
                }
                let accepted = s.notifications_settle(token);
                if let Some(panel) = &mut s.channel_notifications {
                    if !accepted {
                        cx.notify();
                        return;
                    }
                    match result {
                        Ok(()) => {
                            panel.saved = mode.clone();
                        }
                        Err(error) => {
                            panel.error = error;
                            cx.notify();
                            return;
                        }
                    }
                }
                if accepted {
                    s.close_settings(true, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
