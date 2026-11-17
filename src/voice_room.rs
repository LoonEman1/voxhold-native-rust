//! Pure layout and input rules for the voice room, the sidebar dock and the
//! floating stream player. Ported from Go `voice_room.go` and
//! `stream_player.go`, so the geometry and the keyboard order are testable
//! without a window.

/// Go `const gap = 16` between participant cards / stream tiles.
pub const CARD_GAP: f32 = 16.0;
/// Go `min(280, width, ...)`.
pub const CARD_MAX: f32 = 280.0;
pub const CARD_MIN: f32 = 120.0;
/// Go `voiceRoom` margins: body inset 24 left/right, 9 top.
pub const ROOM_INSET: f32 = 24.0;
pub const ROOM_TOP: f32 = 9.0;
pub const ROOM_BOTTOM: f32 = 15.0;
/// Go `layoutVoiceActions`: toolbar `min(width, 330)` wide, 61 high.
pub const TOOLBAR_MAX_WIDTH: f32 = 330.0;
pub const TOOLBAR_HEIGHT: f32 = 61.0;
#[cfg(test)]
pub const TOOLBAR_GAP: f32 = 5.0;
#[cfg(test)]
pub const TOOLBAR_INSET_LABELS: f32 = 7.0;
/// Go `layoutVoiceDock`: 98 px tall.
pub const DOCK_HEIGHT: f32 = 98.0;
/// Go `floatingStreamBox`.
pub const PLAYER_DEFAULT_WIDTH: f32 = 360.0;
pub const PLAYER_MIN_WIDTH: f32 = 260.0;
pub const PLAYER_MAX_WIDTH: f32 = 720.0;
pub const PLAYER_MARGIN: f32 = 8.0;
/// Go `width*9/16 + 76`: 38 px header + 38 px footer.
pub const PLAYER_HEADER: f32 = 38.0;
pub const PLAYER_FOOTER: f32 = 38.0;
pub const PLAYER_ASPECT: f32 = 9.0 / 16.0;
pub const PLAYER_CORNER: f32 = 15.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
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

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// Room body minus the bottom toolbar/join button, and the card grid.
#[derive(Clone, Debug, Default)]
pub struct RoomGeometry {
    pub viewport: Rect,
    pub cards: Vec<Rect>,
    /// Card size and column count are also asserted by the layout tests.
    #[allow(dead_code)]
    pub card_size: f32,
    #[allow(dead_code)]
    pub columns: usize,
    pub content_height: f32,
    /// Bottom toolbar when the call is active, otherwise the join button row.
    pub toolbar: Rect,
}

/// Go `layoutVoiceRoom`: the card grid starts 46 px under the status line; the
/// toolbar (active) or the join button (idle) reserves the bottom.
pub fn room_geometry(body: Rect, active: bool, cards: usize, scroll: f32) -> RoomGeometry {
    let width = (body.w - ROOM_INSET * 2.0).max(0.0);
    let left = body.x + ROOM_INSET;
    let mut top = body.y + ROOM_TOP + 46.0;
    let bottom = body.bottom() - ROOM_BOTTOM;
    let (toolbar, bottom_limit) = if active {
        let bar_width = width.min(TOOLBAR_MAX_WIDTH);
        let toolbar = Rect::new(
            body.x + (body.w - bar_width) / 2.0,
            bottom - TOOLBAR_HEIGHT,
            bar_width,
            TOOLBAR_HEIGHT,
        );
        (toolbar, toolbar.y - 14.0)
    } else {
        let join = Rect::new(left, bottom - 42.0, width.min(165.0), 40.0);
        (join, bottom - 56.0)
    };
    if bottom_limit < top {
        top = bottom_limit;
    }
    let card_size = CARD_MAX
        .min(width)
        .min((bottom_limit - top).max(CARD_MIN))
        .max(1.0);
    let columns = (((width + CARD_GAP) / (card_size + CARD_GAP)).floor() as usize).max(1);
    let rows = cards.div_ceil(columns);
    let content_height = (rows as f32 * (card_size + CARD_GAP) - CARD_GAP).max(0.0);
    let viewport = Rect::new(left, top, width, (bottom_limit - top).max(0.0));
    let max_scroll = (content_height - viewport.h).max(0.0);
    let scroll = scroll.clamp(0.0, max_scroll);
    let cards = (0..cards)
        .map(|index| {
            Rect::new(
                left + (index % columns) as f32 * (card_size + CARD_GAP),
                top + (index / columns) as f32 * (card_size + CARD_GAP) - scroll,
                card_size,
                card_size,
            )
        })
        .collect();
    RoomGeometry {
        viewport,
        cards,
        card_size,
        columns,
        content_height,
        toolbar,
    }
}

