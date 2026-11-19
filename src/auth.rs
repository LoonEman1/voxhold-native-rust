//! Two-step sign-in: instance address, then credentials.
//!
//! Behaviour, validation texts and geometry are ported from the Go client's
//! `cmd/voxhold/auth.go`. The form holds no password after a successful login
//! and a late answer is applied only to the attempt that started it.

use crate::layout::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthScreen {
    Instance,
    Login,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LoginRequest {
    pub attempt: u64,
    pub origin: String,
    pub username: String,
    pub password: String,
}

/// A derived `Debug` would print the password. Diagnostics keep the attempt and
/// the identity, never the credential.
impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("attempt", &self.attempt)
            .field("origin", &self.origin)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// State that survives a repaint of the auth page.
#[derive(Clone, Debug)]
pub struct AuthState {
    pub screen: AuthScreen,
    pub origin: String,
    pub username: String,
    pub status: String,
    pub busy: bool,
    /// The «Запомнить адрес» checkbox. The value is persisted only after the
    /// address step is confirmed; toggling alone never writes a file.
    pub remember_instance: bool,
    /// The «Запомнить вход» checkbox on the password label row. It only marks
    /// the intent; the record is written after a successful login.
    pub remember_login: bool,
    /// A stored session is being checked right now. The form stays usable and
    /// a manual attempt supersedes it.
    pub auto_login: bool,
    /// The last automatic attempt kept the record but could not reach the
    /// server. The login card then offers «Повторить».
    pub auto_retry: bool,
    login_attempt: u64,
}

impl AuthState {
    pub fn new(prefill: &str) -> Self {
        Self {
            screen: AuthScreen::Instance,
            origin: prefill.trim().to_string(),
            username: String::new(),
            status: String::new(),
            busy: false,
            remember_instance: false,
            remember_login: false,
            auto_login: false,
            auto_retry: false,
            login_attempt: 0,
        }
    }

    /// The `-server` flag and a remembered origin only prefill the address; the
    /// operator still confirms it. Go keeps the same first screen.
    pub fn with_prefill(prefill: &str) -> Self {
        Self::new(prefill)
    }

    pub fn is_instance(&self) -> bool {
        self.screen == AuthScreen::Instance
    }

    /// Number of credential attempts started by this form. Used by the UI test
    /// that checks double Enter does not open two requests.
    #[allow(dead_code)]
    pub fn login_attempt(&self) -> u64 {
        self.login_attempt
    }

    /// Validate and commit the address. Returns the trimmed origin on success.
    pub fn submit_instance(&mut self, raw: &str) -> Option<String> {
        if self.busy {
            return None;
        }
        let origin = raw.trim();
        let client = match voxhold_api::Client::new(origin) {
            Ok(client) => client,
            Err(_) => {
                self.status = "Введите корректный HTTPS-адрес инстанса".into();
                return None;
            }
        };
        // Keep one canonical origin: no trailing slash and no default port.
        self.origin = client.origin();
        self.status.clear();
        self.screen = AuthScreen::Login;
        Some(self.origin.clone())
    }

    /// Go trims the user name but keeps password spaces verbatim.
    pub fn submit_login(&mut self, raw_user: &str, password: &str) -> Option<LoginRequest> {
        if self.busy {
            return None;
        }
        let user = clamp_chars(raw_user.trim(), 32);
        let pass = clamp_chars(password, 72);
        if user.chars().count() < 3 {
            self.status = "Введите имя пользователя (не менее 3 символов)".into();
            return None;
        }
        if pass.chars().count() < 8 {
            self.status = "Введите пароль (не менее 8 символов)".into();
            return None;
        }
        self.username = user.clone();
        self.status.clear();
        self.busy = true;
        self.login_attempt += 1;
        Some(LoginRequest {
            attempt: self.login_attempt,
            origin: self.origin.clone(),
            username: user,
            password: pass,
        })
    }

    /// Apply a finished login. Returns `false` for an answer that belongs to an
    /// older attempt or a form that is no longer pending.
    pub fn finish_login(&mut self, attempt: u64, result: &Result<(), String>) -> bool {
        if attempt != self.login_attempt || !self.busy {
            return false;
        }
        self.busy = false;
        match result {
            Ok(()) => true,
            Err(message) => {
                self.status = message.clone();
                false
            }
        }
    }

    /// Explicit retry after a failed attempt keeps the password field contents
    /// owned by the caller; only pending and status reset here.
    #[allow(dead_code)]
    pub fn reset_pending(&mut self) {
        self.busy = false;
    }

    /// Map a transport error to the Go user-facing text.
    pub fn describe_error(error: &voxhold_api::Error) -> String {
        match error {
            voxhold_api::Error::Http(401) => "Неверное имя пользователя или пароль".into(),
            voxhold_api::Error::Network => "Не удалось связаться с сервером".into(),
            _ => "Не удалось войти. Попробуйте ещё раз.".into(),
        }
    }
}

fn clamp_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// Result of the auth page layout. Field rectangles are absolute inside the
/// client area, matching the Go `authScreen.Layout` placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthLayout {
    pub story: f32,
    pub card: Rect,
    pub pad: f32,
    pub radius: f32,
    pub inner: Rect,
    pub heading_height: f32,
    pub user: Rect,
    pub password: Rect,
    pub primary: Rect,
    /// «Запомнить адрес» on the instance step. Empty on the login step, where
    /// the geometry must stay identical to the reference.
    pub remember: Rect,
    pub toggle: Rect,
    pub error: Rect,
    pub preview: Rect,
    pub brand: Rect,
    /// The «Назад» link and the confirmed origin above the login card. They are
    /// separate rectangles so the address can never sit under the link or the
    /// theme toggle.
    pub back: Rect,
    pub origin: Rect,
    pub username_label: Rect,
    pub password_label: Rect,
    /// «Запомнить вход» on the password label row. Empty on the address step:
    /// the login card keeps its reference height and field positions.
    pub remember_login: Rect,
    pub terms: Rect,
}

