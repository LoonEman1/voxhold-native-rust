//! Owner-only server administration: general settings, custom roles, storage
//! and bans. Async work lives in `src/app/admin_ops.rs`.

use std::collections::HashSet;

use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::InputState;
use voxhold_api::{CustomRole, RolePreview, ServerBan, StorageState};

use crate::settings::{self, CleanupSelection, Dialog, Scope, Tab};

pub const TABS: [Tab; 4] = [
    Tab::new("Основное", "settings"),
    Tab::new("Роли", "people"),
    Tab::new("Хранилище", "inbox"),
    Tab::new("Баны", "lock"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Retry(usize),
    SaveName,
    SaveCors,
    Diagnostics,
    RoleCreate,
    RoleEdit(i64),
    RoleSave,
    RolePreview(i64),
    RoleDelete(i64),
    RoleDeleteConfirm,
    RoleDeleteCancel,
    RoleCancel,
    RoleToggle(i32),
    RoleTarget(usize),
    AssignToggle(i64),
    AssignSave,
    GrantChannel(usize),
    GrantToggle(i64),
    GrantsSave,
    StorageSave,
    CleanupCategory(usize),
    CleanupPeriod(usize),
    CleanupPreview,
    CleanupConfirm,
    Unban(i64),
    UnbanConfirm,
    UnbanCancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleEdit {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub permissions: i32,
}

impl RoleEdit {
    pub fn new_role() -> Self {
        Self {
            id: 0,
            name: String::new(),
            color: "#8b93a7".into(),
            permissions: 15,
        }
    }

    pub fn from_role(role: &CustomRole) -> Self {
        Self {
            id: role.id,
            name: role.name.clone(),
            color: role.color.clone(),
            permissions: role.permissions & 255,
        }
    }
}

pub struct ServerSettings {
    pub dialog: Dialog,
    pub server: i64,
    pub name: Entity<InputState>,
    pub cors: Entity<InputState>,
    pub general_loaded: bool,
    pub general_notice: String,

    pub roles: Vec<CustomRole>,
    pub roles_loaded: bool,
    pub roles_error: String,
    pub role_edit: Option<RoleEdit>,
    pub role_name: Entity<InputState>,
    pub role_color: Entity<InputState>,
    pub role_preview: Option<Vec<RolePreview>>,
    pub role_preview_for: i64,
    pub role_delete: Option<i64>,
    pub members: Vec<voxhold_api::Member>,
    pub role_target: usize,
    pub assigned: HashSet<i64>,
    pub restricted_channels: Vec<(i64, String)>,
    pub grant_channel: usize,
    pub grants: HashSet<i64>,
    pub grants_loaded: bool,

    pub storage: Option<StorageState>,
    pub storage_loaded: bool,
    pub storage_error: String,
    pub quotas: [Entity<InputState>; 3],
    pub quota_text: [String; 3],
    pub quota_bytes: [i64; 3],
    pub cleanup: CleanupSelection,
    pub storage_result: String,

    pub bans: Vec<ServerBan>,
    pub bans_loaded: bool,
    pub bans_error: String,
    pub confirm_unban: Option<i64>,
}

impl ServerSettings {
    pub fn new(window: &mut Window, cx: &mut Context<crate::app::Voxhold>) -> Self {
        let field =
            |cx: &mut Context<crate::app::Voxhold>, window: &mut Window, text: &'static str| {
                cx.new(|cx| InputState::new(window, cx).placeholder(text))
            };
        Self {
            dialog: Dialog::new("Настройки сервера", Scope::default(), TABS.to_vec()),
            server: 0,
            name: field(cx, window, "Название сервера"),
            cors: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line()
                    .placeholder("По одному origin на строку")
            }),
            general_loaded: false,
            general_notice: String::new(),
            roles: Vec::new(),
            roles_loaded: false,
            roles_error: String::new(),
            role_edit: None,
            role_name: field(cx, window, "Название роли"),
            role_color: field(cx, window, "Цвет (#RRGGBB)"),
            role_preview: None,
            role_preview_for: 0,
            role_delete: None,
            members: Vec::new(),
            role_target: 0,
            assigned: HashSet::new(),
            restricted_channels: Vec::new(),
            grant_channel: 0,
            grants: HashSet::new(),
            grants_loaded: false,
            storage: None,
            storage_loaded: false,
            storage_error: String::new(),
            quotas: [
                field(cx, window, "0.000"),
                field(cx, window, "0.000"),
                field(cx, window, "0.000"),
            ],
            quota_text: Default::default(),
            quota_bytes: [0; 3],
            cleanup: CleanupSelection::default(),
            storage_result: String::new(),
            bans: Vec::new(),
            bans_loaded: false,
            bans_error: String::new(),
            confirm_unban: None,
        }
    }

    pub fn reopen(
        &mut self,
        server: i64,
        name: &str,
        window: &mut Window,
        cx: &mut Context<crate::app::Voxhold>,
    ) {
        self.dialog = Dialog::new("Настройки сервера", Scope::server(server), TABS.to_vec());
        self.dialog.activate(0);
        self.server = server;
        self.general_loaded = false;
        self.general_notice.clear();
        self.roles.clear();
        self.roles_loaded = false;
        self.roles_error.clear();
        self.role_edit = None;
        self.role_preview = None;
        self.role_delete = None;
        self.restricted_channels.clear();
        self.grants.clear();
        self.grants_loaded = false;
        self.storage = None;
        self.storage_loaded = false;
        self.storage_error.clear();
        self.storage_result.clear();
        self.cleanup = CleanupSelection::default();
        self.bans.clear();
        self.bans_loaded = false;
        self.bans_error.clear();
        self.confirm_unban = None;
        self.name
            .update(cx, |input, cx| input.set_value(name.to_owned(), window, cx));
        self.cors
            .update(cx, |input, cx| input.set_value(String::new(), window, cx));
    }

    pub fn set_members(
        &mut self,
        members: &[voxhold_api::Member],
        channels: &[voxhold_api::Channel],
    ) {
        self.members = members
            .iter()
            .filter(|member| member.role != "owner")
            .cloned()
            .collect();
        self.restricted_channels = channels
            .iter()
            .filter(|channel| channel.access_restricted)
            .map(|channel| (channel.id, channel.name.clone()))
            .collect();
        self.role_target = self.role_target.min(self.members.len().saturating_sub(1));
        self.grant_channel = self
            .grant_channel
            .min(self.restricted_channels.len().saturating_sub(1));
    }

    pub fn selected_member(&self) -> Option<&voxhold_api::Member> {
        self.members.get(self.role_target)
    }

    pub fn selected_channel(&self) -> Option<i64> {
        self.restricted_channels
            .get(self.grant_channel)
            .map(|(id, _)| *id)
    }

    pub fn sync_assigned(&mut self) {
        self.assigned.clear();
        let Some(user_id) = self.selected_member().map(|member| member.user_id) else {
            return;
        };
        let assigned: HashSet<i64> = self
            .roles
            .iter()
            .filter(|role| role.user_ids.contains(&user_id))
            .map(|role| role.id)
            .collect();
        self.assigned = assigned;
    }

    pub fn quota_values(&self, cx: &gpui::App) -> [String; 3] {
        [
            self.quotas[0].read(cx).value().to_string(),
            self.quotas[1].read(cx).value().to_string(),
            self.quotas[2].read(cx).value().to_string(),
        ]
    }

    pub fn apply_storage(
        &mut self,
        state: StorageState,
        window: &mut Window,
        cx: &mut Context<crate::app::Voxhold>,
    ) {
        for (index, category) in settings::CLEANUP_CATEGORIES.iter().enumerate() {
            let bytes = state
                .categories
                .get(*category)
                .map(|value| value.quota_bytes)
                .unwrap_or(0);
            self.quota_bytes[index] = bytes;
            let text = format!("{:.3}", bytes as f64 / 1_073_741_824.0);
            self.quota_text[index] = text.clone();
            self.quotas[index].update(cx, |input, cx| input.set_value(text, window, cx));
        }
        self.storage = Some(state);
        self.storage_loaded = true;
    }
}