/// Go `floatingStreamBox`. Start width is 360 px; the box is clamped into the
/// available area with an 8 px margin so it can never leave the chat.
pub fn floating_stream_box(area: Rect, position: (f32, f32), width: f32) -> Rect {
    let min_width = PLAYER_MIN_WIDTH.min((area.w - PLAYER_MARGIN * 2.0).max(1.0));
    let max_width = PLAYER_MAX_WIDTH
        .min((area.w - PLAYER_MARGIN * 2.0).max(1.0))
        .min(((area.h - 92.0).max(1.0)) * 16.0 / 9.0);
    let width = width.clamp(min_width.min(max_width), max_width.max(min_width));
    let height = width * PLAYER_ASPECT + PLAYER_HEADER + PLAYER_FOOTER;
    let x = position.0.clamp(
        area.x + PLAYER_MARGIN,
        (area.right() - width - PLAYER_MARGIN).max(area.x + PLAYER_MARGIN),
    );
    let y = position.1.clamp(
        area.y + PLAYER_MARGIN,
        (area.bottom() - height - PLAYER_MARGIN).max(area.y + PLAYER_MARGIN),
    );
    Rect::new(x, y, width, height)
}

/// Interactive regions inside a floating player, from Go `layoutStreamPlayer`.
#[derive(Clone, Debug, Default)]
pub struct PlayerGeometry {
    pub header: Rect,
    pub picture: Rect,
    pub corner: Rect,
    pub fullscreen: Rect,
    pub close: Rect,
    pub audio: Option<Rect>,
    pub fullscreen_picture: Rect,
    pub fullscreen_exit: Rect,
    pub fullscreen_audio: Option<Rect>,
}

