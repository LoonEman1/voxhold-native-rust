//! Settings state that does not depend on GPUI.
//!
//! The dialog tree follows the Go client (`internal/workspace/settings_dialog.go`):
//! one modal, lazily opened tabs, a per-dialog request generation, and forms
//! that keep the confirmed snapshot apart from the local edit and the request
//! in flight. The rules here are unit tested; the views in `src/app` only
//! render and start requests.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ABOUT_LIMIT: usize = 512;
pub const PASSWORD_MIN_CHARS: usize = 8;
pub const PASSWORD_MAX_BYTES: usize = 72;
pub const SLOWMODE_MAX: i64 = 21_600;
pub const CLEANUP_PLAN_SECONDS: i64 = 300;
pub const CLEANUP_CATEGORIES: [&str; 3] = ["images", "files", "recordings"];
pub const CLEANUP_CATEGORY_LABELS: [&str; 3] =
    ["Картинки", "Файлы и короткие видео", "Записи звонков"];
pub const CLEANUP_PERIOD_LABELS: [&str; 5] = [
    "Старше суток",
    "Старше недели",
    "Старше 30 дней",
    "Старше 90 дней",
    "Все сохранённые",
];
pub const CLEANUP_PERIOD_DAYS: [i64; 5] = [1, 7, 30, 90, 0];
pub const NOTIFICATION_MODES: [&str; 3] = ["all", "mentions", "none"];
pub const NOTIFICATION_LABELS: [&str; 3] = ["Все сообщения", "Только упоминания", "Ничего"];
pub const NOTIFICATION_DESCRIPTIONS: [&str; 3] = [
    "Уведомлять о каждом новом сообщении в канале.",
    "Когда упоминают вас, вашу роль или отвечают на ваше сообщение.",
    "Без уведомлений и звука из этого канала.",
];
pub const PRIVACY_MODES: [&str; 3] = ["shared", "everyone", "none"];
pub const PRIVACY_LABELS: [&str; 3] = ["Участники сообщества", "Все пользователи сервера", "Никто"];
pub const PRIVACY_DESCRIPTIONS: [&str; 3] = [
    "Вам могут писать участники вашего сообщества.",
    "В том числе те, кто ещё не вступил в сообщество.",
    "Входящие сообщения отключены, включая существующие беседы.",
];

/// Permission bits stored on a channel and on a custom role.
pub const PERMISSION_READ: i32 = 1;
pub const PERMISSION_WRITE: i32 = 2;
pub const PERMISSION_VOICE: i32 = 4;
pub const PERMISSION_STREAM: i32 = 8;
pub const PERMISSION_MANAGE: i32 = 16;
pub const PERMISSION_BITS: [(i32, &str); 5] = [
    (PERMISSION_READ, "Читать каналы"),
    (PERMISSION_WRITE, "Публиковать сообщения"),
    (PERMISSION_VOICE, "Участвовать в голосе"),
    (PERMISSION_STREAM, "Запускать трансляции"),
    (PERMISSION_MANAGE, "Управлять сообщениями"),
];

/// The resource a dialog is bound to. The Go client closes a settings dialog
/// as soon as the channel/server context no longer matches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub server: i64,
    pub channel: i64,
}

impl Scope {
    pub fn server(server: i64) -> Self {
        Self { server, channel: 0 }
    }

    pub fn channel(server: i64, channel: i64) -> Self {
        Self { server, channel }
    }

    /// `None` outside a concrete server; `Some(false)` for a server scope while
    /// the channel moved; `Some(true)` for an exact match.
    pub fn matches(&self, server: i64, channel: i64) -> Option<bool> {
        if self.server == 0 || server != self.server {
            return None;
        }
        if self.channel != 0 && channel != self.channel {
            return Some(false);
        }
        Some(true)
    }
}

/// A tab of one dialog. `label` and `icon` are the Go strings/resources.
#[derive(Clone, Copy, Debug)]
pub struct Tab {
    pub label: &'static str,
    /// Go draws the tab icon only when the dialog is at least 435 px wide.
    #[allow(dead_code)]
    pub icon: &'static str,
}

impl Tab {
    pub const fn new(label: &'static str, icon: &'static str) -> Self {
        Self { label, icon }
    }
}

/// Lazy tab lifecycle. A tab that was left while its request was still in
/// flight returns to `Idle`, so activating it again starts the request anew
/// instead of leaving the form on a permanent "Загрузка…" line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoadState {
    #[default]
    Idle,
    Loading,
    Loaded,
    Error,
}

