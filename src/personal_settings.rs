//! Personal settings form state: profile, avatar crop, password, sessions,
//! appearance, system notifications and privacy.
//!
//! Network work lives in `src/app/personal_ops.rs`; this module only holds the
//! confirmed snapshots, editor values and per-request bookkeeping.

use std::sync::Arc;

use gpui::{AppContext, Context, Entity, RenderImage, Window};
use gpui_component::input::InputState;
use voxhold_api::{AccountSession, MessagePrivacy, Profile};

use crate::settings::{self, Crop, Dialog, LoadState, Scope, Tab};
use crate::theme::ThemeMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Retry,
    RetrySessions,
    SaveProfile,
    CancelProfile,
    OpenCountry,
    CloseCountry,
    ChooseCountry(usize),
    PickAvatar,
    SaveAvatar,
    CancelAvatar,
    RemoveAvatar,
    ChangePassword,
    RevokeSession(i64),
    RevokeOthers,
    SetPrivacy(usize),
    Unblock(i64),
    ToggleTheme,
    ToggleNotifications,
    DeleteAccount,
    ConfirmDelete,
    CancelDelete,
    RefreshDevices,
    SelectInput(usize),
    SelectOutput(usize),
    SetVolume(&'static str, i64),
    SetBitrate(i64),
    ToggleSounds,
    SetNoiseMode(usize),
    SetInputMode(usize),
    ResetVoice,
    CropZoom(i32),
    CropX(i32),
    CropY(i32),
}

pub const TABS: [Tab; 3] = [
    Tab::new("Профиль", "people"),
    Tab::new("Голос и звук", "headphones"),
    Tab::new("Приватность", "lock"),
];

/// One decoded avatar crop draft. `source` is already EXIF-oriented and
/// downscaled to at most 640 px; `crop` uses the Go client's 0..100 space and
/// is mapped back to original pixels for upload.
pub struct CropDraft {
    pub data: Arc<Vec<u8>>,
    pub source: Arc<image::DynamicImage>,
    pub width: u32,
    pub height: u32,
    pub crop: Crop,
    pub preview: Option<Arc<RenderImage>>,
    pub preview_token: u64,
}

impl CropDraft {
    /// `crop_x`, `crop_y`, `crop_size` in the original oriented image.
    pub fn upload_crop(&self) -> (i32, i32, i32) {
        let (x, y) = self.crop.origin(self.width, self.height);
        (
            x as i32,
            y as i32,
            self.crop.size(self.width, self.height) as i32,
        )
    }
}

pub struct PersonalSettings {
    pub dialog: Dialog,
    pub about: Entity<InputState>,
    pub country_search: Entity<InputState>,
    pub password_current: Entity<InputState>,
    pub password_next: Entity<InputState>,
    pub password_confirm: Entity<InputState>,
    pub delete_name: Entity<InputState>,
    pub delete_password: Entity<InputState>,
    pub profile: Option<Profile>,
    /// Confirmed profile value, the value sent with the last request and the
    /// request token, kept apart from the live editor text.
    pub about_form: settings::FormValue<String>,
    pub country_form: settings::FormValue<Option<String>>,
    pub profile_loading: bool,
    /// A PATCH is in flight. Editors stay enabled; only the save button and a
    /// repeated `save_profile` call are blocked, and every PATCH gets its own
    /// request id so one answer cannot settle another request.
    pub profile_saving: bool,
    pub profile_request: u64,
    pub avatar: Option<Arc<RenderImage>>,
    pub avatar_version: Option<String>,
    pub avatar_loading: bool,
    pub avatar_missing: bool,
    /// Identifies the chosen file. A decode that finishes after a newer choice
    /// is discarded instead of replacing the newer crop draft.
    pub avatar_pick: u64,
    pub crop: Option<CropDraft>,
    pub sessions: Vec<AccountSession>,
    pub sessions_loading: bool,
    pub sessions_error: String,
    pub privacy: Option<MessagePrivacy>,
    pub country_open: bool,
    pub country_index: usize,
    pub validation: String,
    pub notice: String,
    pub local_error: String,
    pub delete_confirm: bool,
    pub theme: ThemeMode,
    pub notifications_enabled: bool,
    pub notifications_error: String,
    pub countries: Vec<settings::Country>,
}

