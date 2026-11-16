//! Modal geometry and per-dialog state for «Записи сервера».
//!
//! The server snapshot lives in [`crate::recordings`]. This module owns only
//! the open dialog: scroll offset, one-shot focus jumps, clipped action
//! hitboxes and the bounded private preview cache.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use gpui::RenderImage;
use voxhold_api::{Recording, RECORDING_READY, RECORDING_UPLOADING};

use crate::media::ImageCache;
use crate::recordings::{
    expired, live, ready, status_text, RecordingsState, CARD_GAP, CARD_PADDING, LIST_SHARE,
    PREVIEW_BUDGET, PREVIEW_HEIGHT, PREVIEW_WIDTH, RETRY_WIDTH,
};
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    #[allow(dead_code)]
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2., self.y + self.h / 2.)
    }
    pub fn contains(&self, point: (f32, f32)) -> bool {
        point.0 >= self.x
            && point.0 <= self.right()
            && point.1 >= self.y
            && point.1 <= self.bottom()
    }
}

pub fn stacked(inner: f32) -> bool {
    inner < 360.
}

fn status_lines(status: &str, width: f32) -> usize {
    wrap_lines(status, width.max(40.), 11.).len().max(1)
}

fn details_height(status: &str, width: f32, confirm: bool) -> f32 {
    let mut height = 38. + 8. + 15. + 8. + status_lines(status, width) as f32 * 16. + 12. + 30.;
    if confirm {
        height += 8. + 32. + 8.;
    }
    height
}

pub fn details_width(inner: f32) -> f32 {
    if stacked(inner) {
        inner - 2. * CARD_PADDING
    } else {
        inner - 2. * CARD_PADDING - PREVIEW_WIDTH - 14.
    }
}

pub fn row_height(record: &Recording, inner: f32, now: i64, confirm: bool) -> f32 {
    let status = status_text(record, now);
    let details = details_height(&status, details_width(inner), confirm);
    if stacked(inner) {
        2. * CARD_PADDING + PREVIEW_HEIGHT + 10. + details
    } else {
        2. * CARD_PADDING + PREVIEW_HEIGHT.max(details)
    }
}