/// Every new dialog instance takes a fresh value from this counter. Reopening
/// a form therefore never reuses the generation of an older answer, even when
/// the scope and the active tab are identical.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// One lazily opened dialog. `generation` invalidates every answer that was
/// started for a previous tab, scope or lifetime of the dialog.
#[derive(Debug)]
pub struct Dialog {
    pub title: String,
    pub scope: Scope,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub loads: Vec<LoadState>,
    pub generation: u64,
    pub pending: usize,
    pub error: String,
    closed: bool,
}

impl Dialog {
    pub fn new(title: impl Into<String>, scope: Scope, tabs: Vec<Tab>) -> Self {
        let loads = vec![LoadState::Idle; tabs.len()];
        Self {
            title: title.into(),
            scope,
            tabs,
            active: 0,
            loads,
            generation: next_generation(),
            pending: 0,
            error: String::new(),
            closed: false,
        }
    }

    /// Switch to a tab. Returns `true` when the tab must run its lazy `open`
    /// request. Leaving a tab with an unfinished request resets it to `Idle`
    /// so returning starts the request again. All older answers are discarded.
    pub fn activate(&mut self, index: usize) -> bool {
        if self.closed || index >= self.tabs.len() {
            return false;
        }
        if self.active == index {
            return false;
        }
        if self.loads[self.active] == LoadState::Loading {
            self.loads[self.active] = LoadState::Idle;
        }
        self.active = index;
        self.generation = next_generation();
        self.pending = 0;
        self.error.clear();
        match self.loads[index] {
            LoadState::Loaded | LoadState::Loading => false,
            LoadState::Idle | LoadState::Error => true,
        }
    }

    pub fn close(&mut self) -> bool {
        if self.closed {
            return false;
        }
        self.closed = true;
        self.generation = next_generation();
        self.pending = 0;
        true
    }

    #[cfg(test)]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn begin(&mut self) -> RequestToken {
        if let Some(slot) = self.loads.get_mut(self.active) {
            *slot = LoadState::Loading;
        }
        self.pending = self.pending.saturating_add(1);
        RequestToken {
            generation: self.generation,
            tab: self.active,
            scope: self.scope,
            pending: true,
        }
    }

    /// Finish one request. Returns `false` for an answer of a closed dialog or
    /// of a tab/scope that is no longer current; such an answer must not touch
    /// state.
    pub fn settle(&mut self, token: RequestToken) -> bool {
        if !self.closed
            && token.pending
            && token.generation == self.generation
            && token.tab == self.active
            && token.scope == self.scope
        {
            self.pending = self.pending.saturating_sub(1);
            // The tab is complete only after every request it started has
            // settled; a save or preview answer must not hide a pending load.
            if self.pending == 0 {
                if let Some(slot) = self.loads.get_mut(token.tab) {
                    *slot = LoadState::Loaded;
                }
            }
            true
        } else {
            false
        }
    }

    /// The dialog scope no longer exists in the current workspace.
    pub fn stale(&self, server: i64, channel: i64) -> bool {
        self.scope.matches(server, channel) != Some(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestToken {
    pub generation: u64,
    pub tab: usize,
    pub scope: Scope,
    pub pending: bool,
}

/// Confirmed value plus the value that was sent. `settle` only replaces the
/// editor when the user has not typed anything newer than the dispatched
/// value. This is the rule from the plan: a late answer does not overwrite a
/// local edit made after dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormValue<T> {
    pub snapshot: T,
    pub dispatched: Option<(u64, T)>,
}

impl<T: Clone + PartialEq> FormValue<T> {
    pub fn new(snapshot: T) -> Self {
        Self {
            snapshot,
            dispatched: None,
        }
    }

    pub fn load(&mut self, value: T) {
        self.snapshot = value;
        self.dispatched = None;
    }

    pub fn begin(&mut self, request: u64, current: T) -> T {
        self.dispatched = Some((request, current.clone()));
        current
    }

    /// Returns `true` when the reply was also written into the open editor.
    /// `current` is the live editor value at the time the answer arrived.
    pub fn settle(&mut self, request: u64, reply: T, current: &T) -> bool {
        let Some((sent_request, _)) = self.dispatched.as_ref() else {
            return false;
        };
        if *sent_request != request {
            return false;
        }
        let (_, sent_value) = self.dispatched.take().expect("checked above");
        self.snapshot = reply.clone();
        if current == &sent_value {
            true
        } else {
            false
        }
    }
}

/// Built-in server role. The backend roles are `owner`, `admin`, `moderator`
/// and `member`; an unknown value is treated as the least privileged one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Owner,
    Admin,
    Moderator,
    #[default]
    Member,
}

