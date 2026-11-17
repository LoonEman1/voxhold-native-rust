//! Geometry, keyboard and microphone rules of the voice UI. Every value comes
//! from the Go reference, so these tests pin the layout without a window.

use super::*;

#[test]
fn floating_player_starts_at_360_and_keeps_the_go_aspect() {
    let area = Rect::new(0.0, 67.0, 1440.0, 833.0);
    let player = floating_stream_box(area, (1000.0, 400.0), PLAYER_DEFAULT_WIDTH);
    assert_eq!(player.w, 360.0);
    assert!((player.h - (360.0 * 9.0 / 16.0 + 76.0)).abs() < 0.01);
    // 38 px header + picture + 38 px footer.
    assert_eq!(player.h - PLAYER_HEADER - PLAYER_FOOTER, 360.0 * 9.0 / 16.0);
}

#[test]
fn the_player_is_clamped_into_the_area_with_an_eight_pixel_margin() {
    let area = Rect::new(0.0, 0.0, 800.0, 600.0);
    let far = floating_stream_box(area, (5000.0, 5000.0), 720.0);
    assert!(far.right() <= area.right() - PLAYER_MARGIN + 0.01);
    assert!(far.bottom() <= area.bottom() - PLAYER_MARGIN + 0.01);
    let near = floating_stream_box(area, (-50.0, -50.0), 300.0);
    assert_eq!((near.x, near.y), (PLAYER_MARGIN, PLAYER_MARGIN));
}

#[test]
fn the_player_width_stays_between_260_and_720_or_fits_a_small_area() {
    let wide = Rect::new(0.0, 0.0, 1600.0, 1200.0);
    assert_eq!(
        floating_stream_box(wide, (0.0, 0.0), 10.0).w,
        PLAYER_MIN_WIDTH
    );
    assert_eq!(
        floating_stream_box(wide, (0.0, 0.0), 5000.0).w,
        PLAYER_MAX_WIDTH
    );
    // A short area limits the height and therefore the width.
    let short = Rect::new(0.0, 0.0, 1600.0, 400.0);
    let player = floating_stream_box(short, (0.0, 0.0), 5000.0);
    assert!(player.h <= 400.0 - 92.0 + 76.0 + 0.01);
    // A narrow area keeps an 8 px margin on both sides.
    let narrow = Rect::new(0.0, 0.0, 240.0, 600.0);
    let player = floating_stream_box(narrow, (0.0, 0.0), 5000.0);
    assert!(player.w <= 240.0 - 16.0);
    assert!(player.x >= 8.0);
}

#[test]
fn player_regions_match_the_go_controls() {
    let area = Rect::new(0.0, 0.0, 1440.0, 900.0);
    let player = Rect::new(100.0, 200.0, 360.0, 278.5);
    let geometry = player_geometry(area, player, true);
    assert_eq!(geometry.header.x, 104.0);
    assert_eq!(geometry.header.h, 36.0);
    assert_eq!(geometry.picture.y, player.y + 38.0);
    assert_eq!(geometry.picture.h, player.h - 76.0);
    assert_eq!(geometry.corner, Rect::new(445.0, 463.5, 15.0, 15.0));
    assert_eq!(geometry.fullscreen, Rect::new(393.0, 204.0, 30.0, 30.0));
    assert_eq!(geometry.close, Rect::new(425.0, 204.0, 30.0, 30.0));
    assert_eq!(geometry.audio, Some(Rect::new(403.0, 443.5, 32.0, 29.0)));
    // 194 + 38 px audio button, centred in the 1440 px area.
    assert_eq!(
        geometry.fullscreen_exit,
        Rect::new(642.0, 848.0, 194.0, 42.0)
    );
    assert_eq!(geometry.fullscreen_picture.h, 838.0);
    // Without audio there is no mute button and the exit button shifts left.
    let quiet = player_geometry(area, player, false);
    assert!(quiet.audio.is_none());
    assert_eq!(quiet.fullscreen_exit.x, 623.0);
    assert_eq!(quiet.fullscreen_picture.w, area.w);
}

#[test]
fn card_grid_matches_go_sizes_and_columns() {
    // 1200 wide, active with a toolbar: width 1152, so three 280 cards fit
    // because Go divides by the card plus gap.
    let geometry = room_geometry(Rect::new(0.0, 67.0, 1200.0, 833.0), true, 5, 0.0);
    assert_eq!(geometry.card_size, CARD_MAX);
    assert_eq!(geometry.columns, 3);
    assert_eq!(geometry.cards.len(), 5);
    assert_eq!(
        geometry.cards[1].x - geometry.cards[0].x,
        CARD_MAX + CARD_GAP
    );
    assert_eq!(
        geometry.cards[4].y - geometry.cards[0].y,
        CARD_MAX + CARD_GAP
    );
    assert!(geometry.toolbar.w <= TOOLBAR_MAX_WIDTH);
    assert_eq!(geometry.toolbar.h, TOOLBAR_HEIGHT);
    // Narrow rooms fall back to a single column.
    let narrow = room_geometry(Rect::new(0.0, 67.0, 300.0, 500.0), false, 2, 0.0);
    assert_eq!(narrow.columns, 1);
    assert!(narrow.card_size <= 252.0);
    // Scrolling is bounded by the content height.
    let scrolled = room_geometry(Rect::new(0.0, 67.0, 1200.0, 400.0), true, 20, 5000.0);
    let last = scrolled.cards.last().unwrap();
    assert!(last.bottom() <= scrolled.viewport.bottom() + 0.01);
}