/// Rough word wrap for card geometry. GPUI still shapes the text itself; this
/// only decides how many status lines reserve height, like Go `typography.Measure`.
pub fn wrap_lines(text: &str, width: f32, size: f32) -> Vec<String> {
    let char_width = (size * 0.55).max(1.);
    let limit = (width / char_width).max(1.).floor() as usize;
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let next = if line.is_empty() {
            word.chars().count()
        } else {
            line.chars().count() + 1 + word.chars().count()
        };
        if next > limit && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DialogGeometry {
    pub bounds: Rect,
    pub list: Rect,
    pub footer_top: f32,
    pub max_scroll: f32,
}

pub fn dialog_geometry(
    viewport_width: f32,
    viewport_height: f32,
    hint_lines: usize,
    row_heights: &[f32],
) -> DialogGeometry {
    let mut width = 570_f32.min(viewport_width - 16.).max(0.);
    if width < 260. {
        width = (viewport_width - 8.).max(160.);
    }
    let padding = if width >= 480. { 25. } else { 16. };
    let inner = width - 2. * padding;
    let hint_height = hint_lines.max(1) as f32 * 18.;
    let mut content = 0.;
    for (index, height) in row_heights.iter().enumerate() {
        if index > 0 {
            content += CARD_GAP;
        }
        content += *height;
    }
    if row_heights.is_empty() {
        content = 126.;
    }
    let list_height = content.min(viewport_height * LIST_SHARE);
    let header = 72.;
    let footer = 66.;
    let max_height = 180_f32.max(820_f32.min(viewport_height - 24.));
    let min_height = 232_f32.min(max_height);
    let height = (header + hint_height + 18. + list_height + footer).clamp(min_height, max_height);
    let x = (viewport_width - width) / 2.;
    let y = (viewport_height - height) / 2.;
    let bounds = Rect::new(x, y, width, height);
    let list_top = y + header + hint_height + 18.;
    let footer_top = y + height - footer;
    let list = Rect::new(
        x + padding,
        list_top,
        inner,
        (footer_top - list_top).max(60.),
    );
    DialogGeometry {
        bounds,
        list,
        footer_top,
        max_scroll: (content - list.h).max(0.),
    }
}

#[derive(Clone, Debug)]
pub struct RowView {
    pub record: Recording,
    pub card: Rect,
    pub preview: Rect,
    pub details_top: f32,
    pub confirm: bool,
    pub live: bool,
    pub ready: bool,
    pub download: Rect,
    pub delete: Option<Rect>,
    pub confirm_yes: Option<Rect>,
    pub confirm_no: Option<Rect>,
}

impl RowView {
    pub fn action(&self, key: &str) -> Option<Rect> {
        let id = self.record.id.as_str();
        match key {
            _ if key == format!("rec-download-{id}") => Some(self.download),
            _ if key == format!("rec-delete-{id}") => self.delete,
            _ if key == format!("rec-confirm-{id}") => self.confirm_yes,
            _ if key == format!("rec-cancel-{id}") => self.confirm_no,
            _ => None,
        }
    }
}

/// The modal state owned by one open dialog. A closed dialog or a newer access
/// epoch invalidates its pending preview callbacks.
pub struct DialogState {
    pub server_id: i64,
    pub generation: u64,
    pub epoch: u64,
    pub scroll: f32,
    pub max_scroll: f32,
    pub bounds: Rect,
    pub list: Rect,
    pub footer_top: f32,
    pub padding: f32,
    pub inner: f32,
    pub hint_lines: usize,
    pub rows: Vec<RowView>,
    pub confirm: String,
    pub busy: String,
    pub error: String,
    /// A focus change asks for exactly one layout scroll.
    pub scroll_to: Option<String>,
    pub pressed: Option<String>,
    pub previews: ImageCache<String>,
    pub preview_pending: HashSet<String>,
    pub preview_retry: HashMap<String, Instant>,
    pub preview_generation: u64,
}

impl Default for DialogState {
    fn default() -> Self {
        Self::new(0, 0, 0)
    }
}

impl DialogState {
    pub fn new(server_id: i64, generation: u64, epoch: u64) -> Self {
        Self {
            server_id,
            generation,
            epoch,
            scroll: 0.,
            max_scroll: 0.,
            bounds: Rect::default(),
            list: Rect::default(),
            footer_top: 0.,
            padding: 25.,
            inner: 520.,
            hint_lines: 1,
            rows: Vec::new(),
            confirm: String::new(),
            busy: String::new(),
            error: String::new(),
            scroll_to: None,
            pressed: None,
            previews: ImageCache::new(PREVIEW_BUDGET),
            preview_pending: HashSet::new(),
            preview_retry: HashMap::new(),
            preview_generation: 0,
        }
    }

    /// Invalidate every preview task and cached picture of the previous access
    /// generation or dialog instance.
    pub fn renew_preview_scope(&mut self) -> Vec<std::sync::Arc<RenderImage>> {
        self.generation = self.generation.wrapping_add(1);
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self.preview_pending.clear();
        self.preview_retry.clear();
        self.previews.clear()
    }

    /// Recompute the card list and consume a one-shot focus scroll. Called by
    /// `render` before building elements.
    pub fn layout(
        &mut self,
        state: &RecordingsState,
        viewport_width: f32,
        viewport_height: f32,
        now: i64,
        self_user_id: i64,
    ) {
        let hint = state.hint();
        let mut width = 570_f32.min(viewport_width - 16.).max(0.);
        if width < 260. {
            width = (viewport_width - 8.).max(160.);
        }
        self.padding = if width >= 480. { 25. } else { 16. };
        self.inner = width - 2. * self.padding;
        self.hint_lines = wrap_lines(&hint, self.inner, 12.).len();
        let records = state.sorted(now);
        let heights: Vec<f32> = records
            .iter()
            .map(|record| row_height(record, self.inner, now, self.confirm == record.id))
            .collect();
        let geometry = dialog_geometry(viewport_width, viewport_height, self.hint_lines, &heights);
        self.bounds = geometry.bounds;
        self.list = geometry.list;
        self.footer_top = geometry.footer_top;
        self.max_scroll = geometry.max_scroll;
        self.scroll = self.scroll.clamp(0., self.max_scroll);
        self.rows = self.build_rows(state, &records, now, self_user_id);
        // A focus change from the previous frame is applied once; a wheel or a
        // later layout never keeps pulling the list back.
        if let Some(key) = self.scroll_to.take() {
            for _ in 0..2 {
                let Some(rect) = self.action_rect(&key) else {
                    break;
                };
                let Some(next) = scroll_to(self.scroll, self.max_scroll, rect, self.list) else {
                    break;
                };
                if next == self.scroll {
                    break;
                }
                self.scroll = next;
                self.rows = self.build_rows(state, &records, now, self_user_id);
            }
        }
    }

    fn build_rows(
        &self,
        state: &RecordingsState,
        records: &[Recording],
        now: i64,
        self_user_id: i64,
    ) -> Vec<RowView> {
        let mut rows = Vec::with_capacity(records.len());
        let mut y = self.list.y - self.scroll;
        for record in records {
            let height = row_height(record, self.inner, now, self.confirm == record.id);
            let card = Rect::new(self.list.x, y, self.inner, height);
            let is_stacked = stacked(self.inner);
            let preview = if is_stacked {
                Rect::new(
                    card.x + CARD_PADDING,
                    card.y + CARD_PADDING,
                    (self.inner - 2. * CARD_PADDING).min(160.),
                    PREVIEW_HEIGHT,
                )
            } else {
                Rect::new(
                    card.x + CARD_PADDING,
                    card.y + CARD_PADDING,
                    PREVIEW_WIDTH,
                    PREVIEW_HEIGHT,
                )
            };
            let details_top = if is_stacked {
                preview.bottom() + 10.
            } else {
                card.y + CARD_PADDING
            };
            let x = if is_stacked {
                card.x + CARD_PADDING
            } else {
                preview.right() + 14.
            };
            let width = card.right() - CARD_PADDING - x;
            let confirm = self.confirm == record.id;
            let actions_y = card.bottom() - CARD_PADDING - 30.;
            let busy = !self.busy.is_empty();
            let (download, delete, confirm_yes, confirm_no) = if width < 60. {
                (Rect::default(), None, None, None)
            } else if confirm {
                let confirm_w = 118_f32.max((width - 6.) / 2.);
                (
                    Rect::default(),
                    None,
                    Some(Rect::new(x, actions_y, confirm_w, 30.)),
                    Some(Rect::new(
                        x + confirm_w + 6.,
                        actions_y,
                        (width - confirm_w - 6.).max(64.),
                        30.,
                    )),
                )
            } else {
                let download = Rect::new(x, actions_y, RETRY_WIDTH.max(88.), 30.);
                let owns = state.owns(record, self_user_id, "");
                let can_delete = owns
                    && !live(record, now)
                    && !busy
                    && (record.status == RECORDING_READY || record.status == RECORDING_UPLOADING)
                    && !expired(record, now);
                let delete = can_delete.then(|| {
                    Rect::new(
                        x + download.w + 12.,
                        card.bottom() - CARD_PADDING - 24.,
                        (width - download.w - 12.).max(70.),
                        24.,
                    )
                });
                (download, delete, None, None)
            };
            rows.push(RowView {
                record: record.clone(),
                card,
                preview,
                details_top,
                confirm,
                live: live(record, now),
                ready: ready(record, now),
                download,
                delete,
                confirm_yes,
                confirm_no,
            });
            y += height + CARD_GAP;
        }
        rows
    }

    pub fn action_rect(&self, key: &str) -> Option<Rect> {
        self.rows.iter().find_map(|row| row.action(key))
    }

    /// A card action must be fully inside the list to activate from the
    /// keyboard; a clipped action only asks for a scroll.
    pub fn action_visible(&self, key: &str) -> bool {
        if self.list.h <= 0. {
            return false;
        }
        let Some(rect) = self.action_rect(key) else {
            return false;
        };
        const TOLERANCE: f32 = 0.5;
        rect.y >= self.list.y - TOLERANCE
            && rect.bottom() <= self.list.bottom() + TOLERANCE
            && rect.x >= self.list.x - TOLERANCE
            && rect.right() <= self.list.right() + TOLERANCE
    }

    /// Hit test over the visible part of a card action: a release in the
    /// clipped part cancels the press.
    pub fn action_hit(&self, key: &str, point: (f32, f32)) -> bool {
        self.action_rect(key)
            .is_some_and(|rect| rect.contains(point) && self.list.contains(point))
    }

    pub fn focus_action(&mut self, key: &str) {
        self.scroll_to = Some(key.to_owned());
    }

    /// The user takes over scrolling: drop a pending focus jump.
    pub fn wheel(&mut self, delta_y: f32) {
        self.scroll_to = None;
        self.scroll = (self.scroll + delta_y * 42.).clamp(0., self.max_scroll);
    }

    /// `Enter`/`Space` on a clipped action asks for a scroll and returns false.
    pub fn activate_key(&mut self, key: &str) -> bool {
        if self.action_rect(key).is_none() {
            return true;
        }
        if self.action_visible(key) {
            return true;
        }
        self.scroll_to = Some(key.to_owned());
        false
    }
}

pub fn scroll_to(scroll: f32, max: f32, target: Rect, list: Rect) -> Option<f32> {
    let delta = if target.bottom() > list.bottom() {
        target.bottom() - list.bottom() + 10.
    } else if target.y < list.y {
        target.y - list.y - 10.
    } else {
        return None;
    };
    Some((scroll + delta).clamp(0., max))
}
