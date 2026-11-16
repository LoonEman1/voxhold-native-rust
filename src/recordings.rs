//! Pure server snapshot for «Записи сервера» (Go W01).
//!
//! The snapshot belongs to the selected server; the modal owns only its scroll,
//! confirm/busy state and preview cache in `recordings_dialog`. Every network
//! callback checks `(server, access_epoch, request)` before touching either
//! owner. Tombstones are final for the session, so a late list never
//! resurrects a deleted row.

use std::collections::{HashMap, HashSet};

use voxhold_api::{
    Recording, RECORDING_DELETED, RECORDING_READY, RECORDING_STARTED, RECORDING_UPLOADING,
};

pub const ROW_LIMIT: usize = 100;
pub const CARD_GAP: f32 = 12.0;
pub const CARD_PADDING: f32 = 14.0;
pub const PREVIEW_WIDTH: f32 = 132.0;
pub const PREVIEW_HEIGHT: f32 = 74.0;
pub const PREVIEW_BUDGET: usize = 4 << 20;
pub const PREVIEW_PARALLEL: usize = 2;
pub const RETRY_WIDTH: f32 = 88.0;
pub const RETRY_HEIGHT: f32 = 32.0;
pub const DISMISS_WIDTH: f32 = 98.0;
pub const DISMISS_HEIGHT: f32 = 38.0;
pub(crate) const LIST_SHARE: f32 = 0.46;
const RETRY_BUDGET: u8 = 3;

/// The inputs of one list request. The app starts a new request only after it
/// has read the current server and access epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListRequest {
    pub server_id: i64,
    pub epoch: u64,
    pub request: u64,
    pub event_seq: u64,
    pub attempt: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListOutcome {
    /// The answer was applied (or the request no longer owns the state).
    Applied,
    Ignored,
    /// An event or an access change is newer than this answer: fetch again.
    Retry,
}

#[derive(Clone, Debug, Default)]
pub struct RecordingsState {
    pub server_id: i64,
    pub records: Vec<Recording>,
    pub quota_bytes: i64,
    pub max_file_bytes: i64,
    pub retention_seconds: i64,
    pub loading: bool,
    pub error: String,
    /// A list was requested at least once, so a reconnect should refresh it.
    pub loaded: bool,
    /// The access generation currently on screen.
    pub access_epoch: u64,
    /// The generation whose authoritative list is on screen. `None` means the
    /// current access has not been confirmed by a successful list yet, so live
    /// events must not make a visible snapshot out of unverified rows.
    pub confirmed_epoch: Option<u64>,
    pub event_seq: u64,
    pub touched: HashMap<String, u64>,
    pub tombstones: HashSet<String>,
    request_seq: u64,
    pub active: Option<ListRequest>,
}

impl RecordingsState {
    pub fn reset(&mut self) {
        self.server_id = 0;
        self.records.clear();
        self.quota_bytes = 0;
        self.max_file_bytes = 0;
        self.retention_seconds = 0;
        self.loading = false;
        self.error.clear();
        self.loaded = false;
        self.access_epoch = 0;
        self.confirmed_epoch = None;
        self.event_seq = self.event_seq.wrapping_add(1);
        self.touched.clear();
        self.tombstones.clear();
        self.request_seq = self.request_seq.wrapping_add(1);
        self.active = None;
    }

    /// A ready/reconnect/access event unconfirms every card until a fresh list
    /// answers. The rows stay in memory but [`Self::sorted`] hides them.
    pub fn set_access_epoch(&mut self, epoch: u64) {
        self.access_epoch = epoch;
    }

    pub fn begin_refresh(&mut self, server_id: i64, epoch: u64, attempt: u8) -> ListRequest {
        self.request_seq = self.request_seq.wrapping_add(1);
        let request = ListRequest {
            server_id,
            epoch,
            request: self.request_seq,
            event_seq: self.event_seq,
            attempt,
        };
        self.server_id = server_id;
        self.access_epoch = epoch;
        self.active = Some(request);
        self.loading = true;
        self.loaded = true;
        self.error.clear();
        request
    }