impl PersonalSettings {
    pub fn new(window: &mut Window, cx: &mut Context<crate::app::Voxhold>) -> Self {
        let input = |cx: &mut Context<crate::app::Voxhold>,
                     window: &mut Window,
                     placeholder: &'static str,
                     masked: bool| {
            cx.new(|cx| {
                let state = InputState::new(window, cx).placeholder(placeholder);
                if masked {
                    state.masked(true)
                } else {
                    state
                }
            })
        };
        Self {
            dialog: Dialog::new("Настройки", Scope::default(), TABS.to_vec()),
            about: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line()
                    .placeholder("Пара слов о себе, интересах или текущем проекте…")
            }),
            country_search: input(cx, window, "Найти…", false),
            password_current: input(cx, window, "Текущий пароль", true),
            password_next: input(cx, window, "Новый пароль", true),
            password_confirm: input(cx, window, "Повторите новый пароль", true),
            delete_name: input(cx, window, "Имя пользователя", false),
            delete_password: input(cx, window, "Пароль для подтверждения", true),
            profile: None,
            about_form: settings::FormValue::new(String::new()),
            country_form: settings::FormValue::new(None),
            profile_loading: false,
            profile_saving: false,
            profile_request: 0,
            avatar: None,
            avatar_version: None,
            avatar_loading: false,
            avatar_missing: false,
            avatar_pick: 0,
            crop: None,
            sessions: Vec::new(),
            sessions_loading: false,
            sessions_error: String::new(),
            privacy: None,
            country_open: false,
            country_index: 0,
            validation: String::new(),
            notice: String::new(),
            local_error: String::new(),
            delete_confirm: false,
            theme: ThemeMode::System,
            notifications_enabled: false,
            notifications_error: String::new(),
            countries: settings::countries(),
        }
    }

    /// Fresh dialog instance, like Go `showSettings` creating a new one. The
    /// editors keep their text until the profile snapshot arrives.
    pub fn reopen(&mut self, theme: ThemeMode, notifications: bool) {
        self.dialog = Dialog::new("Настройки", Scope::default(), TABS.to_vec());
        self.profile_loading = false;
        self.profile_saving = false;
        self.profile_request = self.profile_request.wrapping_add(1);
        self.avatar_pick = self.avatar_pick.wrapping_add(1);
        self.sessions_loading = false;
        self.sessions_error.clear();
        self.validation.clear();
        self.notice.clear();
        self.local_error.clear();
        self.delete_confirm = false;
        self.country_open = false;
        self.theme = theme;
        self.notifications_enabled = notifications;
        self.notifications_error.clear();
        self.avatar_loading = false;
        self.avatar_missing = false;
    }

    pub fn tab(&self) -> usize {
        self.dialog.active
    }

    /// The profile tab owns two independent requests. The tab counts as
    /// loaded only after both have finished, so leaving while either one is in
    /// flight cannot mark the tab as complete.
    pub fn sync_profile_load(&mut self) {
        if self.dialog.active != 0 {
            return;
        }
        if self.profile_loading || self.sessions_loading {
            if let Some(slot) = self.dialog.loads.get_mut(0) {
                *slot = LoadState::Loading;
            }
        } else if let Some(slot) = self.dialog.loads.get_mut(0) {
            *slot = if self.profile.is_some() {
                LoadState::Loaded
            } else {
                LoadState::Error
            };
        }
    }

    /// Drop the request bookkeeping of a profile tab that is being left. The
    /// dialog generation already rejects its late answers; clearing the flags
    /// lets the return start a fresh GET instead of keeping "Загрузка…".
    pub fn cancel_profile_loads(&mut self) {
        self.profile_loading = false;
        self.sessions_loading = false;
    }

    pub fn next_profile_request(&mut self) -> u64 {
        self.profile_request = self.profile_request.wrapping_add(1);
        self.profile_request
    }

    pub fn about_value(&self, cx: &gpui::App) -> String {
        self.about.read(cx).value().to_string()
    }

    pub fn set_profile(
        &mut self,
        profile: Profile,
        window: &mut Window,
        cx: &mut Context<crate::app::Voxhold>,
    ) {
        self.about.update(cx, |input, cx| {
            input.set_value(profile.about.clone(), window, cx)
        });
        self.country_index =
            settings::country_index(&self.countries, profile.country_code.as_deref());
        self.about_form.load(profile.about.clone());
        self.country_form.load(profile.country_code.clone());
        self.profile = Some(profile);
    }

    pub fn country_code(&self) -> Option<String> {
        if self.country_index == 0 {
            return None;
        }
        self.countries
            .get(self.country_index - 1)
            .map(|country| country.code.clone())
    }

    /// Avatar version from the member snapshot; a missing version means there
    /// is no image to request.
    pub fn set_avatar_version(&mut self, version: Option<String>) {
        self.avatar_version = version.filter(|value| !value.is_empty());
    }

    pub fn clear_avatar(&mut self) {
        self.avatar = None;
        self.avatar_version = None;
    }
}
