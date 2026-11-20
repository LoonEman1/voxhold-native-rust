//! Workspace shell geometry, ported from the Go client's
//! `internal/workspace/layout.go`.
//!
//! The shell is a pure function of the client area, the persisted widths and
//! which overlays are open. Keep it free of GPUI types so the breakpoints can
//! be covered by plain unit tests; `main.rs` converts the result to pixels.

use std::path::{Path, PathBuf};

pub const RAIL_WIDTH: f32 = 76.0;
pub const MOBILE_MAX_WIDTH: f32 = 760.0;
pub const COMPACT_MAX_WIDTH: f32 = 1180.0;
pub const BOTTOM_HEIGHT: f32 = 63.0;
pub const COMPACT_MEMBERS_TOP: f32 = 67.0;
pub const MOBILE_MEMBERS_TOP: f32 = 58.0;
pub const CHANNEL_MIN_WIDTH: f32 = 210.0;
pub const CHANNEL_MAX_WIDTH: f32 = 480.0;
pub const MEMBERS_MIN_WIDTH: f32 = 190.0;
pub const MEMBERS_MAX_WIDTH: f32 = 420.0;
pub const HANDLE_WIDTH: f32 = 13.0;
pub const HANDLE_OFFSET: f32 = 6.0;
pub const MIN_CHAT_WIDTH: f32 = 390.0;
pub const NARROW_MARGIN: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            x,
            y,
            w: w.max(0.0),
            h: h.max(0.0),
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// Persisted workspace preferences. Mirrors the Go `Settings` JSON shape:
/// `{"channel_width":252,"members_width":238,"members_open":true}`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub channel_width: f32,
    pub members_width: f32,
    pub members_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            channel_width: 252.0,
            members_width: 238.0,
            members_open: true,
        }
    }
}

impl Settings {
    /// Clamp like Go's `clamp`: NaN and infinities fall back to the low bound.
    fn value(v: f32, lo: f32, hi: f32) -> f32 {
        if !v.is_finite() {
            return lo;
        }
        v.clamp(lo, hi.max(lo))
    }

    pub fn normalized(self) -> Self {
        Self {
            channel_width: Self::value(self.channel_width, CHANNEL_MIN_WIDTH, CHANNEL_MAX_WIDTH),
            members_width: Self::value(self.members_width, MEMBERS_MIN_WIDTH, MEMBERS_MAX_WIDTH),
            members_open: self.members_open,
        }
    }

    /// `%APPDATA%\Voxhold\workspace-layout.json`, the same file the Go client
    /// uses. Preferences hold no credentials or drafts.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("Voxhold").join("workspace-layout.json"))
    }

    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        parse_settings(&bytes)
            .map(Self::normalized)
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let normalized = self.normalized();
        let body = format!(
            "{{\"channel_width\":{},\"members_width\":{},\"members_open\":{}}}",
            json_number(normalized.channel_width),
            json_number(normalized.members_width),
            normalized.members_open
        );
        save_preferences(path, body.as_bytes())
    }
}

/// Round-trip through the wire form so the file shape matches Go exactly.
fn json_number(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Minimal, tolerant reader for the three known keys. Unknown fields are
/// ignored, matching `encoding/json` on the Go side.
fn parse_settings(bytes: &[u8]) -> Option<Settings> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut settings = Settings::default();
    let mut seen = false;
    for key in ["channel_width", "members_width", "members_open"] {
        let Some(index) = text.find(&format!("\"{key}\"")) else {
            continue;
        };
        let rest = &text[index + key.len() + 2..];
        let rest = rest.trim_start().strip_prefix(':')?.trim_start();
        match key {
            "members_open" => {
                settings.members_open = rest.starts_with("true");
                seen = true;
            }
            _ => {
                let end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '+' || c == '.'))
                    .unwrap_or(rest.len());
                let value: f32 = rest[..end].parse().ok()?;
                if key == "channel_width" {
                    settings.channel_width = value;
                } else {
                    settings.members_width = value;
                }
                seen = true;
            }
        }
    }
    seen.then_some(settings)
}