    pub fn apply_list(
        &mut self,
        result: std::result::Result<voxhold_api::RecordingList, voxhold_api::Error>,
        request: ListRequest,
        epoch: u64,
        now: i64,
    ) -> ListOutcome {
        if self.active != Some(request) || self.server_id != request.server_id {
            return ListOutcome::Ignored;
        }
        if request.epoch != epoch || request.epoch != self.access_epoch {
            self.active = None;
            self.loading = false;
            return ListOutcome::Retry;
        }
        if request.event_seq != self.event_seq {
            if request.attempt < RETRY_BUDGET {
                self.active = None;
                self.loading = false;
                return ListOutcome::Retry;
            }
            // The retry budget is spent: merge what the events already applied
            // instead of rolling the rows back to the older HTTP snapshot.
        }
        self.active = None;
        self.loading = false;
        match result {
            Ok(list) => {
                self.error.clear();
                if list.quota_bytes > 0 {
                    self.quota_bytes = list.quota_bytes;
                }
                if list.max_file_bytes > 0 {
                    self.max_file_bytes = list.max_file_bytes;
                }
                if list.retention_seconds > 0 {
                    self.retention_seconds = list.retention_seconds;
                }
                self.apply_rows(list.recordings, request.event_seq, now);
                self.confirmed_epoch = Some(request.epoch);
                ListOutcome::Applied
            }
            Err(error) => {
                self.error = crate::draft::describe_error(&error);
                if matches!(error.status(), Some(403 | 404)) {
                    // An access answer revokes the confirmation itself. A late
                    // `recording.updated` must not refill the visible snapshot
                    // without a successful GET in this generation. Tombstones
                    // stay until the whole server model is reset.
                    self.confirmed_epoch = None;
                    self.records.clear();
                    self.touched.clear();
                }
                ListOutcome::Applied
            }
        }
    }

    fn apply_rows(&mut self, records: Vec<Recording>, since: u64, now: i64) {
        let existing: HashMap<String, Recording> = self
            .records
            .iter()
            .map(|record| (record.id.clone(), record.clone()))
            .collect();
        let mut out = Vec::with_capacity(records.len());
        let mut seen = HashSet::new();
        for mut record in records {
            if record.id.is_empty()
                || record.server_id != self.server_id
                || self.tombstones.contains(&record.id)
                || expired(&record, now)
            {
                continue;
            }
            if let Some(old) = existing.get(&record.id) {
                if old.updated_at > record.updated_at {
                    record = old.clone();
                }
            }
            seen.insert(record.id.clone());
            out.push(record);
        }
        for (id, record) in existing {
            if seen.contains(&id)
                || self.tombstones.contains(&id)
                || record.server_id != self.server_id
            {
                continue;
            }
            if self.touched.get(&id).is_some_and(|at| *at > since) {
                out.push(record);
            }
        }
        self.touched.retain(|_, at| *at > since);
        out.sort_by(|a, b| a.id.cmp(&b.id));
        self.records = out;
    }

    /// Merge one live `recording.updated`. Newer revision wins; a tombstone is
    /// final for the rest of the session.
    pub fn apply_event(&mut self, record: Recording, now: i64) -> bool {
        if record.id.is_empty() || record.server_id != self.server_id {
            return false;
        }
        self.event_seq = self.event_seq.wrapping_add(1);
        let seq = self.event_seq;
        self.touched.insert(record.id.clone(), seq);
        if record.status == RECORDING_DELETED {
            self.tombstones.insert(record.id.clone());
            self.remove(&record.id);
            return true;
        }
        if self.tombstones.contains(&record.id) {
            return false;
        }
        if expired(&record, now) {
            self.remove(&record.id);
            return true;
        }
        match self.records.iter().position(|row| row.id == record.id) {
            Some(index) => {
                if self.records[index].updated_at > record.updated_at {
                    return false;
                }
                self.records[index] = record;
            }
            None => self.records.push(record),
        }
        true
    }

    fn remove(&mut self, id: &str) {
        self.records.retain(|record| record.id != id);
    }