/// Smallest top margin a short client area may squeeze the card to. The mobile
/// brand strip sits at `card.y + pad` and the theme toggle at `y = 22`, so the
/// card still has to start below the window edge to keep both readable.
const MIN_AUTH_TOP: f32 = 8.0;

/// The confirmed-origin row above the login card: `y = 22`, height 32. When the
/// card is pulled up, it must not cover that row.
const TOP_STRIP_Y: f32 = 22.0;
const TOP_STRIP_HEIGHT: f32 = 32.0;

pub fn layout_auth(w: f32, h: f32, instance: bool, has_error: bool) -> AuthLayout {
    let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
    let h = if h.is_finite() { h.max(0.0) } else { 0.0 };
    let story = if w > 980.0 {
        w * 0.55
    } else if w > 720.0 {
        w / 2.0
    } else {
        0.0
    };
    let (mut left, mut right, mut top, mut bottom) = (
        (w * 0.03).clamp(16.0, 48.0),
        (w * 0.05).clamp(28.0, 76.0),
        72.0f32,
        44.0f32,
    );
    if w <= 980.0 {
        left = 12.0;
        right = 24.0;
    }
    if w <= 720.0 {
        left = 16.0;
        right = 16.0;
        top = 70.0;
        bottom = 28.0;
    }
    let card_width = 486.0f32.min((w - story - left - right).max(0.0));
    let card_x = story + left + (w - story - left - right - card_width) / 2.0;
    let pad = (w * 0.034).clamp(24.0, 44.0) + 1.0;
    let width = (card_width - 2.0 * pad).max(0.0);
    let font = (w * 0.03).clamp(32.0, 42.0);
    let mut heading_height = 64.0 + font * 1.08;
    if width < 330.0 {
        heading_height += 24.0;
    }
    let mut content_height = heading_height + 315.2;
    if w <= 720.0 {
        content_height += 63.0;
    }
    if instance {
        content_height = 253.0;
        if w <= 720.0 {
            content_height += 63.0;
        }
    }
    if has_error {
        content_height += 57.0;
    }
    let card_height = content_height + 2.0 * pad;
    let card_y = top.max(top + (h - top - bottom - card_height) / 2.0);
    // A short client area must not push the primary button out of the window.
    // On mobile the +63 px brand strip and the +25 px checkbox row make the
    // address card taller than the reference assumes, so when the centred card
    // does not fit the top margin is reduced (never below the step's floor)
    // until the card ends as high as the height allows. Normal 900/800 px
    // windows fit already and keep the exact reference centring. The login
    // step reserves the top strip for the confirmed origin, so its floor is
    // higher than the address step's.
    let clamp_floor = if instance {
        MIN_AUTH_TOP
    } else {
        TOP_STRIP_Y + TOP_STRIP_HEIGHT
    };
    let card_y = if card_y + card_height > h {
        (h - card_height).max(clamp_floor)
    } else {
        card_y
    };
    let card = Rect::new(card_x, card_y, card_width, card_height);
    let mut x = card_x + pad;
    let mut y = card_y + pad;
    if w <= 720.0 {
        y += 63.0;
    }
    let user;
    let mut password = Rect::new(0.0, 0.0, 0.0, 0.0);
    let remember;
    let primary;
    if instance {
        user = Rect::new(x, y + 110.0, width, 47.0);
        // The checkbox is a separate control under the address field. The
        // primary button moves down by the row it adds, and the card grows by
        // the same amount so nothing overlaps at any breakpoint.
        remember = Rect::new(x, y + 169.0, width, 20.0);
        primary = Rect::new(x, y + 203.0, width, 50.0);
    } else {
        user = Rect::new(x, y + heading_height + 51.0, width, 47.0);
        password = Rect::new(x, y + heading_height + 140.0, width, 47.0);
        remember = Rect::new(0.0, 0.0, 0.0, 0.0);
        let error_offset = if has_error { 57.0 } else { 0.0 };
        primary = Rect::new(x, y + heading_height + 208.0 + error_offset, width, 50.0);
    }
    let toggle = Rect::new(w - 56.0, 22.0, 34.0, 34.0);
    // The confirmed origin is plain text above the card. It gets its own row
    // over the «Назад» link: a long address may wrap, but it never shares the
    // link's box. When the card starts high enough the row sits above the link;
    // otherwise it moves to the top strip and stops before the theme toggle.
    let back = Rect::new(card_x, (card_y - 28.0).max(8.0), 54.0, 16.0);
    let origin_y = if card_y - 48.0 >= 60.0 {
        card_y - 48.0
    } else {
        TOP_STRIP_Y
    };
    let origin_right = if origin_y < 60.0 {
        (w - 68.0).min(card.right())
    } else {
        card.right()
    };
    let origin_left = (card_x + 68.0).min(origin_right);
    let origin = Rect::new(
        origin_left,
        origin_y,
        (origin_right - origin_left).max(0.0),
        TOP_STRIP_HEIGHT,
    );
    let error_y = if instance {
        primary.bottom() + 12.0
    } else {
        primary.y - 57.0
    };
    let error = Rect::new(x, error_y, width, 40.0);
    let terms = Rect::new(x, primary.bottom() + 20.0, width, 40.0);
    let preview = Rect::new(
        (w * 0.044).clamp(30.0, 72.0),
        h - (w * 0.044).clamp(30.0, 72.0) - 16.0 - 177.0 - story_gap(h, story, w),
        (story - 2.0 * (w * 0.044).clamp(30.0, 72.0)).clamp(0.0, 490.0),
        177.0,
    );
    let brand_y = if story == 0.0 {
        y - 63.0
    } else {
        (w * 0.044).clamp(30.0, 72.0)
    };
    let brand = Rect::new(if story == 0.0 { x } else { brand_y }, brand_y, 39.0, 39.0);
    let username_label = Rect::new(x, user.y - 25.0, width, 17.0);
    let password_label = Rect::new(x, password.y - 25.0, width, 17.0);
    // «Запомнить вход» shares the password label row. The 18 px box, an 8 px
    // gap and the label need 118 px; the row is right-aligned so the «Пароль»
    // caption on the left is never covered and no vertical space is added.
    let remember_login = if instance {
        Rect::new(0.0, 0.0, 0.0, 0.0)
    } else {
        let row = 118.0f32.min(width.max(0.0));
        Rect::new(
            x + (width - row).max(0.0),
            password_label.y - 0.5,
            row,
            18.0,
        )
    };
    // Keep offsets used by the heading stable across renders.
    if instance {
        y += 0.0;
        x += 0.0;
    }
    AuthLayout {
        story,
        card,
        pad,
        radius: if w <= 720.0 { 22.0 } else { 26.0 },
        inner: Rect::new(x, y, width, card.bottom() - pad - y),
        heading_height,
        user,
        password,
        primary,
        remember,
        toggle,
        error,
        preview,
        brand,
        back,
        origin,
        username_label,
        password_label,
        remember_login,
        terms,
    }
}

