//! State and pure helpers for the personal "Сообщения" mode. The visual
//! surface lives in `app/messages_view.rs`; this module owns bounded paging,
//! client-side name filtering and the small validation rules shared with the
//! backend (`internal/community/conversations.go`).

use gpui::{AppContext, Entity};
use gpui_component::input::InputState;
use std::collections::HashSet;
use voxhold_api::{
    Conversation, ConversationSearchHit, ConversationSearchPage, MAX_NAME_RUNES, MAX_USERS,
};

/// One index page. The backend default is 200, but a smaller page keeps the
/// first paint fast and gives "Показать ещё" a real bounded flow.
pub const PAGE_SIZE: u32 = 30;
/// Search pages are server-capped at 50 rows; the client keeps a bounded list.
pub const MAX_SEARCH_ROWS: usize = 200;

#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub results: Vec<ConversationSearchHit>,
    pub has_more: bool,
    pub next_before: Option<i64>,
    pub loading: bool,
    pub loading_more: bool,
    pub error: String,
    pub request: u64,
    pub submitted: String,
}

impl SearchState {
    /// A changed query or a closed surface owns the next answer.
    pub fn invalidate(&mut self) {
        self.request = self.request.wrapping_add(1);
        self.results.clear();
        self.has_more = false;
        self.next_before = None;
        self.loading = false;
        self.loading_more = false;
        self.error.clear();
        self.submitted.clear();
    }

    pub fn apply_page(&mut self, page: ConversationSearchPage, append: bool) {
        let base = if append {
            std::mem::take(&mut self.results)
        } else {
            Vec::new()
        };
        let mut seen: HashSet<i64> = base.iter().map(|hit| hit.id).collect();
        let mut rows = base;
        for hit in page.results {
            if hit.id > 0 && hit.conversation_id > 0 && seen.insert(hit.id) {
                rows.push(hit);
            }
        }
        rows.truncate(MAX_SEARCH_ROWS);
        self.results = rows;
        self.has_more = page.has_more;
        self.next_before = page.next_before;
        self.loading = false;
        self.loading_more = false;
        self.error.clear();
    }
}

#[derive(Debug, Clone, Default)]
pub struct Notifications {
    pub id: i64,
    pub muted: bool,
    pub loaded: bool,
    pub pending: bool,
    pub error: String,
    pub request: u64,
}

/// One `conversation.changed` that asked the next index answer to reconcile
/// the open row. It is scoped to the exact conversation, selection, surface
/// epoch and account session so a later page cannot replay it against a
/// different selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationReconcile {
    pub conversation: i64,
    pub select_seq: u64,
    pub epoch: u64,
    pub session: u64,
}

pub struct Creator {
    pub selected: Vec<i64>,
    pub name: Entity<InputState>,
    pub search: Entity<InputState>,
    pub busy: bool,
    pub error: String,
    pub request: u64,
}

impl Creator {
    pub fn new(window: &mut gpui::Window, cx: &mut gpui::App) -> Self {
        Self {
            selected: Vec::new(),
            name: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Название беседы (необязательно)")
            }),
            search: cx.new(|cx| InputState::new(window, cx).placeholder("Имя участника")),
            busy: false,
            error: String::new(),
            request: 0,
        }
    }
}

pub struct Messages {
    pub loading: bool,
    pub error: String,
    pub has_more: bool,
    pub cursor: Option<String>,
    pub older_loading: bool,
    pub older_error: String,
    pub older_request: Option<u64>,
    pub query: Entity<InputState>,
    pub global: bool,
    pub search: SearchState,
    pub creator: Option<Creator>,
    pub notifications: Option<Notifications>,
    pub leave_confirm: bool,
    pub leave_request: Option<u64>,
    pub leave_error: String,
    /// Bumped when the personal surface closes; an older page answer cannot
    /// append into a view that the user already left.
    pub epoch: u64,
    /// One `GET /conversations/{id}` for a search hit outside the loaded page.
    pub open_request: Option<u64>,
    /// One `GET /conversations/{id}` that decides whether an open row outside
    /// a partial page still exists.
    pub reconcile_request: Option<u64>,
    /// One `GET /conversations/{id}` after a send `403`, which is used both for
    /// a lost membership and for a `canContact` refusal.
    pub send_verify_request: Option<u64>,
}

impl Messages {
    pub fn new(window: &mut gpui::Window, cx: &mut gpui::App) -> Self {
        Self {
            loading: false,
            error: String::new(),
            has_more: false,
            cursor: None,
            older_loading: false,
            older_error: String::new(),
            older_request: None,
            query: cx.new(|cx| InputState::new(window, cx).placeholder("Имя или название беседы")),
            global: false,
            search: SearchState::default(),
            creator: None,
            notifications: None,
            leave_confirm: false,
            leave_request: None,
            leave_error: String::new(),
            epoch: 0,
            open_request: None,
            reconcile_request: None,
            send_verify_request: None,
        }
    }