impl Role {
    pub fn parse(value: &str) -> Self {
        match value {
            "owner" => Self::Owner,
            "admin" => Self::Admin,
            "moderator" => Self::Moderator,
            _ => Self::Member,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Moderator => "moderator",
            Self::Member => "member",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Owner => "Владелец сервера",
            Self::Admin => "Администратор",
            Self::Moderator => "Модератор",
            Self::Member => "Участник",
        }
    }

    /// Go `Model.canManage`: the channel admin form and destructive channel
    /// actions are owner/admin only.
    pub fn can_manage_channels(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }

    /// The server settings dialog is owner-only in the Go client.
    pub fn can_manage_server(self) -> bool {
        self == Self::Owner
    }
}

/// One visible member action and the rule that decides whether it exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberAction {
    ViewProfile,
    BlockMessages,
    SetRole,
    Ban,
}

/// Visibility of the member context menu exactly as in
/// `internal/workspace/popup.go::openMemberMenu`.
pub fn member_action_visible(
    action: MemberAction,
    actor: Role,
    actor_id: i64,
    target_role: Role,
    target_id: i64,
    target_name: &str,
    current_user: &str,
) -> bool {
    let other = target_id > 0 && !target_name.eq_ignore_ascii_case(current_user);
    match action {
        MemberAction::ViewProfile => true,
        MemberAction::BlockMessages => other,
        MemberAction::SetRole => {
            actor == Role::Owner && other && matches!(target_role, Role::Member | Role::Admin)
        }
        MemberAction::Ban => {
            target_id != actor_id
                && target_role != Role::Owner
                && (actor == Role::Owner || (actor == Role::Admin && target_role == Role::Member))
        }
    }
}

/// One selectable built-in role in the member menu.
pub fn role_choices(target: Role) -> [(Role, bool); 2] {
    [
        (Role::Member, target == Role::Member),
        (Role::Admin, target == Role::Admin),
    ]
}

/// Storage plan selection. Changing the category or the period invalidates a
/// preview that was calculated for the previous pair.
#[derive(Debug)]
pub struct CleanupSelection {
    pub category: usize,
    pub period: usize,
    /// (category, period, plan) so a plan cannot survive a period change even
    /// when the computed `before` timestamp is close enough to look equal.
    pub plan: Option<(String, usize, CleanupPlanView)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupPlanView {
    pub category: String,
    pub before: i64,
    pub bytes: i64,
    pub count: i64,
    pub token: String,
}

impl Default for CleanupSelection {
    fn default() -> Self {
        Self {
            category: 0,
            // Go opens the storage tab on "Старше 30 дней".
            period: 2,
            plan: None,
        }
    }
}

impl CleanupSelection {
    pub fn category(&self) -> &'static str {
        CLEANUP_CATEGORIES[self.category.min(CLEANUP_CATEGORIES.len() - 1)]
    }

    pub fn choose_category(&mut self, index: usize) {
        if index < CLEANUP_CATEGORIES.len() && index != self.category {
            self.category = index;
            self.plan = None;
        }
    }

    pub fn choose_period(&mut self, index: usize) {
        if index < CLEANUP_PERIOD_DAYS.len() && index != self.period {
            self.period = index;
            self.plan = None;
        }
    }

    pub fn before(&self, now: i64) -> i64 {
        let days = CLEANUP_PERIOD_DAYS[self.period.min(CLEANUP_PERIOD_DAYS.len() - 1)];
        now.saturating_sub(days.saturating_mul(86_400))
    }

    /// Store a plan only for the pair it was requested for and only while the
    /// dialog is still the current one.
    pub fn apply(&mut self, category: &str, period: usize, plan: CleanupPlanView) -> bool {
        if category != self.category() || period != self.period {
            return false;
        }
        self.plan = Some((category.to_owned(), period, plan));
        true
    }