/// Vertical gap used by the Go story column, needed to keep the preview where
/// the reference puts it.
fn story_gap(h: f32, story: f32, w: f32) -> f32 {
    let pad = (w * 0.044).clamp(30.0, 72.0);
    let font = (w * 0.059).clamp(48.0, 100.0);
    let body_size = (w * 0.012).clamp(15.0, 18.0);
    let inner = story - 2.0 * pad;
    let lines = if inner < 340.0 { 3.0 } else { 2.0 };
    let copy_height = 58.0 + 3.0 * font * 0.96 + lines * body_size * 1.65;
    let (preview_height, spaces) = if w > 980.0 { (177.0, 3.0) } else { (0.0, 2.0) };
    ((h * 0.035).clamp(22.0, 36.0))
        .max((h - 2.0 * pad - 39.0 - copy_height - preview_height - 16.0) / spaces)
}

/// Story-column metrics from Go `drawStory`. The decorative copy is rendered
/// with the same font sizes and vertical rhythm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoryLayout {
    pub pad: f32,
    pub font: f32,
    pub body: f32,
    pub brand: Rect,
    pub tagline: Rect,
    pub line1: Rect,
    pub line2: Rect,
    pub line3: Rect,
    pub paragraph: Rect,
    pub preview: Rect,
    pub footer: Rect,
}