    pub fn clear_search(&mut self) {
        self.global = false;
        self.search.invalidate();
    }

    pub fn clear_leave(&mut self) {
        self.leave_confirm = false;
        self.leave_request = None;
        self.leave_error.clear();
    }

    /// Drop every personal-surface leftover on session end. The query input and
    /// the creator entities belong to the signed-in account, so the input is
    /// cleared and the creator entities are dropped with the window available.
    pub fn reset(&mut self, window: &mut gpui::Window, cx: &mut gpui::App) {
        self.loading = false;
        self.error.clear();
        self.has_more = false;
        self.cursor = None;
        self.older_loading = false;
        self.older_error.clear();
        self.older_request = None;
        self.query.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder("Имя или название беседы", window, cx);
        });
        self.clear_search();
        self.creator = None;
        self.notifications = None;
        self.clear_leave();
        self.epoch = self.epoch.wrapping_add(1);
        self.open_request = None;
        self.reconcile_request = None;
        self.send_verify_request = None;
    }
}

/// The backend orders conversations by activity (`last_message_id`) and id,
/// both descending. Restoring that order keeps a merged tail in place.
pub fn sort_index(index: &mut [Conversation]) {
    index.sort_by(|left, right| {
        right
            .last_message_id
            .cmp(&left.last_message_id)
            .then_with(|| right.id.cmp(&left.id))
    });
}

/// Apply a fresh first page. On a partial index (`has_more`) rows previously
/// loaded from older pages are kept and merged by id, because the page only
/// proves what it contains, not what is gone. A complete index is
/// authoritative: its rows are the whole list and the older tail is pruned.
/// Fresh page values win for a duplicate id.
pub fn merge_first_page(index: &mut Vec<Conversation>, rows: Vec<Conversation>, has_more: bool) {
    let previous = std::mem::take(index);
    let mut seen = HashSet::new();
    let mut merged: Vec<Conversation> = Vec::new();
    for row in rows {
        if row.id > 0 && !row.saved && seen.insert(row.id) {
            merged.push(row);
        }
    }
    if has_more {
        for row in previous {
            if row.id > 0 && !row.saved && seen.insert(row.id) {
                merged.push(row);
            }
        }
    }
    sort_index(&mut merged);
    *index = merged;
}

/// Append an older page once: duplicate ids never create a second row and the
/// existing (newer) copy keeps its position and fields.
pub fn append_page(index: &mut Vec<Conversation>, rows: Vec<Conversation>) -> bool {
    let mut seen: HashSet<i64> = index.iter().map(|row| row.id).collect();
    let before = index.len();
    for row in rows {
        if row.id > 0 && !row.saved && seen.insert(row.id) {
            index.push(row);
        }
    }
    index.len() != before
}

/// The sidebar filter is client-side, exactly like `CommunitySidebar`.
pub fn filter_names(rows: &[Conversation], self_user_id: i64, query: &str) -> Vec<Conversation> {
    let needle = query.trim().to_lowercase();
    rows.iter()
        .filter(|row| !row.saved)
        .filter(|row| {
            needle.is_empty()
                || crate::inbox::conversation_title(row, self_user_id)
                    .to_lowercase()
                    .contains(&needle)
        })
        .cloned()
        .collect()
}

pub fn validate_create(ids: &[i64], name: &str) -> Result<(), String> {
    let mut seen = HashSet::new();
    if ids.is_empty() {
        return Err("Выберите хотя бы одного участника.".into());
    }
    if ids.len() > MAX_USERS {
        return Err(format!("Можно добавить не более {MAX_USERS} участников."));
    }
    if ids.iter().any(|id| *id <= 0 || !seen.insert(*id)) {
        return Err("Участники указаны неверно.".into());
    }
    if name.trim().chars().count() > MAX_NAME_RUNES {
        return Err(format!(
            "Название беседы длиннее {MAX_NAME_RUNES} символов."
        ));
    }
    Ok(())
}

/// Toggle one member in the creator, keeping the backend's 15-person cap.
pub fn toggle_selected(selected: &mut Vec<i64>, id: i64) -> bool {
    if let Some(position) = selected.iter().position(|value| *value == id) {
        selected.remove(position);
        return true;
    }
    if id <= 0 || selected.len() >= MAX_USERS {
        return false;
    }
    selected.push(id);
    true
}

