//! Settings dialog lifecycle and personal settings operations.

use super::*;
use crate::settings::{self, RequestToken};

/// Native window handle used by the tray notification adapter.
pub(super) fn hwnd(window: &Window) -> isize {
    #[cfg(windows)]
    {
        use raw_window_handle::RawWindowHandle;
        if let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                return handle.hwnd.get();
            }
        }
    }
    let _ = window;
    0
}

impl Voxhold {
    pub(super) fn close_settings(
        &mut self,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = self.active_settings.take() else {
            return;
        };
        match kind {
            SettingsKind::Personal => {
                if let Some(personal) = &mut self.personal {
                    personal.dialog.close();
                    personal.crop = None;
                }
            }
            SettingsKind::Server => {
                if let Some(panel) = &mut self.server_settings {
                    panel.dialog.close();
                }
            }
            SettingsKind::Channel => {
                if let Some(panel) = &mut self.channel_settings {
                    panel.dialog.close();
                }
            }
            SettingsKind::ChannelNotifications => {
                if let Some(panel) = &mut self.channel_notifications {
                    panel.dialog.close();
                }
            }
        }
        self.settings_ring.clear();
        if restore {
            if let Some(focus) = self.settings_return_focus.take() {
                window.focus(&focus);
            }
        } else {
            self.settings_return_focus = None;
        }
        cx.notify();
    }

    /// Close a dialog whose resource disappeared or whose scope no longer
    /// matches the selected server/channel.
    pub(super) fn close_stale_settings(
        &mut self,
        server: i64,
        channel: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stale = match self.active_settings {
            Some(SettingsKind::Server) => self
                .server_settings
                .as_ref()
                .is_some_and(|panel| panel.dialog.stale(server, channel)),
            Some(SettingsKind::Channel) => self
                .channel_settings
                .as_ref()
                .is_some_and(|panel| panel.dialog.stale(server, channel)),
            Some(SettingsKind::ChannelNotifications) => {
                self.channel_notifications.as_ref().is_some_and(|panel| {
                    panel.dialog.stale(server, channel)
                        || !self
                            .channels
                            .iter()
                            .any(|row| row.id == panel.channel && row.has_history())
                })
            }
            _ => false,
        };
        if stale {
            self.close_settings(false, window, cx);
        }
    }