pub fn story_layout(w: f32, h: f32) -> StoryLayout {
    let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
    let h = if h.is_finite() { h.max(0.0) } else { 0.0 };
    let story = if w > 980.0 {
        w * 0.55
    } else if w > 720.0 {
        w / 2.0
    } else {
        0.0
    };
    story_metrics(w, h, story)
}

fn story_metrics(w: f32, h: f32, story: f32) -> StoryLayout {
    let pad = (w * 0.044).clamp(30.0, 72.0);
    let font = (w * 0.059).clamp(48.0, 100.0);
    let body = (w * 0.012).clamp(15.0, 18.0);
    let inner = (story - 2.0 * pad).max(0.0);
    let lines = if inner < 340.0 { 3.0 } else { 2.0 };
    let copy_height = 58.0 + 3.0 * font * 0.96 + lines * body * 1.65;
    let (preview_height, spaces) = if w > 980.0 { (177.0, 3.0) } else { (0.0, 2.0) };
    let gap = (h * 0.035)
        .clamp(22.0, 36.0)
        .max((h - 2.0 * pad - 39.0 - copy_height - preview_height - 16.0) / spaces);
    let mut y = pad + 39.0 + gap + 6.0;
    let tagline = Rect::new(pad + 16.0, y, (inner - 16.0).max(0.0), 17.0);
    y += 42.0;
    let line1 = Rect::new(pad, y, inner, font);
    y += font * 0.96;
    let line2 = Rect::new(pad, y, inner, font);
    y += font * 0.96;
    let line3 = Rect::new(pad, y, inner, font);
    y += font + 9.0;
    let paragraph = Rect::new(pad, y, inner.min(480.0), body * 1.65 * 3.0);
    let preview = Rect::new(pad, h - pad - 16.0 - gap - 177.0, (inner).min(490.0), 177.0);
    let footer = Rect::new(pad, h - pad - 16.0, inner.min(490.0), 16.0);
    StoryLayout {
        pad,
        font,
        body,
        brand: Rect::new(pad, pad, 39.0, 39.0),
        tagline,
        line1,
        line2,
        line3,
        paragraph,
        preview,
        footer,
    }
}