    /// A confirmed plan must still belong to the shown category/period.
    pub fn confirmed(&self) -> Option<&CleanupPlanView> {
        self.plan
            .as_ref()
            .filter(|(category, period, _)| category == self.category() && *period == self.period)
            .map(|(_, _, plan)| plan)
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Local date in the Go `formatDate` style: `d <месяц> yyyy г.`.
pub fn format_date(value: i64) -> String {
    const MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    let (year, month, day) = civil_from_days(value.div_euclid(86_400));
    format!(
        "{day} {} {year} г.",
        MONTHS[(month as usize).saturating_sub(1).min(11)]
    )
}

/// Days since 1970-01-01 to a Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

/// Go `storageSize`: two decimals in GiB.
pub fn storage_size(bytes: i64) -> String {
    format!("{:.2} ГБ", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Country {
    pub code: String,
    pub name: String,
}

/// Embedded copy of the Go client's `internal/workspace/countries.tsv`.
const COUNTRIES_TSV: &str = include_str!("../assets/countries.tsv");

pub fn countries() -> Vec<Country> {
    let mut values: Vec<Country> = COUNTRIES_TSV
        .lines()
        .filter_map(|line| {
            let (code, name) = line.split_once('\t')?;
            (!code.trim().is_empty()).then(|| Country {
                code: code.trim().to_owned(),
                name: name.trim().to_owned(),
            })
        })
        .collect();
    values.sort_by(|a, b| a.name.cmp(&b.name));
    values
}

/// `Не указана` followed by the country choices. The first entry clears the
/// country code.
pub fn country_labels(values: &[Country]) -> Vec<String> {
    let mut labels = Vec::with_capacity(values.len() + 1);
    labels.push("Не указана".to_owned());
    labels.extend(
        values
            .iter()
            .map(|country| format!("{} · {}", country.name, country.code)),
    );
    labels
}

pub fn country_index(values: &[Country], code: Option<&str>) -> usize {
    code.filter(|code| !code.is_empty())
        .and_then(|code| values.iter().position(|country| country.code == code))
        .map(|index| index + 1)
        .unwrap_or(0)
}

pub fn filter_countries(values: &[Country], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..values.len()).collect();
    }
    values
        .iter()
        .enumerate()
        .filter(|(_, country)| {
            country.name.to_lowercase().contains(&query)
                || country.code.to_lowercase().contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Validation of the profile and password fields, using the Go messages.
pub fn validate_about(value: &str) -> Result<(), &'static str> {
    if value.chars().count() > ABOUT_LIMIT {
        Err("Описание не должно превышать 512 символов.")
    } else {
        Ok(())
    }
}

pub fn validate_password(current: &str, next: &str, confirm: &str) -> Result<(), &'static str> {
    if current.is_empty() {
        return Err("Введите текущий пароль.");
    }
    if next.chars().count() < PASSWORD_MIN_CHARS || next.len() > PASSWORD_MAX_BYTES {
        return Err("Новый пароль должен содержать от 8 символов и не более 72 байт.");
    }
    if next != confirm {
        return Err("Пароли не совпадают.");
    }
    Ok(())
}

pub fn validate_slowmode(value: &str) -> Result<i64, &'static str> {
    match value.trim().parse::<i64>() {
        Ok(seconds) if (0..=SLOWMODE_MAX).contains(&seconds) => Ok(seconds),
        _ => Err("Укажите целое число от 0 до 21600."),
    }
}

/// Parse the quota editors (GiB with a comma allowed) and reject a total above
/// the instance capacity. Returns bytes per category in the Go order.
pub fn parse_quotas(
    values: [&str; 3],
    original_bytes: [i64; 3],
    original_text: [&str; 3],
    capacity: i64,
) -> Result<[i64; 3], &'static str> {
    let mut out = [0i64; 3];
    let mut total = 0i64;
    for index in 0..3 {
        let raw = values[index].trim();
        let number: f64 = raw
            .replace(',', ".")
            .parse()
            .map_err(|_| "Укажите допустимые лимиты в ГБ.")?;
        if !number.is_finite() || number < 0.0 || number > capacity as f64 / 1_073_741_824.0 {
            return Err("Укажите допустимые лимиты в ГБ.");
        }
        let mut bytes = (number * 1_073_741_824.0).round() as i64;
        if raw == original_text[index] {
            bytes = original_bytes[index];
        }
        if bytes > capacity.saturating_sub(total) {
            return Err("Сумма лимитов превышает доступное место.");
        }
        out[index] = bytes;
        total = total.saturating_add(bytes);
    }
    Ok(out)
}

/// Avatar crop with the Go client's coordinate space: `zoom` 0..100 (the crop
/// shrinks as zoom grows), `x`/`y` 0..100 percentages of the remaining space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub zoom: i32,
    pub x: i32,
    pub y: i32,
}

