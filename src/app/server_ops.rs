//! Server administration operations.

use super::*;
use crate::settings::{self, RequestToken};
impl Voxhold {
    fn server_token(&mut self) -> Option<RequestToken> {
        self.server_settings
            .as_mut()
            .map(|panel| panel.dialog.begin())
    }
    fn server_settle(&mut self, token: RequestToken) -> bool {
        self.server_settings
            .as_mut()
            .is_some_and(|panel| panel.dialog.settle(token))
    }
    fn server_error(&mut self, message: String) {
        if let Some(panel) = &mut self.server_settings {
            panel.dialog.error = message;
        }
    }
    pub(super) fn load_server_general(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.cors_origins())
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.server_settle(token) {
                    return;
                }
                match result {
                    Ok(origins) => {
                        if let Some(panel) = &mut s.server_settings {
                            panel.general_loaded = true;
                            let text = origins.join("\n");
                            panel
                                .cors
                                .update(cx, |input, cx| input.set_value(text, window, cx));
                        }
                    }
                    Err(error) => s.server_error(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn load_server_roles(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let server = self.server;
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.roles(server))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(roles) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.roles = roles;
                                panel.roles_loaded = true;
                                panel.roles_error.clear();
                                panel.sync_assigned();
                            }
                            s.load_role_grants(cx);
                        }
                        Err(error) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.roles_error = error;
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
    fn load_role_grants(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(server) = self.server_settings.as_ref().map(|panel| panel.server) else {
            return;
        };
        let Some(channel) = self
            .server_settings
            .as_ref()
            .and_then(|panel| panel.selected_channel())
        else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.role_grants(server, channel))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    if let Some(panel) = &mut s.server_settings {
                        if panel.selected_channel() != Some(channel) {
                            return;
                        }
                        match result {
                            Ok(ids) => {
                                panel.grants = ids.into_iter().collect();
                                panel.grants_loaded = true;
                            }
                            Err(error) => panel.dialog.error = error,
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
    pub(super) fn load_server_storage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.storage())
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.server_settle(token) {
                    return;
                }
                match result {
                    Ok(state) => {
                        if let Some(panel) = &mut s.server_settings {
                            panel.apply_storage(state, window, cx);
                            panel.storage_error.clear();
                        }
                    }
                    Err(error) => {
                        if let Some(panel) = &mut s.server_settings {
                            panel.storage_error = error;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn load_server_bans(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let server = self.server;
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.bans(server))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(bans) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.bans = bans;
                                panel.bans_loaded = true;
                                panel.bans_error.clear();
                            }
                        }
                        Err(error) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.bans_error = error;
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
    pub(super) fn server_action(
        &mut self,
        action: server_settings::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use server_settings::Action;
        match action {
            Action::Retry(tab) => match tab {
                0 => self.load_server_general(window, cx),
                1 => self.load_server_roles(cx),
                2 => self.load_server_storage(window, cx),
                3 => self.load_server_bans(cx),
                _ => {}
            },
            Action::SaveName => self.save_server_name(window, cx),
            Action::SaveCors => self.save_cors(cx),
            Action::Diagnostics => self.save_diagnostics(window, cx),
            Action::RoleCreate => {
                if let Some(panel) = &mut self.server_settings {
                    panel.role_edit = Some(server_settings::RoleEdit::new_role());
                    let color = "#8b93a7".to_owned();
                    panel
                        .role_name
                        .update(cx, |input, cx| input.set_value(String::new(), window, cx));
                    panel
                        .role_color
                        .update(cx, |input, cx| input.set_value(color, window, cx));
                    panel.role_preview = None;
                }
                cx.notify();
            }
            Action::RoleEdit(id) => {
                if let Some(panel) = &mut self.server_settings {
                    if let Some(role) = panel.roles.iter().find(|role| role.id == id).cloned() {
                        let edit = server_settings::RoleEdit::from_role(&role);
                        panel.role_name.update(cx, |input, cx| {
                            input.set_value(edit.name.clone(), window, cx)
                        });
                        panel.role_color.update(cx, |input, cx| {
                            input.set_value(edit.color.clone(), window, cx)
                        });
                        panel.role_edit = Some(edit);
                        panel.role_preview = None;
                    }
                }
                cx.notify();
            }
            Action::RoleToggle(bit) => {
                if let Some(panel) = &mut self.server_settings {
                    if let Some(edit) = &mut panel.role_edit {
                        edit.permissions ^= bit;
                    }
                }
                cx.notify();
            }
            Action::RoleCancel => {
                if let Some(panel) = &mut self.server_settings {
                    panel.role_edit = None;
                }
                cx.notify();
            }
            Action::RoleSave => self.save_role(cx),
            Action::RolePreview(id) => self.preview_role(id, cx),
            Action::RoleDelete(id) => {
                if let Some(panel) = &mut self.server_settings {
                    panel.role_delete = Some(id);
                }
                cx.notify();
            }
            Action::RoleDeleteCancel => {
                if let Some(panel) = &mut self.server_settings {
                    panel.role_delete = None;
                }
                cx.notify();
            }
            Action::RoleDeleteConfirm => self.delete_role(cx),
            Action::RoleTarget(index) => {
                if let Some(panel) = &mut self.server_settings {
                    panel.role_target = index;
                    panel.sync_assigned();
                }
                cx.notify();
            }
            Action::AssignToggle(id) => {
                if let Some(panel) = &mut self.server_settings {
                    if !panel.assigned.remove(&id) {
                        panel.assigned.insert(id);
                    }
                }
                cx.notify();
            }
            Action::AssignSave => self.save_member_roles(cx),
            Action::GrantChannel(index) => {
                if let Some(panel) = &mut self.server_settings {
                    if panel.grant_channel != index {
                        panel.grant_channel = index;
                        panel.grants.clear();
                        panel.grants_loaded = false;
                    }
                }
                self.load_role_grants(cx);
            }
            Action::GrantToggle(id) => {
                if let Some(panel) = &mut self.server_settings {
                    if !panel.grants.remove(&id) {
                        panel.grants.insert(id);
                    }
                }
                cx.notify();
            }
            Action::GrantsSave => self.save_role_grants(cx),
            Action::StorageSave => self.save_storage(window, cx),
            Action::CleanupCategory(index) => {
                if let Some(panel) = &mut self.server_settings {
                    panel.cleanup.choose_category(index);
                    panel.storage_result.clear();
                }
                cx.notify();
            }
            Action::CleanupPeriod(index) => {
                if let Some(panel) = &mut self.server_settings {
                    panel.cleanup.choose_period(index);
                    panel.storage_result.clear();
                }
                cx.notify();
            }
            Action::CleanupPreview => self.preview_cleanup(cx),
            Action::CleanupConfirm => self.cleanup(window, cx),
            Action::Unban(id) => {
                if let Some(panel) = &mut self.server_settings {
                    panel.confirm_unban = Some(id);
                }
                cx.notify();
            }
            Action::UnbanCancel => {
                if let Some(panel) = &mut self.server_settings {
                    panel.confirm_unban = None;
                }
                cx.notify();
            }
            Action::UnbanConfirm => self.unban(cx),
        }
    }
    fn save_server_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let name = panel.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let server = panel.server;
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let patch_name = name.clone();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.rename_server(server, &patch_name))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.server_settle(token) {
                    return;
                }
                match result {
                    Ok(()) => {
                        if let Some(server) = s.servers.iter_mut().find(|s| s.id == server) {
                            server.name = name.clone();
                        }
                        if let Some(panel) = &mut s.server_settings {
                            panel
                                .name
                                .update(cx, |input, cx| input.set_value(name.clone(), window, cx));
                            panel.general_notice = "Название сохранено.".into();
                        }
                    }
                    Err(error) => s.server_error(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn save_cors(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let origins: Vec<String> = panel
            .cors
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        for origin in &origins {
            if let Err(message) = validate_origin(origin) {
                self.server_error(message);
                cx.notify();
                return;
            }
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_cors_origins(&origins))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.general_notice = "Список origin сохранён.".into();
                                panel.dialog.error.clear();
                            }
                        }
                        Err(error) => s.server_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn save_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        if !panel.general_loaded {
            return;
        }
        let directory = std::env::var_os("USERPROFILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let name = format!("voxhold-diagnostics-{stamp}.json");
        let prompt = cx.prompt_for_new_path(&directory, Some(&name));
        let Some(generation) = self.server_settings.as_ref().map(|p| p.dialog.generation) else {
            return;
        };
        let session = self.session_generation;
        cx.spawn_in(window, async move |this, window| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                _ => return,
            };
            let _ = this.update_in(window, |s, window, cx| {
                if s.server_settings
                    .as_ref()
                    .map(|panel| panel.dialog.generation)
                    != Some(generation)
                    || s.session_generation != session
                {
                    return;
                }
                let Some(client) = s.client.clone() else {
                    return;
                };
                let runtime = s.runtime.clone();
                let task = cx.background_spawn(async move {
                    runtime.block_on(async move {
                        let value = client
                            .client_diagnostics()
                            .await
                            .map_err(|error| error.to_string())?;
                        let text = serde_json::to_string_pretty(&value)
                            .map_err(|error| error.to_string())?;
                        let bytes = format!("{text}\n").into_bytes();
                        let mut file = crate::file_io::PartFile::create(&path)?;
                        file.write(&bytes)?;
                        file.finish(Some(bytes.len() as u64))?;
                        Ok::<_, String>(())
                    })
                });
                cx.spawn_in(window, async move |this, window| {
                    let result = task.await;
                    let _ = this.update_in(window, |s, _window, cx| {
                        let Some(panel) = &mut s.server_settings else {
                            return;
                        };
                        if s.session_generation != session || panel.dialog.generation != generation
                        {
                            return;
                        }
                        match result {
                            Ok(()) => panel.general_notice = "Диагностика сохранена.".into(),
                            Err(error) => panel.dialog.error = error,
                        }
                        cx.notify();
                    });
                })
                .detach();
            });
        })
        .detach();
        cx.notify();
    }
    fn save_role(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let Some(mut edit) = panel.role_edit.clone() else {
            return;
        };
        edit.name = panel.role_name.read(cx).value().trim().to_owned();
        edit.color = panel.role_color.read(cx).value().trim().to_owned();
        if edit.name.is_empty() {
            return;
        }
        if !valid_color(&edit.color) {
            self.server_error("Укажите цвет в формате #RRGGBB.".into());
            cx.notify();
            return;
        }
        let server = panel.server;
        let role = voxhold_api::CustomRole {
            id: edit.id,
            name: edit.name,
            color: edit.color,
            permissions: edit.permissions & 255,
            user_ids: Vec::new(),
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.save_role(server, &role))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.role_edit = None;
                            }
                            s.load_server_roles(cx);
                        }
                        Err(error) => s.server_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn preview_role(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(server) = self.server_settings.as_ref().map(|panel| panel.server) else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.role_preview(server, id))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(preview) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.role_preview = Some(preview);
                                panel.role_preview_for = id;
                            }
                        }
                        Err(error) => s.server_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn delete_role(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let Some(id) = panel.role_delete else { return };
        let server = panel.server;
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.delete_role(server, id))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.role_delete = None;
                            }
                            s.load_server_roles(cx);
                        }
                        Err(error) => s.server_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn save_member_roles(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let Some(member) = panel.selected_member().cloned() else {
            return;
        };
        let server = panel.server;
        let mut ids: Vec<i64> = panel.assigned.iter().copied().collect();
        ids.sort_unstable();
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_member_roles(server, member.user_id, &ids))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => s.load_server_roles(cx),
                        Err(error) => s.server_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn save_role_grants(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let Some(channel) = panel.selected_channel() else {
            return;
        };
        let server = panel.server;
        let mut ids: Vec<i64> = panel.grants.iter().copied().collect();
        ids.sort_unstable();
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_role_grants(server, channel, &ids))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.general_notice = "Доступ ролей сохранён.".into();
                            }
                        }
                        Err(error) => s.server_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
    fn save_storage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let Some(state) = panel.storage.clone() else {
            return;
        };
        let values = panel.quota_values(cx);
        let parsed = settings::parse_quotas(
            [&values[0], &values[1], &values[2]],
            panel.quota_bytes,
            [
                &panel.quota_text[0],
                &panel.quota_text[1],
                &panel.quota_text[2],
            ],
            state.capacity_bytes,
        );
        let quotas = match parsed {
            Ok(quotas) => quotas,
            Err(message) => {
                if let Some(panel) = &mut self.server_settings {
                    panel.storage_result = message.into();
                }
                cx.notify();
                return;
            }
        };
        let body = serde_json::json!({
                    "images": quotas[0],            "files": quotas[1],            "recordings": quotas[2],        }
        );
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.set_storage(body))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.server_settle(token) {
                    return;
                }
                match result {
                    Ok(state) => {
                        if let Some(panel) = &mut s.server_settings {
                            panel.apply_storage(state, window, cx);
                            panel.storage_result = "Распределение сохранено.".into();
                        }
                    }
                    Err(error) => {
                        if let Some(panel) = &mut s.server_settings {
                            panel.storage_result = error;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn preview_cleanup(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let category = panel.cleanup.category().to_owned();
        let request_category = category.clone();
        let period = panel.cleanup.period;
        let before = panel.cleanup.before(settings::now());
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.preview_cleanup(&request_category, before))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(plan) => {
                            if let Some(panel) = &mut s.server_settings {
                                let view = settings::CleanupPlanView {
                                    category: plan.category.clone(),
                                    before: plan.before,
                                    bytes: plan.bytes,
                                    count: i64::from(plan.count),
                                    token: plan.token.clone(),
                                };
                                if panel.cleanup.apply(&category, period, view) {
                                    panel.storage_result.clear();
                                } else {
                                    panel.storage_result =
                                        "Расчёт устарел. Выполните его снова.".into();
                                }
                            }
                        }
                        Err(error) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.storage_result = error;
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
    fn cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let Some(plan) = panel.cleanup.confirmed().cloned() else {
            return;
        };
        if plan.count == 0 {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let plan_token = plan.token.clone();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.cleanup(&plan_token))
                .map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.session_generation != session || !s.server_settle(token) {
                    return;
                }
                match result {
                    Ok(outcome) => {
                        if let Some(panel) = &mut s.server_settings {
                            panel.cleanup.plan = None;
                            panel.storage_result = format!(
                                "Удалено {}, освобождено {}.",
                                outcome.count,
                                settings::storage_size(outcome.bytes)
                            );
                        }
                        s.load_server_storage(window, cx);
                    }
                    Err(error) => {
                        if let Some(panel) = &mut s.server_settings {
                            panel.storage_result = error;
                            panel.cleanup.plan = None;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn unban(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.server_settings else {
            return;
        };
        let Some(id) = panel.confirm_unban else {
            return;
        };
        let server = panel.server;
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(token) = self.server_token() else {
            return;
        };
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.unban(server, id))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |s, cx| {
                    if s.session_generation != session || !s.server_settle(token) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(panel) = &mut s.server_settings {
                                panel.confirm_unban = None;
                            }
                            s.load_server_bans(cx);
                        }
                        Err(error) => s.server_error(error),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    // ---- channel settings -------------------------------------------------
}

pub fn validate_origin(value: &str) -> Result<(), String> {
    let invalid = || format!("Некорректный origin «{value}».");
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    if !(url.scheme() == "http" || url.scheme() == "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn valid_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|byte| byte.is_ascii_hexdigit())
}