/// Write through a unique temporary file in the target directory and rename,
/// so a crash mid-write cannot leave a truncated preference file.
fn save_preferences(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace-layout.json".into());
    let mut attempt = 0u32;
    let temp = loop {
        let candidate = dir.join(format!(".{name}.{}.{attempt}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                use std::io::Write;
                if let Err(error) = file.write_all(data).and_then(|_| file.sync_all()) {
                    drop(file);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error);
                }
                break candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
                if attempt > 16 {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    };
    let result = std::fs::rename(&temp, path);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub mobile: bool,
    pub compact: bool,
    pub rail: Rect,
    pub channels: Rect,
    pub chat: Rect,
    pub members: Rect,
    pub bottom: Rect,
    pub channel_handle: Rect,
    pub member_handle: Rect,
}

impl Layout {
    pub fn is_members_open(&self) -> bool {
        !self.members.is_empty()
    }

    pub fn is_channels_open(&self) -> bool {
        !self.channels.is_empty()
    }
}

/// Port of `ComputeLayout`. All measurements are logical pixels; a small window
/// never overwrites the persisted widths.
pub fn compute_layout(
    w: f32,
    h: f32,
    settings: Settings,
    channels_open: bool,
    members_open: bool,
) -> Layout {
    let settings = settings.normalized();
    let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
    let h = if h.is_finite() { h.max(0.0) } else { 0.0 };
    let mut layout = Layout {
        mobile: w <= MOBILE_MAX_WIDTH,
        compact: w <= COMPACT_MAX_WIDTH,
        rail: Rect::new(0.0, 0.0, 0.0, 0.0),
        channels: Rect::new(0.0, 0.0, 0.0, 0.0),
        chat: Rect::new(0.0, 0.0, 0.0, 0.0),
        members: Rect::new(0.0, 0.0, 0.0, 0.0),
        bottom: Rect::new(0.0, 0.0, 0.0, 0.0),
        channel_handle: Rect::new(0.0, 0.0, 0.0, 0.0),
        member_handle: Rect::new(0.0, 0.0, 0.0, 0.0),
    };
    if layout.mobile {
        let body = (h - BOTTOM_HEIGHT).max(0.0);
        layout.bottom = Rect::new(0.0, body, w, h - body);
        layout.chat = Rect::new(0.0, 0.0, w, body);
        if channels_open {
            layout.channels = Rect::new(
                0.0,
                0.0,
                settings.channel_width.min(w - NARROW_MARGIN),
                body,
            );
            layout.channel_handle = Rect::new(
                layout.channels.right() - HANDLE_OFFSET,
                0.0,
                HANDLE_WIDTH,
                body,
            );
        }
        if members_open {
            let mw = settings.members_width.min(w - NARROW_MARGIN);
            layout.members = Rect::new(w - mw, MOBILE_MEMBERS_TOP, mw, body - MOBILE_MEMBERS_TOP);
            layout.member_handle = Rect::new(
                w - mw - HANDLE_OFFSET,
                MOBILE_MEMBERS_TOP,
                HANDLE_WIDTH,
                body - MOBILE_MEMBERS_TOP,
            );
        }
        return layout;
    }
    layout.rail = Rect::new(0.0, 0.0, RAIL_WIDTH, h);
    let mut mw = 0.0;
    if !layout.compact && members_open {
        mw = Settings::value(
            settings.members_width,
            MEMBERS_MIN_WIDTH,
            (MEMBERS_MAX_WIDTH).min(w - RAIL_WIDTH - CHANNEL_MIN_WIDTH - MIN_CHAT_WIDTH),
        );
    }
    let cw = Settings::value(
        settings.channel_width,
        CHANNEL_MIN_WIDTH,
        (CHANNEL_MAX_WIDTH).min(w - RAIL_WIDTH - mw - MIN_CHAT_WIDTH),
    );
    layout.channels = Rect::new(RAIL_WIDTH, 0.0, cw, h);
    layout.channel_handle = Rect::new(RAIL_WIDTH + cw - HANDLE_OFFSET, 0.0, HANDLE_WIDTH, h);
    layout.chat = Rect::new(RAIL_WIDTH + cw, 0.0, w - RAIL_WIDTH - cw - mw, h);
    if members_open {
        if layout.compact {
            mw = settings.members_width.min(w - NARROW_MARGIN);
            layout.members = Rect::new(w - mw, COMPACT_MEMBERS_TOP, mw, h - COMPACT_MEMBERS_TOP);
        } else {
            layout.members = Rect::new(w - mw, 0.0, mw, h);
        }
        layout.member_handle = Rect::new(
            w - mw - HANDLE_OFFSET,
            layout.members.y,
            HANDLE_WIDTH,
            layout.members.h,
        );
    }
    layout
}

pub const VOICE_CHAT_MIN_DOCK: f32 = 300.0;
pub const VOICE_CHAT_MAX_DOCK: f32 = 440.0;
pub const VOICE_CHAT_MIN_ROOM: f32 = 300.0;
pub const VOICE_CHAT_MIN_HEIGHT: f32 = 200.0;

/// The room area and the optional docked chat panel inside it. The two
/// rectangles never overlap: a wide area docks the chat on the right, a narrow
/// one stacks it under the room. The room is resized to the remainder before
/// its tiles are laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceChatLayout {
    pub room: Rect,
    pub chat: Option<Rect>,
}

impl VoiceChatLayout {
    pub fn closed(area: Rect) -> Self {
        Self {
            room: area,
            chat: None,
        }
    }
}

pub fn voice_chat_layout(area: Rect, open: bool) -> VoiceChatLayout {
    if !open || area.is_empty() {
        return VoiceChatLayout::closed(area);
    }
    if !area.w.is_finite() || !area.h.is_finite() || area.w <= 0.0 {
        return VoiceChatLayout::closed(area);
    }
    let dock = (area.w * 0.34)
        .clamp(VOICE_CHAT_MIN_DOCK, VOICE_CHAT_MAX_DOCK)
        .min(area.w);
    if area.w - dock >= VOICE_CHAT_MIN_ROOM {
        return VoiceChatLayout {
            room: Rect::new(area.x, area.y, area.w - dock, area.h),
            chat: Some(Rect::new(area.x + area.w - dock, area.y, dock, area.h)),
        };
    }
    // Not enough width for both columns: use a non-overlapping stacked panel
    // instead of covering the room cards with an overlay.
    let chat_h = (area.h * 0.45)
        .clamp(VOICE_CHAT_MIN_HEIGHT, 420.0)
        .min((area.h - VOICE_CHAT_MIN_HEIGHT).max(0.0));
    let room_h = (area.h - chat_h).max(0.0);
    VoiceChatLayout {
        room: Rect::new(area.x, area.y, area.w, room_h),
        chat: Some(Rect::new(area.x, area.y + room_h, area.w, chat_h)),
    }
}

/// Drag state for the two shell splitters. The offset from the original edge is
/// applied to the pointer position, then both Go bounds are enforced by
/// [`compute_layout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitterSide {
    Channels,
    Members,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitterDrag {
    pub side: SplitterSide,
    pub grab_offset: f32,
    pub last_width: f32,
}

/// Width proposed by a pointer position for the dragged splitter.
pub fn proposed_width(side: SplitterSide, pointer_x: f32, grab_offset: f32) -> f32 {
    match side {
        SplitterSide::Channels => pointer_x - grab_offset - RAIL_WIDTH,
        SplitterSide::Members => pointer_x - grab_offset,
    }
}

/// Members width is measured from the right edge, so a pointer drag maps to
/// `right - pointer` rather than a left-aligned width.
pub fn members_width_from_pointer(viewport_width: f32, pointer_x: f32, grab_offset: f32) -> f32 {
    viewport_width - (pointer_x - grab_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rect(actual: Rect, expected: (f32, f32, f32, f32)) {
        assert_eq!(
            (actual.x, actual.y, actual.w, actual.h),
            expected,
            "unexpected rect {actual:?}"
        );
    }

    #[test]
    fn control_bounds_from_plan() {
        let settings = Settings::default();
        let wide = compute_layout(1440.0, 900.0, settings, true, true);
        assert!(!wide.mobile && !wide.compact);
        assert_rect(wide.rail, (0.0, 0.0, 76.0, 900.0));
        assert_rect(wide.channels, (76.0, 0.0, 252.0, 900.0));
        assert_rect(wide.chat, (328.0, 0.0, 874.0, 900.0));
        assert_rect(wide.members, (1202.0, 0.0, 238.0, 900.0));
        assert!(wide.bottom.is_empty());
        assert_rect(wide.channel_handle, (322.0, 0.0, 13.0, 900.0));
        assert_rect(wide.member_handle, (1196.0, 0.0, 13.0, 900.0));

        let default = compute_layout(1280.0, 900.0, settings, true, true);
        assert_rect(default.rail, (0.0, 0.0, 76.0, 900.0));
        assert_rect(default.channels, (76.0, 0.0, 252.0, 900.0));
        assert_rect(default.chat, (328.0, 0.0, 714.0, 900.0));
        assert_rect(default.members, (1042.0, 0.0, 238.0, 900.0));

        let compact = compute_layout(1180.0, 900.0, settings, true, true);
        assert!(compact.compact && !compact.mobile);
        assert_rect(compact.chat, (328.0, 0.0, 852.0, 900.0));
        assert_rect(compact.members, (942.0, 67.0, 238.0, 833.0));

        let small = compute_layout(900.0, 720.0, settings, true, true);
        assert_rect(small.rail, (0.0, 0.0, 76.0, 720.0));
        assert_rect(small.channels, (76.0, 0.0, 252.0, 720.0));
        assert_rect(small.chat, (328.0, 0.0, 572.0, 720.0));
        assert_rect(small.members, (662.0, 67.0, 238.0, 653.0));

        let mobile = compute_layout(390.0, 760.0, settings, false, true);
        assert!(mobile.mobile);
        assert!(mobile.rail.is_empty());
        assert!(mobile.channels.is_empty());
        assert_rect(mobile.chat, (0.0, 0.0, 390.0, 697.0));
        assert_rect(mobile.members, (152.0, 58.0, 238.0, 639.0));
        assert_rect(mobile.bottom, (0.0, 697.0, 390.0, 63.0));
    }

    #[test]
    fn closed_members_sidebars_return_the_space_to_chat() {
        let layout = compute_layout(1280.0, 900.0, Settings::default(), true, false);
        assert_eq!(layout.channels.x, RAIL_WIDTH);
        assert!(layout.members.is_empty());
        assert_rect(layout.chat, (328.0, 0.0, 952.0, 900.0));
        // The channel width still applies; on desktop the channel panel is part
        // of the shell and only collapses through the narrow-layout overlay.
        assert_eq!(layout.chat.x, RAIL_WIDTH + layout.channels.w);
    }

    #[test]
    fn breakpoints_are_inclusive() {
        assert!(compute_layout(760.0, 720.0, Settings::default(), true, true).mobile);
        assert!(!compute_layout(761.0, 720.0, Settings::default(), true, true).mobile);
        assert!(compute_layout(1180.0, 900.0, Settings::default(), true, true).compact);
        assert!(!compute_layout(1181.0, 900.0, Settings::default(), true, true).compact);
    }

    #[test]
    fn narrow_mobile_panels_keep_a_20px_gutter() {
        let layout = compute_layout(150.0, 400.0, Settings::default(), true, true);
        assert_eq!(layout.channels.w, 130.0);
        assert_eq!(layout.members.w, 130.0);
        assert_eq!(layout.members.x, 20.0);
    }

    #[test]
    fn persisted_widths_are_clamped() {
        let settings = Settings {
            channel_width: 999.0,
            members_width: -4.0,
            members_open: true,
        }
        .normalized();
        assert_eq!(settings.channel_width, CHANNEL_MAX_WIDTH);
        assert_eq!(settings.members_width, MEMBERS_MIN_WIDTH);

        let nan = Settings {
            channel_width: f32::NAN,
            members_width: f32::INFINITY,
            members_open: false,
        }
        .normalized();
        assert_eq!(nan.channel_width, CHANNEL_MIN_WIDTH);
        assert_eq!(nan.members_width, MEMBERS_MIN_WIDTH);
    }

    #[test]
    fn wide_window_never_squeezes_chat_below_390() {
        let settings = Settings {
            channel_width: 480.0,
            members_width: 420.0,
            members_open: true,
        };
        let layout = compute_layout(1100.0, 800.0, settings, true, true);
        assert!(layout.chat.w >= MIN_CHAT_WIDTH - 0.01);
        assert!(layout.channels.w >= CHANNEL_MIN_WIDTH);
    }

    #[test]
    fn zero_and_tiny_areas_stay_non_negative() {
        for (w, h) in [(0.0, 0.0), (1.0, 1.0), (10.0, 30.0), (40.0, 63.0)] {
            let layout = compute_layout(w, h, Settings::default(), true, true);
            for rect in [
                layout.rail,
                layout.channels,
                layout.chat,
                layout.members,
                layout.bottom,
                layout.channel_handle,
                layout.member_handle,
            ] {
                assert!(rect.w >= 0.0 && rect.h >= 0.0, "{rect:?}");
            }
        }
    }

    #[test]
    fn settings_round_trip_through_the_go_file_shape() {
        let dir = std::env::temp_dir().join(format!(
            "voxhold-layout-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspace-layout.json");
        let settings = Settings {
            channel_width: 300.0,
            members_width: 260.0,
            members_open: false,
        };
        settings.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            raw,
            "{\"channel_width\":300,\"members_width\":260,\"members_open\":false}"
        );
        assert_eq!(Settings::load(&path), settings);

        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn splitter_drag_maps_pointer_to_width() {
        assert_eq!(proposed_width(SplitterSide::Channels, 400.0, 6.0), 318.0);
        assert_eq!(members_width_from_pointer(1280.0, 1048.0, 6.0), 238.0);
    }

    #[test]
    fn voice_chat_docks_right_and_resizes_the_room() {
        let area = Rect::new(328.0, 67.0, 714.0, 833.0);
        let layout = voice_chat_layout(area, true);
        let chat = layout.chat.expect("docked chat");
        assert_eq!(chat.w, 300.0, "clamp lower bound");
        assert_eq!(chat.x + chat.w, area.right());
        assert_eq!(layout.room.w, area.w - chat.w);
        assert_eq!(layout.room.right(), chat.x, "no overlap");
        assert_eq!(layout.room.h, area.h);
    }

    #[test]
    fn voice_chat_width_follows_the_web_clamp() {
        let area = Rect::new(0.0, 0.0, 1200.0, 700.0);
        let chat = voice_chat_layout(area, true).chat.expect("docked chat");
        assert_eq!(chat.w, 408.0, "34% of the area");
        assert!(chat.w <= VOICE_CHAT_MAX_DOCK);
        let wide = voice_chat_layout(Rect::new(0.0, 0.0, 2000.0, 700.0), true)
            .chat
            .expect("docked chat");
        assert_eq!(wide.w, VOICE_CHAT_MAX_DOCK);
    }

    #[test]
    fn voice_chat_stacks_without_overlap_on_a_narrow_area() {
        let area = Rect::new(328.0, 67.0, 572.0, 653.0);
        let layout = voice_chat_layout(area, true);
        let chat = layout.chat.expect("stacked chat");
        assert_eq!(layout.room.bottom(), chat.y, "no overlap");
        assert_eq!(chat.w, area.w, "the panel keeps the full width");
        assert!(layout.room.h > 0.0 && chat.h > 0.0);
    }

    #[test]
    fn clipped_voice_chat_changes_nothing() {
        let area = Rect::new(10.0, 20.0, 0.0, 100.0);
        assert_eq!(voice_chat_layout(area, true), VoiceChatLayout::closed(area));
        let area = Rect::new(10.0, 20.0, 500.0, 600.0);
        assert_eq!(
            voice_chat_layout(area, false),
            VoiceChatLayout::closed(area)
        );
    }
}