impl Default for Crop {
    fn default() -> Self {
        Self {
            zoom: 0,
            x: 50,
            y: 50,
        }
    }
}

impl Crop {
    pub fn clamped(self) -> Self {
        Self {
            zoom: self.zoom.clamp(0, 100),
            x: self.x.clamp(0, 100),
            y: self.y.clamp(0, 100),
        }
    }

    pub fn size(self, width: u32, height: u32) -> u32 {
        let crop = self.clamped();
        let side = width.min(height) as i64 * 100 / (100 + 3 * i64::from(crop.zoom));
        side.clamp(1, i64::from(width.min(height))) as u32
    }

    /// Top-left pixel of the square crop in EXIF-oriented space.
    pub fn origin(self, width: u32, height: u32) -> (u32, u32) {
        let crop = self.clamped();
        let side = i64::from(crop.size(width, height));
        let x = (i64::from(width) - side) * i64::from(crop.x) / 100;
        let y = (i64::from(height) - side) * i64::from(crop.y) / 100;
        (x.max(0) as u32, y.max(0) as u32)
    }
}

/// JPEG EXIF orientation (1..8), following the Go avatar crop code. PNG and
/// WebP answer `1`.
pub fn image_orientation(data: &[u8]) -> u16 {
    if data.len() < 2 || data[0] != 0xff || data[1] != 0xd8 {
        return 1;
    }
    let mut offset = 2usize;
    while offset + 4 <= data.len() && data[offset] == 0xff {
        let marker = data[offset + 1];
        if marker == 0xff {
            offset += 1;
            continue;
        }
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        let size = usize::from(u16::from_be_bytes([data[offset + 2], data[offset + 3]]));
        if size < 2 || size > data.len().saturating_sub(offset + 2) {
            break;
        }
        let segment = &data[offset + 4..offset + 2 + size];
        if marker == 0xe1 && segment.len() >= 14 && &segment[..6] == b"Exif\0\0" {
            return tiff_orientation(&segment[6..]);
        }
        offset += size + 2;
    }
    1
}

fn tiff_orientation(tiff: &[u8]) -> u16 {
    if tiff.len() < 8 {
        return 1;
    }
    let little = match &tiff[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return 1,
    };
    let u16_at = |at: usize| -> u16 {
        if little {
            u16::from_le_bytes([tiff[at], tiff[at + 1]])
        } else {
            u16::from_be_bytes([tiff[at], tiff[at + 1]])
        }
    };
    let u32_at = |at: usize| -> u32 {
        if little {
            u32::from_le_bytes([tiff[at], tiff[at + 1], tiff[at + 2], tiff[at + 3]])
        } else {
            u32::from_be_bytes([tiff[at], tiff[at + 1], tiff[at + 2], tiff[at + 3]])
        }
    };
    if u16_at(2) != 42 {
        return 1;
    }
    let start = u32_at(4) as usize;
    if start + 2 > tiff.len() {
        return 1;
    }
    let count = usize::from(u16_at(start));
    let mut entry = start + 2;
    for _ in 0..count {
        if entry + 12 > tiff.len() {
            break;
        }
        if u16_at(entry) == 0x0112 && u16_at(entry + 2) == 3 && u32_at(entry + 4) == 1 {
            let value = u16_at(entry + 8);
            return if (1..=8).contains(&value) { value } else { 1 };
        }
        entry += 12;
    }
    1
}