/// One-line preview for a global search hit.
pub fn search_preview(content: &str) -> String {
    let value = content.replace(['\n', '\r'], " ");
    let mut out: String = value.chars().take(120).collect();
    if value.chars().count() > 120 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxhold_api::{Author, ConversationSearchHit};

    fn conversation(id: i64, name: &str, direct: bool) -> Conversation {
        Conversation {
            id,
            name: name.into(),
            direct,
            members: vec![
                Author {
                    user_id: 1,
                    username: "me".into(),
                },
                Author {
                    user_id: 2,
                    username: name.into(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn pages_append_once_and_keep_the_backend_order() {
        let mut index = Vec::new();
        merge_first_page(
            &mut index,
            vec![
                conversation(5, "five", true),
                conversation(4, "four", false),
            ],
            false,
        );
        assert_eq!(index.len(), 2);
        assert!(append_page(
            &mut index,
            vec![
                conversation(5, "duplicate", true),
                conversation(3, "three", true)
            ],
        ));
        assert_eq!(
            index.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![5, 4, 3]
        );
        assert_eq!(index[0].name, "five", "the existing row is not replaced");
        assert!(!append_page(
            &mut index,
            vec![conversation(3, "again", true)]
        ));
    }

    #[test]
    fn a_partial_first_page_keeps_rows_loaded_from_older_pages() {
        let mut index = vec![conversation(9, "older", true), conversation(1, "new", true)];
        merge_first_page(
            &mut index,
            vec![
                conversation(9, "fresh", true),
                conversation(8, "eight", true),
            ],
            true,
        );
        assert_eq!(
            index.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![9, 8, 1],
            "the fresh page leads and the older tail is kept once"
        );
        assert_eq!(index[0].name, "fresh", "fresh fields win for a duplicate");
        // A complete index owns the whole list and prunes the tail.
        merge_first_page(&mut index, vec![conversation(9, "fresh", true)], false);
        assert_eq!(index.iter().map(|row| row.id).collect::<Vec<_>>(), vec![9]);
    }

    #[test]
    fn saved_rows_are_not_part_of_the_personal_index() {
        let mut index = Vec::new();
        let mut saved = conversation(9, "Избранное", false);
        saved.saved = true;
        merge_first_page(
            &mut index,
            vec![saved, conversation(2, "neva", true)],
            false,
        );
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].id, 2);
    }

    #[test]
    fn client_filter_uses_the_visible_title() {
        let rows = vec![
            conversation(1, "neva", true),
            conversation(2, "Проект", false),
        ];
        assert_eq!(filter_names(&rows, 1, "").len(), 2);
        assert_eq!(filter_names(&rows, 1, "nev")[0].id, 1);
        assert_eq!(filter_names(&rows, 1, "про")[0].id, 2);
        assert!(filter_names(&rows, 1, "никого").is_empty());
    }

    #[test]
    fn creation_validation_matches_the_backend_limits() {
        assert!(validate_create(&[], "").is_err());
        assert!(validate_create(&[2], "ok").is_ok());
        assert!(validate_create(&[2, 2], "").is_err());
        assert!(validate_create(&[2, 0], "").is_err());
        assert!(validate_create(&(2..18).collect::<Vec<_>>(), "").is_err());
        assert!(validate_create(&[2], &"я".repeat(MAX_NAME_RUNES)).is_ok());
        assert!(validate_create(&[2], &"я".repeat(MAX_NAME_RUNES + 1)).is_err());
    }

    #[test]
    fn member_toggle_keeps_the_fifteen_person_cap() {
        let mut selected = vec![2];
        assert!(toggle_selected(&mut selected, 3));
        assert_eq!(selected, vec![2, 3]);
        assert!(toggle_selected(&mut selected, 2));
        assert_eq!(selected, vec![3]);
        selected = (2..17).collect();
        assert!(!toggle_selected(&mut selected, 99));
        assert_eq!(selected.len(), MAX_USERS);
    }

    #[test]
    fn search_pages_deduplicate_and_bound_the_rows() {
        let mut state = SearchState::default();
        let hit = |id: i64, cid: i64| ConversationSearchHit {
            id,
            conversation_id: cid,
            author_id: 2,
            author_username: "neva".into(),
            content: "budget".into(),
            created_at: 1,
        };
        state.apply_page(
            ConversationSearchPage {
                results: vec![hit(5, 1), hit(4, 1)],
                has_more: true,
                next_before: Some(4),
            },
            false,
        );
        assert_eq!(state.results.len(), 2);
        assert!(state.has_more);
        state.apply_page(
            ConversationSearchPage {
                results: vec![hit(4, 1), hit(3, 2)],
                has_more: false,
                next_before: None,
            },
            true,
        );
        assert_eq!(
            state.results.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![5, 4, 3]
        );
        assert!(!state.has_more);
    }

    #[test]
    fn search_preview_is_one_line_and_bounded() {
        assert_eq!(search_preview("a\nb"), "a b");
        let long: String = "x".repeat(130);
        assert_eq!(search_preview(&long).chars().count(), 121);
    }
}