pub fn player_geometry(area: Rect, player: Rect, has_audio: bool) -> PlayerGeometry {
    let fullscreen_picture = Rect::new(area.x, area.y, area.w, (area.h - 62.0).max(0.0));
    let mut width = 194.0;
    if has_audio {
        width += 38.0;
    }
    let actions = Rect::new(
        area.x + (area.w - width) / 2.0,
        area.bottom() - 52.0,
        width,
        42.0,
    );
    let (fullscreen_audio, fullscreen_exit) = if has_audio {
        (
            Some(Rect::new(actions.x, actions.y, 35.0, 42.0)),
            Rect::new(actions.x + 38.0, actions.y, 194.0, 42.0),
        )
    } else {
        (None, Rect::new(actions.x, actions.y, 194.0, 42.0))
    };
    PlayerGeometry {
        header: Rect::new(
            player.x + 4.0,
            player.y + 1.0,
            (player.w - 84.0).max(0.0),
            36.0,
        ),
        picture: Rect::new(
            player.x + 1.0,
            player.y + PLAYER_HEADER,
            (player.w - 2.0).max(0.0),
            (player.h - PLAYER_HEADER - PLAYER_FOOTER).max(0.0),
        ),
        corner: Rect::new(
            player.right() - PLAYER_CORNER,
            player.bottom() - PLAYER_CORNER,
            PLAYER_CORNER,
            PLAYER_CORNER,
        ),
        fullscreen: Rect::new(player.right() - 67.0, player.y + 4.0, 30.0, 30.0),
        close: Rect::new(player.right() - 35.0, player.y + 4.0, 30.0, 30.0),
        audio: has_audio
            .then(|| Rect::new(player.right() - 57.0, player.bottom() - 35.0, 32.0, 29.0)),
        fullscreen_picture,
        fullscreen_exit,
        fullscreen_audio,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPhase {
    /// The user is not in this room.
    Idle,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

/// Header text and tone of the room, from Go `layoutVoiceRoom`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Lime,
    Danger,
}

pub fn room_status(phase: CallPhase) -> (&'static str, Tone) {
    match phase {
        CallPhase::Idle => ("Голосовой канал", Tone::Muted),
        CallPhase::Connecting => ("Подключение…", Tone::Muted),
        CallPhase::Connected => ("Голосовая связь установлена", Tone::Lime),
        CallPhase::Reconnecting => ("Восстанавливаем соединение…", Tone::Muted),
        CallPhase::Failed => ("Голосовой канал", Tone::Muted),
    }
}

/// Sidebar dock line, from Go `layoutVoiceDock`.
pub fn dock_status(phase: CallPhase) -> (&'static str, Tone) {
    match phase {
        CallPhase::Connected => ("Голос подключён", Tone::Lime),
        CallPhase::Connecting => ("Подключение…", Tone::Muted),
        CallPhase::Reconnecting => ("Восстанавливаем соединение…", Tone::Muted),
        CallPhase::Failed => ("Голосовое соединение прервано", Tone::Danger),
        CallPhase::Idle => ("Голосовой канал", Tone::Muted),
    }
}

/// Participant badge: reconnecting > deafen > mute (local mute included).
pub fn voice_state_icon(reconnecting: bool, deaf: bool, muted: bool) -> Option<&'static str> {
    if reconnecting {
        Some("Переподключение")
    } else if deaf {
        Some("Звук выключен")
    } else if muted {
        Some("Микрофон выключен")
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceAction {
    pub id: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub toggle: bool,
}

/// Go `layoutVoiceActions`: one order for the toolbar and the dock.
pub const VOICE_ACTIONS: [VoiceAction; 4] = [
    VoiceAction {
        id: "voice-mic",
        label: "Микрофон",
        hint: "Выключить микрофон",
        toggle: true,
    },
    VoiceAction {
        id: "voice-deaf",
        label: "Звук",
        hint: "Выключить звук",
        toggle: true,
    },
    VoiceAction {
        id: "voice-records",
        label: "Записи",
        hint: "Записи сервера",
        toggle: false,
    },
    VoiceAction {
        id: "voice-leave",
        label: "Выйти",
        hint: "Выйти из голосового канала",
        toggle: false,
    },
];

/// Toolbar action rectangles in Go order and inset. Pure geometry used by the
/// layout tests; the live toolbar uses flex with the same gap and inset.
#[cfg(test)]
pub fn toolbar_buttons(toolbar: Rect, labels: bool) -> Vec<Rect> {
    let inset = if labels { TOOLBAR_INSET_LABELS } else { 0.0 };
    let count = VOICE_ACTIONS.len() as f32;
    let width = (toolbar.w - inset * 2.0 - TOOLBAR_GAP * (count - 1.0)).max(1.0) / count;
    (0..VOICE_ACTIONS.len())
        .map(|index| {
            Rect::new(
                toolbar.x + inset + index as f32 * (width + TOOLBAR_GAP),
                toolbar.y + inset,
                width,
                (toolbar.h - 2.0 * inset).max(1.0),
            )
        })
        .collect()
}

/// Microphone is closed until the join is confirmed, while the user is muted
/// or deafened, and for push-to-talk unless the key is held.
pub fn microphone_open(
    joined: bool,
    self_muted: bool,
    deafened: bool,
    push_to_talk: bool,
    held: bool,
    blocked: bool,
) -> bool {
    joined && !self_muted && !deafened && (!push_to_talk || (held && !blocked))
}

/// Parsed shortcut like Go `Ctrl+Shift+M`. Modifier comparisons are exact and
/// case-insensitive on the key name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub platform: bool,
    pub key: String,
}

impl Hotkey {
    pub fn parse(text: &str) -> Self {
        let mut hotkey = Hotkey::default();
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" => hotkey.ctrl = true,
                "shift" => hotkey.shift = true,
                "alt" => hotkey.alt = true,
                "super" | "win" | "cmd" | "meta" => hotkey.platform = true,
                _ => hotkey.key = part.to_lowercase(),
            }
        }
        hotkey
    }

    pub fn matches(&self, key: &str, ctrl: bool, shift: bool, alt: bool, platform: bool) -> bool {
        self.key == key.to_lowercase()
            && self.ctrl == ctrl
            && self.shift == shift
            && self.alt == alt
            && self.platform == platform
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerKey {
    ToggleFullscreen,
    ExitFullscreen,
    Ignore,
}

/// Go `streamPlayerEvent`: Esc leaves fullscreen, `F` without modifiers toggles
/// it when the player is fullscreen or its button is focused.
pub fn player_key(
    key: &str,
    ctrl: bool,
    shift: bool,
    alt: bool,
    platform: bool,
    fullscreen: bool,
    fullscreen_focused: bool,
) -> PlayerKey {
    if key.eq_ignore_ascii_case("escape") && fullscreen {
        return PlayerKey::ExitFullscreen;
    }
    if key.eq_ignore_ascii_case("f")
        && !ctrl
        && !shift
        && !alt
        && !platform
        && (fullscreen || fullscreen_focused)
    {
        return PlayerKey::ToggleFullscreen;
    }
    PlayerKey::Ignore
}

/// Player status line: an explicit error wins, then the connection state, then
/// the Go "Ожидаем первый кадр…" placeholder.
pub fn player_status(status: &str, error: &str, has_frame: bool, connected: bool) -> String {
    if !error.is_empty() {
        return error.to_owned();
    }
    if has_frame && connected {
        return String::new();
    }
    if status.is_empty() || status == "Подключено" {
        "Ожидаем первый кадр…".into()
    } else {
        status.to_owned()
    }
}

#[cfg(test)]
mod tests;