/// Orientation-aware dimensions.
#[cfg(test)]
pub fn oriented_size(width: u32, height: u32, orientation: u16) -> (u32, u32) {
    if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

/// Apply the EXIF orientation to a decoded image.
pub fn orient_image(image: image::DynamicImage, orientation: u16) -> image::DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

/// Go `notificationText`. Only events the account is allowed to see arrive on
/// this socket; the titles and bodies are built exactly like the reference.
pub fn notification_text(
    kind: &str,
    data: &serde_json::Value,
    current_user: &str,
) -> Option<(String, String, String)> {
    match kind {
        "conversation.notification" => {
            let id = data["id"].as_i64().unwrap_or_default();
            let conversation = data["conversation_id"]
                .as_i64()
                .or_else(|| data["conversation"]["id"].as_i64())
                .unwrap_or_default();
            let author = data["author"]["username"]
                .as_str()
                .or_else(|| data["sender"]["username"].as_str())
                .unwrap_or_default();
            let content = data["content"].as_str().unwrap_or_default();
            if id == 0
                || conversation == 0
                || author.eq_ignore_ascii_case(current_user)
                || content.trim().is_empty()
            {
                return None;
            }
            Some((
                format!("direct:{conversation}:{id}"),
                author.to_owned(),
                content.to_owned(),
            ))
        }
        "notification.message" => {
            let id = data["message"]["id"].as_i64().unwrap_or_default();
            let author = data["message"]["author"]["username"]
                .as_str()
                .unwrap_or_default();
            let content = data["message"]["content"].as_str().unwrap_or_default();
            if id == 0 || author.eq_ignore_ascii_case(current_user) || content.trim().is_empty() {
                return None;
            }
            let channel = data["channel_name"].as_str().unwrap_or_default();
            let server = data["server_name"].as_str().unwrap_or_default();
            Some((
                format!("channel:{id}"),
                format!("#{channel} · {server}"),
                format!("{author}: {content}"),
            ))
        }
        "community.event-reminder" => {
            let id = data["event_id"].as_i64().unwrap_or_default();
            let starts = data["starts_at"].as_i64().unwrap_or_default();
            let title = data["title"].as_str().unwrap_or_default();
            if id == 0 || title.trim().is_empty() {
                return None;
            }
            Some((
                format!("event:{id}:{starts}"),
                "Скоро событие".to_owned(),
                title.to_owned(),
            ))
        }
        "conversation.call" => {
            let id = data["conversation_id"].as_i64().unwrap_or_default();
            let members = data["members"].as_array()?;
            if members.iter().any(|member| {
                member["username"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(current_user))
            }) {
                return None;
            }
            let first = members.first()?;
            let user = first["username"].as_str()?;
            let session = first["session_id"].as_str().unwrap_or_default();
            Some((
                format!("call:{id}:{session}"),
                "Входящий звонок".to_owned(),
                format!("{user} приглашает в разговор"),
            ))
        }
        _ => None,
    }
}

/// The bounded set of already shown notifications (Go keeps 128).
#[derive(Default, Debug)]
pub struct SeenNotifications {
    keys: Vec<String>,
}

impl SeenNotifications {
    pub fn contains(&self, key: &str) -> bool {
        self.keys.iter().any(|value| value == key)
    }

    pub fn remember(&mut self, key: &str) {
        if self.contains(key) {
            return;
        }
        if self.keys.len() >= 128 {
            self.keys.remove(0);
        }
        self.keys.push(key.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> Dialog {
        Dialog::new(
            "Настройки",
            Scope::server(4),
            vec![
                Tab::new("Профиль", "people"),
                Tab::new("Приватность", "lock"),
            ],
        )
    }

    #[test]
    fn tabs_open_lazily_and_invalidate_older_requests() {
        let mut dialog = dialog();
        assert_eq!(dialog.loads[0], LoadState::Idle);
        assert_eq!(dialog.loads[1], LoadState::Idle);
        let profile = dialog.begin();
        assert_eq!(dialog.loads[0], LoadState::Loading);
        assert!(dialog.settle(profile));
        assert_eq!(dialog.loads[0], LoadState::Loaded);
        assert!(dialog.activate(1));
        assert!(!dialog.settle(profile), "old tab answer is discarded");
        assert_eq!(dialog.pending, 0);
        assert!(!dialog.activate(1), "already active");
        assert_eq!(dialog.loads[1], LoadState::Idle);
        let privacy = dialog.begin();
        assert!(dialog.settle(privacy));
        assert_eq!(dialog.loads[1], LoadState::Loaded);
        assert!(!dialog.activate(1), "loaded tab is not reopened");
    }

    #[test]
    fn leaving_a_loading_tab_makes_the_return_reload_it() {
        let mut dialog = dialog();
        let profile = dialog.begin();
        assert_eq!(dialog.loads[0], LoadState::Loading);
        assert!(dialog.activate(1));
        assert_eq!(
            dialog.loads[0],
            LoadState::Idle,
            "unfinished request is reset on leave"
        );
        assert!(dialog.activate(0), "returning starts the request again");
        assert!(
            !dialog.settle(profile),
            "the interrupted answer is discarded"
        );
    }

    #[test]
    fn a_reopened_dialog_never_reuses_a_generation_or_accepts_another_scope() {
        let mut first = Dialog::new(
            "Настройки",
            Scope::channel(4, 9),
            vec![Tab::new("Доступ", "lock")],
        );
        let old = first.begin();
        first.close();
        let mut second = Dialog::new(
            "Настройки",
            Scope::channel(4, 9),
            vec![Tab::new("Доступ", "lock")],
        );
        assert_ne!(first.generation, second.generation);
        assert!(!second.settle(old), "an old dialog instance is rejected");

        let mut other = dialog();
        let token = other.begin();
        other.scope = Scope::channel(4, 10);
        assert!(!other.settle(token), "a foreign scope is rejected");
    }

    #[test]
    fn closing_drops_answers_and_pending() {
        let mut dialog = dialog();
        let token = dialog.begin();
        assert!(dialog.close());
        assert!(!dialog.settle(token));
        assert!(dialog.close() == false);
        assert_eq!(dialog.pending, 0);
    }

    #[test]
    fn scope_validation_matches_the_go_close_rule() {
        assert_eq!(Scope::server(4).matches(4, 9), Some(true));
        assert_eq!(Scope::server(4).matches(5, 9), None);
        assert_eq!(Scope::channel(4, 9).matches(4, 8), Some(false));
        assert_eq!(Scope::channel(4, 9).matches(4, 9), Some(true));
        assert!(dialog().stale(4, 9) == false);
        assert!(dialog().stale(5, 9));
    }

    #[test]
    fn late_reply_does_not_overwrite_a_local_edit() {
        let mut form = FormValue::new("old".to_string());
        let sent = form.begin(7, form.snapshot.clone());
        assert_eq!(sent, "old");
        let edited = "typed after dispatch".to_string();
        assert!(!form.settle(7, "server answer".to_string(), &edited));
        assert_eq!(form.snapshot, "server answer");
        // A second, unchanged editor does receive the reply.
        let mut form = FormValue::new("old".to_string());
        let sent = form.begin(8, "old".to_string());
        assert!(form.settle(8, "new".to_string(), &sent));
        assert_eq!(form.snapshot, "new");
    }

    #[test]
    fn mismatched_request_does_not_touch_the_form() {
        let mut form = FormValue::new(1);
        form.begin(3, 1);
        assert!(!form.settle(4, 2, &1));
        assert_eq!(form.snapshot, 1);
        assert_eq!(form.dispatched, Some((3, 1)));
    }

    #[test]
    fn roles_gate_the_same_actions_as_go() {
        assert!(Role::Owner.can_manage_server());
        assert!(!Role::Admin.can_manage_server());
        assert!(Role::Admin.can_manage_channels());
        assert!(!Role::Moderator.can_manage_channels());
        assert!(member_action_visible(
            MemberAction::Ban,
            Role::Admin,
            1,
            Role::Member,
            2,
            "neva",
            "admin"
        ));
        assert!(!member_action_visible(
            MemberAction::Ban,
            Role::Admin,
            1,
            Role::Admin,
            2,
            "neva",
            "admin"
        ));
        assert!(!member_action_visible(
            MemberAction::SetRole,
            Role::Admin,
            1,
            Role::Member,
            2,
            "neva",
            "admin"
        ));
        assert!(member_action_visible(
            MemberAction::SetRole,
            Role::Owner,
            1,
            Role::Member,
            2,
            "neva",
            "owner"
        ));
    }

    #[test]
    fn cleanup_plan_belongs_to_one_category_and_period() {
        let mut selection = CleanupSelection::default();
        let period = selection.period;
        let plan = CleanupPlanView {
            category: "images".into(),
            before: selection.before(now()),
            bytes: 10,
            count: 2,
            token: "t".into(),
        };
        assert!(selection.apply("images", period, plan.clone()));
        assert_eq!(selection.confirmed(), Some(&plan));
        assert!(!selection.apply("images", period + 1, plan.clone()));
        selection.choose_period(1);
        assert!(selection.confirmed().is_none());
    }

    #[test]
    fn quota_parsing_keeps_unchanged_values_and_rejects_overflow() {
        let original = [2_i64 << 30, 3_i64 << 30, 4_i64 << 30];
        let text = ["2.000", "3.000", "4.000"];
        let capacity = 20_i64 << 30;
        assert_eq!(
            parse_quotas(["2.000", "3.000", "4.000"], original, text, capacity),
            Ok(original)
        );
        assert_eq!(
            parse_quotas(["1,5", "3.000", "4.000"], original, text, capacity),
            Ok([
                (1.5_f64 * 1_073_741_824.0).round() as i64,
                original[1],
                original[2]
            ])
        );
        assert_eq!(
            parse_quotas(["10", "10", "4.000"], original, text, capacity),
            Err("Сумма лимитов превышает доступное место.")
        );
    }

    #[test]
    fn avatar_crop_follows_go_math() {
        let crop = Crop::default();
        assert_eq!(crop.size(200, 100), 100);
        assert_eq!(crop.origin(200, 100), (50, 0));
        let narrow = Crop {
            zoom: 100,
            x: 0,
            y: 100,
        };
        assert_eq!(narrow.size(200, 100), 25);
        assert_eq!(narrow.origin(200, 100), (0, 75));
    }

    #[test]
    fn exif_orientation_is_parsed_from_a_minimal_jpeg() {
        // SOI + APP1 "Exif\0\0" + little-endian TIFF with orientation 6.
        let mut data = vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x22];
        data.extend_from_slice(b"Exif\0\0");
        data.extend_from_slice(b"II*\0\x08\0\0\0");
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&0x0112u16.to_le_bytes());
        data.extend_from_slice(&3u16.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&6u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(image_orientation(&data), 6);
        assert_eq!(oriented_size(200, 100, 6), (100, 200));
        assert_eq!(image_orientation(b"not a jpeg"), 1);
    }

    #[test]
    fn country_choice_search_and_empty_option() {
        let values = countries();
        assert!(values.len() > 200);
        assert_eq!(country_labels(&values)[0], "Не указана");
        let ru = values.iter().position(|c| c.code == "RU").unwrap();
        assert_eq!(country_index(&values, Some("RU")), ru + 1);
        assert_eq!(country_index(&values, Some("")), 0);
        assert!(!filter_countries(&values, "рос").is_empty());
        assert!(filter_countries(&values, "zz").is_empty());
        assert_eq!(filter_countries(&values, "").len(), values.len());
    }

    #[test]
    fn notifications_are_built_and_ignored_like_go() {
        let json = serde_json::json!({
            "channel_id": 3,
            "channel_name": "qa-chat",
            "server_name": "QA",
            "message": {"id": 9, "content": "hello", "author": {"username": "neva"}}
        });
        assert_eq!(
            notification_text("notification.message", &json, "owner"),
            Some((
                "channel:9".into(),
                "#qa-chat · QA".into(),
                "neva: hello".into()
            ))
        );
        assert_eq!(
            notification_text("notification.message", &json, "neva"),
            None
        );
        let dm = serde_json::json!({
            "id": 4, "conversation_id": 8, "content": "hi",
            "author": {"username": "neva"}
        });
        assert_eq!(
            notification_text("conversation.notification", &dm, "owner"),
            Some(("direct:8:4".into(), "neva".into(), "hi".into()))
        );
        let mut seen = SeenNotifications::default();
        seen.remember("direct:8:4");
        assert!(seen.contains("direct:8:4"));
        for index in 0..200 {
            seen.remember(&format!("key:{index}"));
        }
        assert!(seen.keys.len() <= 128);
    }

    #[test]
    fn validation_messages_match_go() {
        assert_eq!(validate_about(&"a".repeat(512)), Ok(()));
        assert_eq!(
            validate_about(&"😀".repeat(513)),
            Err("Описание не должно превышать 512 символов.")
        );
        assert_eq!(
            validate_password("", "12345678", "12345678"),
            Err("Введите текущий пароль.")
        );
        assert_eq!(
            validate_password("old", "short", "short"),
            Err("Новый пароль должен содержать от 8 символов и не более 72 байт.")
        );
        assert_eq!(
            validate_password("old", "12345678", "12345679"),
            Err("Пароли не совпадают.")
        );
        assert_eq!(validate_slowmode("0"), Ok(0));
        assert_eq!(validate_slowmode("21600"), Ok(21600));
        assert!(validate_slowmode("21601").is_err());
        assert_eq!(storage_size(1 << 30), "1.00 ГБ");
    }
}