    /// Live rows first, then newest `created_at`, then id descending — the
    /// server's order recomputed after events.
    pub fn sorted(&self, now: i64) -> Vec<Recording> {
        if self.confirmed_epoch != Some(self.access_epoch) {
            return Vec::new();
        }
        let mut rows: Vec<Recording> = self
            .records
            .iter()
            .filter(|record| !expired(record, now))
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            let live = a.status == RECORDING_STARTED;
            let other = b.status == RECORDING_STARTED;
            live.cmp(&other)
                .reverse()
                .then_with(|| a.created_at.cmp(&b.created_at).reverse())
                .then_with(|| a.id.cmp(&b.id).reverse())
        });
        rows.truncate(ROW_LIMIT);
        rows
    }

    pub fn live(&self, channel_id: i64, now: i64) -> Option<Recording> {
        if channel_id == 0 || self.confirmed_epoch != Some(self.access_epoch) {
            return None;
        }
        self.records
            .iter()
            .find(|record| {
                record.channel_id == channel_id
                    && record.status == RECORDING_STARTED
                    && !expired(record, now)
            })
            .cloned()
    }

    /// The owner sees «Удалить с сервера»; an administrator role alone does
    /// not add the button.
    pub fn owns(&self, record: &Recording, self_user_id: i64, username: &str) -> bool {
        if record.owner_user_id == 0 {
            return false;
        }
        if self_user_id != 0 {
            return record.owner_user_id == self_user_id;
        }
        !username.is_empty() && record.owner_username.eq_ignore_ascii_case(username)
    }

    pub fn hint(&self) -> String {
        let mut text = "Файл скачивается после остановки записи.".to_string();
        if self.quota_bytes > 0 {
            text.push_str(" Лимит хранилища на сервере — ");
            text.push_str(&size(self.quota_bytes));
            text.push('.');
        }
        text.push_str(" Копии доступны участникам ");
        text.push_str(&retention_label(self.retention_seconds));
        text.push('.');
        text
    }
}

pub fn expired(record: &Recording, now: i64) -> bool {
    record.expires_at > 0 && record.expires_at <= now && record.status != RECORDING_STARTED
}

pub fn ready(record: &Recording, now: i64) -> bool {
    record.status == RECORDING_READY && !expired(record, now)
}

pub fn live(record: &Recording, now: i64) -> bool {
    record.status == RECORDING_STARTED && !expired(record, now)
}

pub fn status_text(record: &Recording, now: i64) -> String {
    if record.status == RECORDING_DELETED {
        "Серверная копия недоступна".into()
    } else if live(record, now) {
        "Идёт запись".into()
    } else if ready(record, now) {
        format!("На сервере до {}", date(record.expires_at))
    } else if record.status == RECORDING_UPLOADING && !expired(record, now) {
        "Загружается на сервер".into()
    } else if expired(record, now) {
        "Срок серверной копии истёк".into()
    } else {
        "Серверная копия недоступна".into()
    }
}

pub fn size(bytes: i64) -> String {
    if bytes <= 0 {
        return "0 МБ".into();
    }
    if bytes >= 1 << 30 {
        format!("{:.1} ГБ", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.1} МБ", bytes as f64 / (1u64 << 20) as f64)
    }
}

pub fn duration(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}

/// The shared retention window in Russian, as Go `recordingRetentionLabel`.
pub fn retention_label(seconds: i64) -> String {
    let seconds = if seconds <= 0 { 24 * 3600 } else { seconds };
    let hours = seconds / 3600;
    if hours <= 0 {
        return "менее часа".into();
    }
    let unit = if hours % 100 < 11 || hours % 100 > 14 {
        if hours % 10 == 1 {
            "час"
        } else if (2..=4).contains(&(hours % 10)) {
            "часа"
        } else {
            "часов"
        }
    } else {
        "часов"
    };
    format!("{hours} {unit}")
}

pub fn date(seconds: i64) -> String {
    if seconds <= 0 {
        return String::new();
    }
    let (year, month, day, hour, minute) = local_parts(seconds);
    format!("{day:02}.{month:02}.{year:04}, {hour:02}:{minute:02}")
}

fn utc_parts(seconds: i64) -> (i64, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        month,
        day,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
    )
}