/// Story-column metrics from Go `drawStory`. The decorative copy is rendered
/// with the same font sizes and vertical rhythm.
#[cfg(test)]
mod story_tests {
    use super::*;

    #[test]
    fn story_metrics_follow_the_reference_breakpoints() {
        let wide = story_layout(1280.0, 900.0);
        assert!((wide.pad - 56.32).abs() < 0.01);
        assert!((wide.font - 75.52).abs() < 0.01);
        assert!((wide.body - 15.36).abs() < 0.01);
        assert_eq!(wide.brand.w, 39.0);
        assert!(wide.paragraph.y > wide.line3.y);
        assert!(wide.preview.y > wide.paragraph.y);

        let medium = story_layout(981.0, 900.0);
        assert!((medium.font - 57.879).abs() < 0.01);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_breakpoints_match_go() {
        for (width, story) in [
            (1280.0, 704.0),
            (981.0, 539.55),
            (980.0, 490.0),
            (721.0, 360.5),
            (720.0, 0.0),
        ] {
            let layout = layout_auth(width, 900.0, false, false);
            assert!(
                (layout.story - story).abs() <= 0.01,
                "width {width}: story {}, want {story}",
                layout.story
            );
        }
    }

    #[test]
    fn login_card_geometry_matches_go() {
        for (width, x, y, card_width, field_x, field_y) in [
            (1280.0, 742.39, 209.16, 473.61, 786.91, 410.14),
            (981.0, 568.97, 210.78, 362.97, 603.31, 418.67),
            (980.0, 502.0, 222.81, 454.0, 536.31, 406.67),
            (721.0, 372.5, 219.63, 324.5, 398.0, 418.67),
            (720.0, 117.0, 207.16, 486.0, 142.47, 445.17),
        ] {
            let layout = layout_auth(width, 900.0, false, false);
            for (got, want) in [
                (layout.card.x, x),
                (layout.card.y, y),
                (layout.card.w, card_width),
                (layout.user.x, field_x),
                (layout.user.y, field_y),
            ] {
                assert!(
                    (got - want).abs() <= 1.0,
                    "width {width}: got {got:.2}, want {want:.2}"
                );
            }
        }
    }

    #[test]
    fn instance_step_uses_the_short_card() {
        let login = layout_auth(1280.0, 900.0, false, false);
        let instance = layout_auth(1280.0, 900.0, true, false);
        assert!(instance.card.h < login.card.h);
        assert!(login.card.h - instance.card.h > 150.0);
        assert_eq!(instance.user.h, 47.0);
        assert_eq!(instance.primary.h, 50.0);
    }

    #[test]
    fn remember_checkbox_stays_between_the_field_and_the_button() {
        for (width, height) in [
            (390.0, 760.0),
            (560.0, 800.0),
            (720.0, 900.0),
            (1280.0, 900.0),
            // A short window clamps the card to the top; the row must still
            // stay inside the card and clear of both neighbours.
            (390.0, 600.0),
            // The minimum window size: the card itself may not fit, but the
            // checkbox must stay inside the card and the client area.
            (320.0, 360.0),
        ] {
            for has_error in [false, true] {
                let layout = layout_auth(width, height, true, has_error);
                let row = layout.remember;
                assert!(
                    row.w > 100.0,
                    "{width}x{height}: checkbox column is {} wide",
                    row.w
                );
                assert!(row.h >= 18.0, "{width}x{height}: checkbox row too short");
                assert!(
                    row.y >= layout.user.bottom(),
                    "{width}x{height}: checkbox covers the address field"
                );
                assert!(
                    row.bottom() <= layout.primary.y,
                    "{width}x{height}: checkbox covers the button"
                );
                assert!(
                    row.x >= layout.card.x
                        && row.right() <= layout.card.right()
                        && row.y >= layout.card.y
                        && row.bottom() <= layout.card.bottom(),
                    "{width}x{height}: checkbox leaves the card"
                );
                assert!(
                    row.bottom() <= height && row.right() <= width,
                    "{width}x{height}: checkbox leaves the client area"
                );
            }
        }
        // The login step keeps its original geometry and has no checkbox.
        let login = layout_auth(1280.0, 900.0, false, false);
        assert!(login.remember.is_empty());
    }

    #[test]
    fn a_short_window_pulls_the_card_up_so_the_primary_button_stays_visible() {
        // 390x390 and the minimum 320x360: the instance card is 366 px tall
        // (253 px content + 63 px brand strip + 2 * 25 px padding), so the
        // reference centring used to push «Продолжить» 21 px below the client.
        // The clamp moves the card up to its minimum top margin instead.
        for (width, height) in [(390.0, 390.0), (320.0, 360.0)] {
            for has_error in [false, true] {
                let layout = layout_auth(width, height, true, has_error);
                assert!(
                    layout.primary.bottom() <= height,
                    "{width}x{height}: «Продолжить» leaves the client (bottom {})",
                    layout.primary.bottom()
                );
                assert!(
                    layout.card.y >= MIN_AUTH_TOP,
                    "{width}x{height}: card starts at {}",
                    layout.card.y
                );
                assert!(
                    layout.card.y <= 70.0,
                    "{width}x{height}: a short window only moves the card up"
                );
                assert!(
                    layout.user.bottom() <= layout.remember.y
                        && layout.remember.bottom() <= layout.primary.y,
                    "{width}x{height}: the rows overlap after the clamp"
                );
                assert!(
                    layout.brand.bottom() <= layout.user.y,
                    "{width}x{height}: the brand covers the address field"
                );
                assert!(
                    !overlaps(layout.toggle, layout.brand),
                    "{width}x{height}: the toggle covers the mobile brand"
                );
            }
        }
        // The card itself is 366 px tall, which is 6 px more than the 360 px
        // client; only the button can be guaranteed. 390x390 fits the card
        // exactly at its minimum margin.
        let tight = layout_auth(390.0, 390.0, true, false);
        assert!(tight.card.bottom() <= 390.0);
        assert!((tight.card.y - 24.0).abs() < 0.01);

        // Normal heights keep the reference geometry untouched.
        for (width, height) in [
            (1280.0, 900.0),
            (900.0, 800.0),
            (720.0, 900.0),
            (560.0, 800.0),
        ] {
            let layout = layout_auth(width, height, true, false);
            assert!(
                layout.card.bottom() <= height,
                "{width}x{height}: a fitting card must not be clipped"
            );
            assert!(
                layout.card.y >= 70.0,
                "{width}x{height}: the card moved above its normal top margin"
            );
            assert!(layout.primary.bottom() <= height);
        }
    }

    #[test]
    fn save_error_text_fits_the_narrow_error_box() {
        // The error row is painted with 12 px side padding, an 8 px gap and a
        // 16 px «×» glyph. 13 px Inter advances at most ~8 px per Cyrillic
        // character, a deliberately pessimistic bound. The phrase must wrap to
        // two lines and two lines must fit the 40 px row at 320 px width.
        let layout = layout_auth(320.0, 360.0, true, true);
        let inner = (layout.error.w - 2.0 * 12.0 - 16.0 - 8.0).max(1.0);
        let chars = crate::instance_prefs::SAVE_ERROR.chars().count() as f32;
        let lines = (chars * 8.0 / inner).ceil();
        assert!(
            lines <= 2.0,
            "«{}» needs {lines} lines in {inner} px",
            crate::instance_prefs::SAVE_ERROR
        );
        assert!(
            lines * 13.0 * 1.35 <= layout.error.h,
            "two lines do not fit the {} px row",
            layout.error.h
        );
        // The same box is fully visible at 390x600, where the save error is
        // actually read.
        let roomy = layout_auth(390.0, 600.0, true, true);
        assert!(roomy.error.bottom() <= 600.0);
        assert!(roomy.error.w >= 200.0);
    }

    #[test]
    fn remember_login_stays_on_the_password_label_row() {
        for (width, height) in [
            (320.0, 360.0),
            (390.0, 760.0),
            (720.0, 900.0),
            (1280.0, 900.0),
        ] {
            for has_error in [false, true] {
                let layout = layout_auth(width, height, false, has_error);
                let row = layout.remember_login;
                assert_eq!(row.h, 18.0);
                assert!(row.w > 100.0, "{width}x{height}: row is {} wide", row.w);
                assert!(
                    row.x >= layout.password_label.x
                        && row.right() <= layout.password_label.right(),
                    "{width}x{height}: the checkbox leaves the label row"
                );
                assert!(
                    (row.y - (layout.password_label.y - 0.5)).abs() < 0.01,
                    "{width}x{height}: the checkbox moved off the label row"
                );
                // The row sits between the password field and the button, and
                // the caption on the left keeps at least 40 px of clear space.
                assert!(row.bottom() <= layout.password.y);
                assert!(row.x - layout.password_label.x >= 40.0);
                assert!(row.y >= layout.card.y && row.bottom() <= layout.card.bottom());
            }
            // The address step has no login checkbox and keeps its geometry.
            assert!(layout_auth(width, height, true, false)
                .remember_login
                .is_empty());
        }
    }

    #[test]
    fn error_adds_a_row_and_moves_the_button() {
        let clean = layout_auth(1280.0, 900.0, false, false);
        let failed = layout_auth(1280.0, 900.0, false, true);
        assert!((failed.card.h - clean.card.h - 57.0).abs() < 0.01);
        // The taller card is re-centred, so the button only moves down by the
        // added row minus half of the height difference.
        assert!((failed.primary.y - clean.primary.y - 28.5).abs() < 0.01);
        assert!((failed.error.y - (failed.primary.y - 57.0)).abs() < 0.01);
    }

    #[test]
    fn narrow_width_drops_the_story_and_brand_moves_above_the_card() {
        let layout = layout_auth(600.0, 800.0, false, false);
        assert_eq!(layout.story, 0.0);
        assert_eq!(layout.radius, 22.0);
        assert!((layout.brand.y + 63.0 - layout.inner.y).abs() < 0.01);
        assert_eq!(layout.card.x, 57.0);
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
    }

    #[test]
    fn origin_never_covers_the_back_link_or_the_theme_toggle() {
        for (width, height) in [
            (390.0, 760.0),
            (560.0, 800.0),
            (720.0, 900.0),
            (1280.0, 900.0),
            // A short window clamps the card to the top; the origin row then
            // moves to the top strip and must still stop before the toggle.
            (390.0, 600.0),
        ] {
            let layout = layout_auth(width, height, false, false);
            assert!(
                layout.origin.w > 100.0,
                "{width}x{height}: origin column is {} wide",
                layout.origin.w
            );
            assert!(
                !overlaps(layout.origin, layout.back),
                "{width}x{height}: origin covers «Назад»"
            );
            assert!(
                !overlaps(layout.origin, layout.toggle),
                "{width}x{height}: origin covers the theme toggle"
            );
            assert!(
                !overlaps(layout.origin, layout.card),
                "{width}x{height}: origin covers the card"
            );
            assert!(
                !overlaps(layout.origin, layout.brand),
                "{width}x{height}: origin covers the mobile brand"
            );
            assert!(
                layout.origin.right() <= width && layout.origin.bottom() <= height,
                "{width}x{height}: origin leaves the client area"
            );
            // Two 15px lines plus the row itself: a long address may wrap, but
            // the row reserves the room for it.
            assert!(layout.origin.h >= 30.0);
        }
    }

    #[test]
    fn very_narrow_card_gets_an_extra_heading_row() {
        let layout = layout_auth(390.0, 760.0, false, false);
        assert!(layout.inner.w < 330.0);
        assert!(layout.heading_height > 64.0 + 32.0 * 1.08);
    }

    #[test]
    fn submit_validates_like_go() {
        let mut auth = AuthState::new("");
        assert!(!auth.remember_instance);
        assert_eq!(auth.submit_instance("http://example.com").as_deref(), None);
        assert_eq!(auth.status, "Введите корректный HTTPS-адрес инстанса");
        assert!(auth.is_instance());

        let origin = auth
            .submit_instance("  http://127.0.0.1:8080  ")
            .expect("loopback HTTP is allowed");
        assert_eq!(origin, "http://127.0.0.1:8080");
        assert_eq!(auth.screen, AuthScreen::Login);
        assert!(auth.status.is_empty());

        assert!(auth.submit_login(" ab ", "longenough").is_none());
        assert_eq!(
            auth.status,
            "Введите имя пользователя (не менее 3 символов)"
        );
        assert!(auth.submit_login(" abc ", "short").is_none());
        assert_eq!(auth.status, "Введите пароль (не менее 8 символов)");
    }

    #[test]
    fn instance_origin_is_normalized() {
        let mut auth = AuthState::new("");
        let origin = auth
            .submit_instance("  http://127.0.0.1:8080/  ")
            .expect("loopback HTTP is allowed");
        assert_eq!(origin, "http://127.0.0.1:8080");
        assert_eq!(auth.origin, "http://127.0.0.1:8080");

        let mut secure = AuthState::new("");
        assert_eq!(
            secure
                .submit_instance("https://chat.example.org:443/")
                .as_deref(),
            Some("https://chat.example.org")
        );
    }

    #[test]
    fn login_keeps_password_spaces_and_trims_the_user_name() {
        let mut auth = AuthState::new("");
        auth.submit_instance("https://chat.example.org");
        let request = auth
            .submit_login("  neva  ", "  pass word  ")
            .expect("valid form");
        assert_eq!(request.username, "neva");
        assert_eq!(request.password, "  pass word  ");
        assert!(auth.busy);
    }

    #[test]
    fn debug_output_never_prints_the_password() {
        let mut auth = AuthState::new("");
        auth.submit_instance("https://chat.example.org");
        let request = auth
            .submit_login("neva", "hunter2-secret")
            .expect("valid form");
        let debug = format!("{request:?}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(debug.contains("neva"), "{debug}");
        assert!(!debug.contains("hunter2-secret"), "{debug}");
    }

    #[test]
    fn double_enter_only_starts_one_attempt() {
        let mut auth = AuthState::new("");
        auth.submit_instance("https://chat.example.org");
        let first = auth.submit_login("neva", "password").unwrap();
        assert!(auth.submit_login("neva", "password").is_none());
        assert_eq!(first.attempt, 1);
    }

    #[test]
    fn late_answer_from_an_older_attempt_is_ignored() {
        let mut auth = AuthState::new("");
        auth.submit_instance("https://chat.example.org");
        let first = auth.submit_login("neva", "password").unwrap();
        assert!(auth.finish_login(first.attempt, &Err("Сервер недоступен".into())) == false);
        assert!(!auth.busy);
        assert_eq!(auth.status, "Сервер недоступен");

        let second = auth.submit_login("neva", "password2").unwrap();
        assert!(!auth.finish_login(first.attempt, &Ok(())));
        assert!(auth.busy);
        assert!(auth.finish_login(second.attempt, &Ok(())));
        assert!(!auth.busy);
    }

    #[test]
    fn transport_errors_use_the_go_text() {
        assert_eq!(
            AuthState::describe_error(&voxhold_api::Error::Http(401)),
            "Неверное имя пользователя или пароль"
        );
        assert_eq!(
            AuthState::describe_error(&voxhold_api::Error::Network),
            "Не удалось связаться с сервером"
        );
        assert_eq!(
            AuthState::describe_error(&voxhold_api::Error::Http(500)),
            "Не удалось войти. Попробуйте ещё раз."
        );
    }
}