#[test]
fn the_toolbar_keeps_the_go_action_order_and_inset() {
    let toolbar = Rect::new(100.0, 700.0, 330.0, 61.0);
    let buttons = toolbar_buttons(toolbar, true);
    assert_eq!(buttons.len(), 4);
    let ids: Vec<_> = VOICE_ACTIONS.iter().map(|action| action.id).collect();
    assert_eq!(
        ids,
        vec!["voice-mic", "voice-deaf", "voice-records", "voice-leave"]
    );
    assert_eq!(buttons[0].x, toolbar.x + 7.0);
    assert_eq!(buttons[0].y, toolbar.y + 7.0);
    assert!(buttons[0].right() < buttons[1].x);
    assert!((buttons[3].right() - (toolbar.right() - 7.0)).abs() < 0.01);
    // The dock has no labels and no inset.
    let dock = toolbar_buttons(Rect::new(0.0, 0.0, 200.0, 31.0), false);
    assert_eq!(dock[0].x, 0.0);
    assert_eq!(dock[0].y, 0.0);
}

#[test]
fn room_and_dock_titles_follow_the_call_phase() {
    assert_eq!(
        room_status(CallPhase::Idle),
        ("Голосовой канал", Tone::Muted)
    );
    assert_eq!(
        room_status(CallPhase::Connected),
        ("Голосовая связь установлена", Tone::Lime)
    );
    assert_eq!(
        room_status(CallPhase::Reconnecting).0,
        "Восстанавливаем соединение…"
    );
    assert_eq!(
        dock_status(CallPhase::Connected),
        ("Голос подключён", Tone::Lime)
    );
    assert_eq!(voice_state_icon(false, false, false), None);
    assert_eq!(voice_state_icon(true, true, true), Some("Переподключение"));
    assert_eq!(voice_state_icon(false, true, false), Some("Звук выключен"));
    assert_eq!(
        voice_state_icon(false, false, true),
        Some("Микрофон выключен")
    );
}

#[test]
fn microphone_opens_only_for_a_confirmed_unmuted_join() {
    assert!(!microphone_open(false, false, false, false, false, false));
    assert!(microphone_open(true, false, false, false, false, false));
    assert!(!microphone_open(true, true, false, false, false, false));
    assert!(!microphone_open(true, false, true, false, false, false));
    // Push-to-talk is closed until the key is held and not blocked.
    assert!(!microphone_open(true, false, false, true, false, false));
    assert!(microphone_open(true, false, false, true, true, false));
    assert!(!microphone_open(true, false, false, true, true, true));
    // An unknown state (no join) stays closed even while held.
    assert!(!microphone_open(false, false, false, true, true, false));
}

#[test]
fn hotkeys_parse_exactly_like_the_go_form() {
    let hotkey = Hotkey::parse("Ctrl+Shift+M");
    assert!(hotkey.ctrl && hotkey.shift && !hotkey.alt);
    assert_eq!(hotkey.key, "m");
    assert!(hotkey.matches("m", true, true, false, false));
    assert!(!hotkey.matches("m", true, false, false, false));
    assert!(!hotkey.matches("n", true, true, false, false));
    assert_eq!(Hotkey::parse("Space").key, "space");
    assert_eq!(Hotkey::parse(""), Hotkey::default());
}

#[test]
fn player_keys_follow_stream_player_event() {
    assert_eq!(
        player_key("escape", false, false, false, false, true, false),
        PlayerKey::ExitFullscreen
    );
    assert_eq!(
        player_key("escape", false, false, false, false, false, false),
        PlayerKey::Ignore,
        "Go only closes fullscreen on Esc"
    );
    assert_eq!(
        player_key("f", false, false, false, false, false, true),
        PlayerKey::ToggleFullscreen
    );
    assert_eq!(
        player_key("f", false, false, false, false, false, false),
        PlayerKey::Ignore
    );
    assert_eq!(
        player_key("f", true, false, false, false, true, true),
        PlayerKey::Ignore
    );
    assert_eq!(
        player_key("f", false, false, false, false, true, false),
        PlayerKey::ToggleFullscreen
    );
}

#[test]
fn player_status_prefers_errors_then_waiting_for_the_first_frame() {
    assert_eq!(player_status("", "", false, false), "Ожидаем первый кадр…");
    assert_eq!(
        player_status("Подключено", "", false, true),
        "Ожидаем первый кадр…"
    );
    assert_eq!(
        player_status("Подключение…", "", false, false),
        "Подключение…"
    );
    assert_eq!(player_status("Подключено", "", true, true), "");
    assert_eq!(
        player_status("Подключено", "Трансляция завершена", true, true),
        "Трансляция завершена"
    );
}