/// Go formats the card date in local time. Windows converts the UTC fields
/// with `SystemTimeToTzSpecificLocalTime`; other targets fall back to UTC.
#[cfg(windows)]
fn local_parts(seconds: i64) -> (i64, u32, u32, u32, u32) {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            zone: *const core::ffi::c_void,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
    }
    let (year, month, day, hour, minute) = utc_parts(seconds);
    let utc = SystemTime {
        year: year.clamp(0, u16::MAX as i64) as u16,
        month: month as u16,
        day: day as u16,
        hour: hour as u16,
        minute: minute as u16,
        ..SystemTime::default()
    };
    let mut local = SystemTime::default();
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
    if ok == 0 {
        return utc_parts(seconds);
    }
    (
        local.year as i64,
        local.month as u32,
        local.day as u32,
        local.hour as u32,
        local.minute as u32,
    )
}

#[cfg(not(windows))]
fn local_parts(seconds: i64) -> (i64, u32, u32, u32, u32) {
    utc_parts(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recordings_dialog::*;
    use voxhold_api::{Recording, RecordingList};

    fn record(id: &str, status: &str, updated: i64) -> Recording {
        Recording {
            id: id.into(),
            server_id: 1,
            channel_id: 7,
            owner_user_id: 3,
            owner_username: "alice".into(),
            status: status.into(),
            mime_type: "video/mp4".into(),
            size_bytes: 1024,
            duration_ms: 61_500,
            created_at: 1_700_000_000,
            updated_at: updated,
            expires_at: 1_800_000_000,
            preview_version: Some("etag-1234".into()),
        }
    }

    fn ready_state() -> RecordingsState {
        let mut state = RecordingsState::default();
        let request = state.begin_refresh(1, 5, 0);
        state.apply_list(
            Ok(RecordingList {
                recordings: vec![
                    record(&"a".repeat(32), RECORDING_READY, 1),
                    record(&"b".repeat(32), RECORDING_STARTED, 2),
                ],
                ..RecordingList::default()
            }),
            request,
            5,
            1_700_000_100,
        );
        state
    }

    #[test]
    fn recordings_retry_keeps_newer_event() {
        let mut state = RecordingsState::default();
        let id = "a".repeat(32);
        for attempt in 0..=RETRY_BUDGET {
            let request = state.begin_refresh(1, 1, attempt);
            state.apply_event(
                record(&id, RECORDING_READY, 4 + attempt as i64),
                1_700_000_100,
            );
            let outcome = state.apply_list(
                Ok(RecordingList {
                    recordings: vec![record(&id, RECORDING_STARTED, 1)],
                    ..RecordingList::default()
                }),
                request,
                1,
                1_700_000_100,
            );
            if attempt < RETRY_BUDGET {
                assert_eq!(outcome, ListOutcome::Retry);
            }
        }
        let rows = state.sorted(1_700_000_100);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, RECORDING_READY);
        assert_eq!(rows[0].updated_at, 4 + RETRY_BUDGET as i64);
        assert!(state.live(7, 1_700_000_100).is_none());
    }

    #[test]
    fn recordings_hide_metadata_while_access_is_unconfirmed() {
        let mut state = ready_state();
        assert_eq!(state.sorted(1_700_000_100).len(), 2);
        assert!(state.live(7, 1_700_000_100).is_some());
        state.set_access_epoch(6);
        assert!(state.sorted(1_700_000_100).is_empty());
        assert!(state.live(7, 1_700_000_100).is_none());
        let request = state.begin_refresh(1, 6, 0);
        state.apply_list(
            Err(voxhold_api::Error::Http(503)),
            request,
            6,
            1_700_000_100,
        );
        assert!(state.sorted(1_700_000_100).is_empty());
        assert!(state.live(7, 1_700_000_100).is_none());
    }

    #[test]
    fn access_error_unconfirms_the_snapshot_and_keeps_tombstones() {
        let mut state = ready_state();
        let deleted = "a".repeat(32);
        state.apply_event(record(&deleted, RECORDING_DELETED, 9), 1_700_000_100);
        assert!(state.tombstones.contains(&deleted));

        let request = state.begin_refresh(1, 5, 0);
        state.apply_list(
            Err(voxhold_api::Error::Http(404)),
            request,
            5,
            1_700_000_100,
        );
        assert_eq!(state.confirmed_epoch, None);
        assert!(state.records.is_empty());
        assert_eq!(state.sorted(1_700_000_100).len(), 0);
        assert!(state.live(7, 1_700_000_100).is_none());
        assert!(
            state.tombstones.contains(&deleted),
            "an access error must not let a late list resurrect the row"
        );

        // A late live event cannot refill the unconfirmed snapshot.
        assert!(state.apply_event(record(&"b".repeat(32), RECORDING_READY, 10), 1_700_000_100));
        assert!(state.sorted(1_700_000_100).is_empty());

        let request = state.begin_refresh(1, 5, 1);
        state.apply_list(
            Ok(RecordingList {
                recordings: vec![
                    record(&deleted, RECORDING_READY, 11),
                    record(&"b".repeat(32), RECORDING_READY, 10),
                ],
                ..RecordingList::default()
            }),
            request,
            5,
            1_700_000_100,
        );
        let ids: Vec<String> = state
            .sorted(1_700_000_100)
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, vec!["b".repeat(32)]);
    }

    #[test]
    fn a_failed_manual_refresh_keeps_a_confirmed_snapshot_visible() {
        let mut state = ready_state();
        let request = state.begin_refresh(1, 5, 1);
        state.apply_list(
            Err(voxhold_api::Error::Http(503)),
            request,
            5,
            1_700_000_100,
        );
        assert_eq!(state.confirmed_epoch, Some(5));
        assert_eq!(state.sorted(1_700_000_100).len(), 2);
        assert!(state.live(7, 1_700_000_100).is_some());
    }

    #[test]
    fn tombstone_is_final_and_expiry_filters_rows() {
        let mut state = ready_state();
        let id = "a".repeat(32);
        state.apply_event(record(&id, RECORDING_DELETED, 9), 1_700_000_100);
        assert_eq!(state.sorted(1_700_000_100).len(), 1);
        let request = state.begin_refresh(1, 5, 0);
        state.apply_list(
            Ok(RecordingList {
                recordings: vec![record(&id, RECORDING_READY, 10)],
                ..RecordingList::default()
            }),
            request,
            5,
            1_700_000_100,
        );
        assert!(!state.sorted(1_700_000_100).iter().any(|r| r.id == id));
        let mut expiring = record(&"c".repeat(32), RECORDING_READY, 1);
        expiring.expires_at = 1_700_000_050;
        let request = state.begin_refresh(1, 5, 0);
        state.apply_list(
            Ok(RecordingList {
                recordings: vec![expiring],
                ..RecordingList::default()
            }),
            request,
            5,
            1_700_000_100,
        );
        assert!(state.sorted(1_700_000_100).is_empty());
    }

    #[test]
    fn sorting_puts_live_first_and_breaks_ties_by_id() {
        let mut state = RecordingsState::default();
        let request = state.begin_refresh(1, 1, 0);
        let mut older = record(&"a".repeat(32), RECORDING_READY, 1);
        older.created_at = 100;
        let mut newer = record(&"b".repeat(32), RECORDING_READY, 1);
        newer.created_at = 200;
        let mut live = record(&"c".repeat(32), RECORDING_STARTED, 1);
        live.created_at = 50;
        state.apply_list(
            Ok(RecordingList {
                recordings: vec![older, newer, live],
                ..RecordingList::default()
            }),
            request,
            1,
            1_700_000_100,
        );
        let ids: Vec<String> = state
            .sorted(1_700_000_100)
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec!["c".repeat(32), "b".repeat(32), "a".repeat(32)]);
    }

    #[test]
    fn card_geometry_matches_the_web_breakpoints() {
        let ready = record(&"a".repeat(32), RECORDING_READY, 1);
        assert!(stacked(359.));
        assert!(!stacked(360.));
        let wide = row_height(&ready, 520., 1_700_000_100, false);
        let narrow = row_height(&ready, 300., 1_700_000_100, false);
        assert!(narrow > wide);
        let confirm = row_height(&ready, 520., 1_700_000_100, true);
        assert!(confirm >= wide + 40.);
        let hint_lines = wrap_lines(&"слово ".repeat(80), 520., 12.).len();
        let geometry = dialog_geometry(1440., 900., hint_lines, &[wide, wide, wide]);
        assert_eq!(geometry.bounds.w, 570.);
        assert!(geometry.list.h <= 900. * 0.46 + 0.1);
        assert!(geometry.max_scroll > 0.);
    }

    fn three_ready_cards() -> RecordingsState {
        let mut state = ready_state();
        state.records = (1..=3)
            .map(|n| record(&n.to_string().repeat(32), RECORDING_READY, n as i64))
            .collect();
        state
    }

    #[test]
    fn recording_enter_on_clipped_action_waits_for_scroll() {
        let state = three_ready_cards();
        let mut dialog = DialogState::new(1, 1, 5);
        dialog.layout(&state, 390., 760., 1_700_000_100, 3);
        assert!(dialog.max_scroll > 0.);
        // The third row is below the viewport: Enter there only asks for a
        // scroll, and the next Enter reaches the now visible action.
        let key = format!("rec-download-{}", "1".repeat(32));
        assert!(!dialog.action_visible(&key));
        dialog.scroll_to = Some(key.clone());
        assert!(!dialog.activate_key(&key));
        assert_eq!(dialog.scroll_to.as_deref(), Some(key.as_str()));
        dialog.layout(&state, 390., 760., 1_700_000_100, 3);
        assert!(dialog.scroll_to.is_none());
        assert!(dialog.action_visible(&key));
        assert!(dialog.activate_key(&key));
    }

    #[test]
    fn recording_wheel_after_click_scrolls_to_list_end() {
        let state = three_ready_cards();
        let mut dialog = DialogState::new(1, 1, 5);
        dialog.layout(&state, 390., 760., 1_700_000_100, 3);
        // A focus change asks for one scroll; the wheel takes over and drops
        // the pending jump so a later layout cannot pull the list back.
        let key = format!("rec-delete-{}", "1".repeat(32));
        dialog.scroll_to = Some(key);
        dialog.scroll = 0.;
        dialog.wheel(42.);
        assert!(dialog.scroll_to.is_none());
        assert_eq!(dialog.scroll, 42.);
        let max = dialog.max_scroll;
        dialog.wheel(max);
        assert_eq!(dialog.scroll, max);
        dialog.layout(&state, 390., 760., 1_700_000_100, 3);
        assert_eq!(dialog.scroll, dialog.max_scroll);
    }

    #[test]
    fn recording_release_in_clipped_part_cancels_press() {
        let state = three_ready_cards();
        let mut dialog = DialogState::new(1, 1, 5);
        dialog.layout(&state, 390., 760., 1_700_000_100, 3);
        let key = format!("rec-download-{}", "1".repeat(32));
        let rect = dialog.action_rect(&key).unwrap();
        dialog.scroll = (rect.center().1 - dialog.list.bottom()).clamp(0., dialog.max_scroll);
        dialog.layout(&state, 390., 760., 1_700_000_100, 3);
        let rect = dialog.action_rect(&key).unwrap();
        assert!(
            rect.y < dialog.list.bottom() && rect.bottom() > dialog.list.bottom(),
            "the action must straddle the list edge: {rect:?} / {:?}",
            dialog.list
        );
        let press = (rect.center().0, rect.y + 2.);
        let release = (rect.center().0, rect.bottom() - 2.);
        assert!(dialog.action_hit(&key, press));
        assert!(!dialog.action_hit(&key, release));
    }

    #[test]
    fn hint_and_status_text_follow_go() {
        let mut state = RecordingsState {
            quota_bytes: 6 << 30,
            retention_seconds: 21 * 3600,
            ..RecordingsState::default()
        };
        state.access_epoch = 1;
        state.confirmed_epoch = Some(1);
        assert_eq!(size(6 << 30), "6.0 ГБ");
        assert_eq!(size(1536 << 20), "1.5 ГБ");
        assert_eq!(duration(61_500), "1:01");
        assert_eq!(retention_label(3600), "1 час");
        assert_eq!(retention_label(2 * 3600), "2 часа");
        assert_eq!(retention_label(5 * 3600), "5 часов");
        assert_eq!(retention_label(11 * 3600), "11 часов");
        assert!(state
            .hint()
            .contains("Лимит хранилища на сервере — 6.0 ГБ."));
        assert!(state.hint().contains("Копии доступны участникам 21 час."));
        let status = status_text(&record(&"a".repeat(32), RECORDING_READY, 1), 1_700_000_100);
        assert!(status.starts_with("На сервере до "));
        assert!(status.contains(".2027,"));
    }
}