    pub(super) fn open_settings(
        &mut self,
        kind: SettingsKind,
        entry: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_settings == Some(kind) {
            return;
        }
        if self.active_settings.is_some() {
            self.close_settings(false, window, cx);
        }
        self.settings_return_focus = Some(entry);
        self.active_settings = Some(kind);
        self.settings_ring.clear();
        match kind {
            SettingsKind::Personal => {
                let self_member = self
                    .members
                    .iter()
                    .find(|member| member.user_id == self.self_user_id)
                    .cloned();
                let version = self_member.and_then(|member| member.avatar_version);
                if let Some(panel) = &mut self.personal {
                    panel.reopen(self.theme_mode, self.notifications.enabled);
                    panel.set_avatar_version(version);
                }
                self.load_personal_profile(window, cx);
                self.load_personal_sessions(cx);
                self.load_personal_avatar(window, cx);
            }
            SettingsKind::Server => {
                let Some(server) = self
                    .servers
                    .iter()
                    .find(|server| server.id == self.server)
                    .cloned()
                else {
                    self.active_settings = None;
                    return;
                };
                let members = self.members.clone();
                let channels = self.channels.clone();
                if let Some(panel) = &mut self.server_settings {
                    panel.reopen(server.id, &server.name, window, cx);
                    panel.set_members(&members, &channels);
                }
                self.load_server_general(window, cx);
            }
            SettingsKind::Channel => {
                let Some(channel) = self
                    .channels
                    .iter()
                    .find(|channel| channel.id == self.channel)
                    .cloned()
                else {
                    self.active_settings = None;
                    return;
                };
                let allowed = self
                    .servers
                    .iter()
                    .find(|server| server.id == self.server)
                    .is_some_and(|server| {
                        settings::Role::parse(&server.role).can_manage_channels()
                    });
                let members = self.members.clone();
                if let Some(panel) = &mut self.channel_settings {
                    panel.reopen(
                        self.server,
                        channel.id,
                        &channel.kind,
                        &channel.name,
                        allowed,
                        window,
                        cx,
                    );
                    panel.set_members(&members);
                }
                self.load_channel_tab(0, window, cx);
            }
            SettingsKind::ChannelNotifications => {
                let Some(channel) = self
                    .channels
                    .iter()
                    .find(|channel| channel.id == self.channel && channel.has_history())
                    .cloned()
                else {
                    self.active_settings = None;
                    return;
                };
                if let Some(panel) = &mut self.channel_notifications {
                    panel.reopen(self.server, channel.id, &channel.name);
                }
                self.load_channel_notifications(cx);
            }
        }
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            if let Some(view) = weak.upgrade() {
                view.update(cx, |s, cx| {
                    if let Some((focus, _)) = s.settings_ring.first().cloned() {
                        window.focus(&focus);
                    }
                    cx.notify();
                });
            }
        });
        cx.notify();
    }

    pub(super) fn activate_settings_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.active_settings {
            Some(SettingsKind::Personal) => {
                let previous = self.personal.as_ref().map(|panel| panel.tab());
                let first = self
                    .personal
                    .as_mut()
                    .is_some_and(|panel| panel.dialog.activate(index));
                if previous == Some(0) && index != 0 {
                    if let Some(panel) = &mut self.personal {
                        panel.cancel_profile_loads();
                    }
                }
                if first {
                    match index {
                        0 => {
                            self.load_personal_profile(window, cx);
                            self.load_personal_sessions(cx);
                            self.load_personal_avatar(window, cx);
                        }
                        1 => self.load_voice_devices(cx),
                        2 => self.load_personal_privacy(cx),
                        _ => {}
                    }
                }
            }
            Some(SettingsKind::Server) => {
                let first = self
                    .server_settings
                    .as_mut()
                    .is_some_and(|panel| panel.dialog.activate(index));
                if first {
                    match index {
                        0 => self.load_server_general(window, cx),
                        1 => self.load_server_roles(cx),
                        2 => self.load_server_storage(window, cx),
                        3 => self.load_server_bans(cx),
                        _ => {}
                    }
                }
            }
            Some(SettingsKind::Channel) => {
                let first = self
                    .channel_settings
                    .as_mut()
                    .is_some_and(|panel| panel.dialog.activate(index));
                if first {
                    self.load_channel_tab(index, window, cx);
                }
            }
            _ => {}
        }
        self.settings_scroll.set_offset(gpui::point(px(0.), px(0.)));
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            if let Some(view) = weak.upgrade() {
                view.update(cx, |s, cx| {
                    if let Some((focus, _)) = s.settings_ring.first().cloned() {
                        window.focus(&focus);
                    }
                    cx.notify();
                });
            }
        });
        cx.notify();
    }

    pub(super) fn settings_action(
        &mut self,
        action: SettingsAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            SettingsAct::Close => self.close_settings(true, window, cx),
            SettingsAct::Tab(index) => self.activate_settings_tab(index, window, cx),
            SettingsAct::Personal(action) => self.personal_action(action, window, cx),
            SettingsAct::Server(action) => self.server_action(action, window, cx),
            SettingsAct::Channel(action) => self.channel_action(action, window, cx),
            SettingsAct::Notifications(action) => self.notifications_action(action, window, cx),
        }
    }

    pub(super) fn personal_token(&mut self) -> Option<RequestToken> {
        self.personal.as_mut().map(|panel| panel.dialog.begin())
    }

    pub(super) fn personal_settle(&mut self, token: RequestToken) -> bool {
        self.personal
            .as_mut()
            .is_some_and(|panel| panel.dialog.settle(token))
    }

    pub(super) fn personal_error(&mut self, message: String) {
        if let Some(panel) = &mut self.personal {
            panel.local_error = message;
        }
    }

    pub(super) fn load_personal_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self
            .personal
            .as_ref()
            .is_some_and(|panel| panel.profile_loading)
        {
            return;
        }
        let Some(token) = self.personal_token() else {
            return;
        };
        let epoch = self
            .personal
            .as_ref()
            .map(|panel| panel.profile_epoch)
            .unwrap_or_default();
        if let Some(panel) = &mut self.personal {
            panel.profile_loading = true;
            panel.local_error.clear();
        }
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.profile())
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.personal_settle(token) {
                    return;
                }
                let keep_dispatched = s.active_profile_save.is_some();
                if let Some(panel) = &mut s.personal {
                    panel.profile_loading = false;
                    // A successful PATCH answered after this GET started. Its
                    // confirmed snapshot is newer than the page on the wire.
                    if panel.profile_epoch == epoch {
                        match result {
                            Ok(profile) => {
                                panel.apply_profile_snapshot(profile, keep_dispatched, window, cx)
                            }
                            Err(error) => panel.local_error = error,
                        }
                    }
                    panel.sync_profile_load();
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn load_personal_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self
            .personal
            .as_ref()
            .is_some_and(|panel| panel.sessions_loading)
        {
            return;
        }
        let Some(token) = self.personal_token() else {
            return;
        };
        if let Some(panel) = &mut self.personal {
            panel.sessions_loading = true;
            panel.sessions_error.clear();
        }
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.sessions())
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.personal_settle(token) {
                        return;
                    }
                    if let Some(panel) = &mut s.personal {
                        panel.sessions_loading = false;
                        match result {
                            Ok(sessions) => panel.sessions = sessions,
                            Err(error) => panel.sessions_error = error,
                        }
                        panel.sync_profile_load();
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn load_personal_privacy(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.message_privacy())
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.personal_settle(token) {
                        return;
                    }
                    if let Some(panel) = &mut s.personal {
                        match result {
                            Ok(privacy) => panel.privacy = Some(privacy),
                            Err(error) => panel.local_error = error,
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

    pub(super) fn load_voice_devices(&mut self, cx: &mut Context<Self>) {
        if self.voice_settings.devices_loading {
            return;
        }
        let generation = self.personal.as_ref().map(|panel| panel.dialog.generation);
        self.voice_settings.devices_loading = true;
        self.voice_settings.devices_error.clear();
        let task = cx.background_spawn(async {
            #[cfg(feature = "voxhold-media")]
            {
                voxhold_media::devices::list_devices()
                    .into_iter()
                    .map(|device| crate::voice_settings::Device {
                        id: device.id,
                        name: device.name,
                        input: device.input,
                    })
                    .collect::<Vec<_>>()
            }
            #[cfg(not(feature = "voxhold-media"))]
            {
                Vec::new()
            }
        });
        cx.spawn(async move |this, cx| {
            let devices = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    s.voice_settings.devices_loading = false;
                    // Closing the voice tab or the dialog drops the list answer.
                    if s.active_settings == Some(SettingsKind::Personal)
                        && s.personal.as_ref().map(|panel| panel.dialog.generation) == generation
                    {
                        s.voice_settings.devices = devices;
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn personal_action(
        &mut self,
        action: personal_settings::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use personal_settings::Action;
        match action {
            Action::Retry => {
                let tab = self.personal.as_ref().map(|panel| panel.tab()).unwrap_or(0);
                if tab == 0 {
                    self.load_personal_profile(window, cx);
                } else if tab == 2 {
                    self.load_personal_privacy(cx);
                }
            }
            Action::RetrySessions => self.load_personal_sessions(cx),
            Action::SaveProfile => self.save_profile(window, cx),
            Action::CancelProfile => {
                // "Отмена" must not roll the form back while a PATCH owns the
                // editor values: that would clear `dispatched`, and the
                // successful answer would no longer update the fields.
                if self.active_profile_save.is_some() {
                    return;
                }
                let confirmed = self
                    .personal
                    .as_ref()
                    .and_then(|panel| panel.profile.clone());
                if let Some(profile) = confirmed {
                    if let Some(panel) = &mut self.personal {
                        panel.about_form.load(profile.about.clone());
                        panel.country_form.load(profile.country_code.clone());
                        panel.set_profile(profile, window, cx);
                        panel.validation.clear();
                        panel.local_error.clear();
                        panel.notice.clear();
                    }
                    cx.notify();
                }
            }
            Action::OpenCountry => {
                if let Some(panel) = &mut self.personal {
                    panel.country_open = true;
                }
                cx.notify();
            }
            Action::CloseCountry => {
                if let Some(panel) = &mut self.personal {
                    panel.country_open = false;
                }
                cx.notify();
            }
            Action::ChooseCountry(index) => {
                if let Some(panel) = &mut self.personal {
                    panel.country_index = index;
                    panel.country_open = false;
                }
                cx.notify();
            }
            Action::PickAvatar => self.pick_avatar(window, cx),
            Action::SaveAvatar => self.save_avatar(window, cx),
            Action::CancelAvatar => {
                if let Some(panel) = &mut self.personal {
                    panel.crop = None;
                }
                cx.notify();
            }
            Action::RemoveAvatar => self.remove_avatar(cx),
            Action::ChangePassword => self.change_password(window, cx),
            Action::RevokeSession(id) => self.revoke_session(id, cx),
            Action::RevokeOthers => self.revoke_other_sessions(cx),
            Action::SetPrivacy(index) => self.set_privacy(index, cx),
            Action::Unblock(user) => self.unblock_user(user, cx),
            Action::ToggleTheme => {
                self.toggle_theme(window, cx);
                if let Some(panel) = &mut self.personal {
                    panel.theme = self.theme_mode;
                }
            }
            Action::ToggleNotifications => {
                let enabled = !self.notifications.enabled;
                let changed = self.notifications.set_enabled(enabled);
                if changed && !enabled {
                    self.notification_tray.clear(hwnd(window));
                }
                if let Some(panel) = &mut self.personal {
                    panel.notifications_enabled = self.notifications.enabled;
                    panel.notifications_error = if changed {
                        String::new()
                    } else {
                        self.notifications.error.clone()
                    };
                }
                cx.notify();
            }
            Action::DeleteAccount => {
                if let Some(panel) = &mut self.personal {
                    if panel.delete_confirm {
                        self.delete_account(window, cx);
                    } else {
                        panel.delete_confirm = true;
                        cx.notify();
                    }
                }
            }
            Action::ConfirmDelete => self.delete_account(window, cx),
            Action::CancelDelete => {
                if let Some(panel) = &mut self.personal {
                    panel.delete_confirm = false;
                    panel
                        .delete_name
                        .update(cx, |input, cx| input.set_value(String::new(), window, cx));
                    panel
                        .delete_password
                        .update(cx, |input, cx| input.set_value(String::new(), window, cx));
                }
                cx.notify();
            }
            Action::RefreshDevices => self.load_voice_devices(cx),
            Action::SelectInput(index) | Action::SelectOutput(index) => {
                let input = matches!(action, Action::SelectInput(_));
                let (_, ids, _) = crate::voice_settings::device_choices(
                    &self.voice_settings.prefs,
                    &self.voice_settings.devices,
                    input,
                );
                if let Some(id) = ids.get(index).cloned() {
                    if input {
                        self.voice_settings.prefs.input = id;
                    } else {
                        self.voice_settings.prefs.output = id;
                    }
                    self.voice_settings.persist();
                    #[cfg(feature = "voxhold-media")]
                    if let Some(voice) = &self.voice {
                        let prefs = self.voice_settings.prefs.clone();
                        let output_selected = !input;
                        voice.update(cx, |panel, _| {
                            panel.set_devices(Some(prefs.input), Some(prefs.output.clone()));
                            if output_selected {
                                panel.select_output(Some(prefs.output));
                            }
                        });
                    }
                }
                cx.notify();
            }
            Action::SetVolume(which, value) => {
                match which {
                    "input" => {
                        // Input gain is applied by the DSP capture path (R07b).
                        self.voice_settings.prefs.input_volume = value.clamp(0, 200)
                    }
                    "output" => {
                        self.voice_settings.prefs.output_volume = value.clamp(0, 200);
                        #[cfg(feature = "voxhold-media")]
                        if let Some(voice) = &self.voice {
                            voice.update(cx, |panel, _| panel.set_output_volume(value));
                        }
                    }
                    "noise" => self.voice_settings.prefs.noise_gate = value.clamp(0, 100),
                    _ => self.voice_settings.prefs.sound_volume = value.clamp(0, 100),
                }
                self.voice_settings.persist();
                cx.notify();
            }
            Action::SetBitrate(bitrate) => {
                if crate::voice_settings::BITRATES.contains(&bitrate) {
                    self.voice_settings.prefs.bitrate = bitrate;
                    self.voice_settings.persist();
                    #[cfg(feature = "voxhold-media")]
                    if let Some(voice) = &self.voice {
                        voice.update(cx, |panel, _| panel.set_bitrate(bitrate as u32));
                    }
                    cx.notify();
                }
            }
            Action::ToggleSounds => {
                self.voice_settings.prefs.sounds = !self.voice_settings.prefs.sounds;
                self.voice_settings.persist();
                cx.notify();
            }
            Action::SetNoiseMode(index) => {
                self.voice_settings.prefs.noise_mode = if index == 1 {
                    "threshold".into()
                } else {
                    "auto".into()
                };
                self.voice_settings.persist();
                cx.notify();
            }
            Action::SetInputMode(index) => {
                let push_to_talk = index == 1;
                self.voice_settings.prefs.input_mode = if push_to_talk {
                    "push_to_talk".into()
                } else {
                    "voice_activity".into()
                };
                self.voice_settings.persist();
                #[cfg(feature = "voxhold-media")]
                if let Some(voice) = &self.voice {
                    voice.update(cx, |voice, cx| {
                        voice.set_push_to_talk(push_to_talk);
                        cx.notify();
                    });
                }
                cx.notify();
            }
            Action::ResetVoice => {
                let mut prefs = crate::voice_settings::VoicePrefs::default();
                prefs.input = self.voice_settings.prefs.input.clone();
                prefs.output = self.voice_settings.prefs.output.clone();
                self.voice_settings.prefs = prefs;
                self.voice_settings.persist();
                #[cfg(feature = "voxhold-media")]
                if let Some(voice) = &self.voice {
                    voice.update(cx, |voice, cx| {
                        voice.set_push_to_talk(false);
                        cx.notify();
                    });
                }
                cx.notify();
            }
            Action::CropZoom(value) => self.edit_crop(window, cx, 0, value),
            Action::CropX(value) => self.edit_crop(window, cx, 1, value),
            Action::CropY(value) => self.edit_crop(window, cx, 2, value),
        }
    }

    pub(super) fn save_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.personal else { return };
        if self.active_profile_save.is_some() {
            return;
        }
        let about = panel.about_value(cx);
        let country_code = panel.country_code();
        let sent_about = about.clone();
        let sent_country = country_code.clone();
        let country = country_code.clone().unwrap_or_default();
        if let Err(message) = settings::validate_about(&about) {
            if let Some(panel) = &mut self.personal {
                panel.validation = message.into();
            }
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let request = self
            .personal
            .as_mut()
            .map(|panel| panel.next_profile_request())
            .unwrap_or_default();
        self.active_profile_save = Some(request);
        if let Some(panel) = &mut self.personal {
            panel.validation.clear();
            panel.notice.clear();
            panel.local_error.clear();
            panel.about_form.begin(request, about.clone());
            panel.country_form.begin(request, country_code);
        }
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.update_profile(about.trim(), &country))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session {
                    return;
                }
                // The lock belongs to the session, not to the dialog. Release
                // it on the answer of the request that owns it, even when the
                // form was closed and reopened meanwhile.
                if s.active_profile_save == Some(request) {
                    s.active_profile_save = None;
                }
                let open_profile = s.active_settings == Some(SettingsKind::Personal);
                let accepted = s.personal_settle(token);
                let Some(panel) = &mut s.personal else {
                    return;
                };
                match result {
                    Ok(profile) => {
                        panel.profile_epoch = panel.profile_epoch.wrapping_add(1);
                        let current_about = panel.about_value(cx);
                        let current_country = panel.country_code();
                        let reply_about = profile.about.clone();
                        let reply_country = profile.country_code.clone();
                        let profile_visible = open_profile && panel.tab() == 0;
                        if accepted {
                            // A local edit made after dispatch stays in the
                            // editor; only the confirmed snapshot is replaced.
                            let apply_about = panel.about_form.settle(
                                request,
                                reply_about.clone(),
                                &current_about,
                            );
                            let apply_country = panel.country_form.settle(
                                request,
                                reply_country.clone(),
                                &current_country,
                            );
                            if apply_about {
                                panel.about.update(cx, |input, cx| {
                                    input.set_value(reply_about.clone(), window, cx)
                                });
                            }
                            if apply_country {
                                panel.country_index = crate::settings::country_index(
                                    &panel.countries,
                                    reply_country.as_deref(),
                                );
                            }
                        } else {
                            // The PATCH came from a form that was closed,
                            // reopened or left on another tab. The panel may
                            // already hold an older GET snapshot; replace the
                            // confirmed values on every tab but keep fields the
                            // user edited locally.
                            let about_edited = current_about != panel.about_form.snapshot
                                && current_about != sent_about;
                            let country_edited = current_country != panel.country_form.snapshot
                                && current_country != sent_country;
                            panel.about_form.load(reply_about.clone());
                            panel.country_form.load(reply_country.clone());
                            if !about_edited {
                                panel.about.update(cx, |input, cx| {
                                    input.set_value(reply_about.clone(), window, cx)
                                });
                            }
                            if !country_edited {
                                panel.country_index = crate::settings::country_index(
                                    &panel.countries,
                                    reply_country.as_deref(),
                                );
                            }
                        }
                        panel.profile = Some(profile);
                        if accepted || profile_visible {
                            panel.notice = "Профиль обновлён.".into();
                        }
                    }
                    Err(error) => {
                        if accepted {
                            panel.local_error = error;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn change_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.personal else { return };
        let current = panel.password_current.read(cx).value().to_string();
        let next = panel.password_next.read(cx).value().to_string();
        let confirm = panel.password_confirm.read(cx).value().to_string();
        if let Err(message) = settings::validate_password(&current, &next, &confirm) {
            if let Some(panel) = &mut self.personal {
                panel.validation = message.into();
            }
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        if let Some(panel) = &mut self.personal {
            panel.validation.clear();
            panel.notice.clear();
            panel.local_error.clear();
        }
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.change_password(&current, &next, &confirm))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.personal_settle(token) {
                    return;
                }
                match result {
                    Ok(()) => {
                        if let Some(panel) = &mut s.personal {
                            for input in [
                                &panel.password_current,
                                &panel.password_next,
                                &panel.password_confirm,
                            ] {
                                input.update(cx, |input, cx| {
                                    input.set_value(String::new(), window, cx)
                                });
                            }
                            panel.notice = "Пароль изменён. Остальные сессии завершены.".into();
                        }
                        s.load_personal_sessions(cx);
                    }
                    Err(error) => s.personal_error(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn revoke_session(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.revoke_session(id))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.personal_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => s.load_personal_sessions(cx),
                        Err(error) => s.personal_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn revoke_other_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.revoke_other_sessions())
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.personal_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => s.load_personal_sessions(cx),
                        Err(error) => s.personal_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn set_privacy(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(mode) = settings::PRIVACY_MODES.get(index) else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let mode = (*mode).to_owned();
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_message_privacy(&mode))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.personal_settle(token) {
                        return;
                    }
                    match result {
                        Ok(privacy) => {
                            if let Some(panel) = &mut s.personal {
                                panel.privacy = Some(privacy);
                            }
                        }
                        Err(error) => s.personal_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn unblock_user(&mut self, user: i64, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.block_user(user, false))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.personal_settle(token) {
                        return;
                    }
                    if let Err(error) = result {
                        s.personal_error(error);
                    } else if let Some(panel) = &mut s.personal {
                        if let Some(privacy) = &mut panel.privacy {
                            privacy.blocked_user_ids.retain(|id| *id != user);
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

    fn delete_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.personal else { return };
        let Some(profile) = &panel.profile else {
            return;
        };
        let name = panel.delete_name.read(cx).value().to_string();
        let password = panel.delete_password.read(cx).value().to_string();
        if name != profile.username || password.is_empty() {
            if let Some(panel) = &mut self.personal {
                panel.validation = "Введите имя аккаунта и пароль.".into();
            }
            cx.notify();
            return;
        }
        if self.servers.iter().any(|server| server.role == "owner") {
            if let Some(panel) = &mut self.personal {
                panel.validation =
                    "Владелец не может удалить пространство или свой аккаунт из приложения.".into();
            }
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.personal_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.delete_account(&password))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                // The account is already gone on the server. A closed form
                // must not swallow the success; only the dialog token decides
                // where an error can still be shown.
                if s.session_generation != session {
                    return;
                }
                match result {
                    Ok(()) => {
                        s.close_settings(false, window, cx);
                        s.logout(window, cx);
                    }
                    Err(error) => {
                        if s.personal_settle(token) {
                            s.personal_error(error);
                            cx.notify();
                        }
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }
}
