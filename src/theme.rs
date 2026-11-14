//! Theme model and design tokens shared by every Voxhold surface.
//!
//! Values are taken from the Go client (`internal/workspace/theme.go`,
//! `cmd/voxhold/auth.go` and `background.go`) so the Rust client uses the same
//! palette, including the light-theme surface overrides.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

impl ThemeMode {
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub fn is_light(self, system_light: bool) -> bool {
        match self {
            Self::Light => true,
            Self::Dark => false,
            Self::System => system_light,
        }
    }

    pub fn toggled(self, system_light: bool) -> Self {
        if self.is_light(system_light) {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

/// `%APPDATA%\Voxhold\theme`, the file the Go client writes.
pub fn theme_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("Voxhold").join("theme"))
}

pub fn load_mode(path: Option<&std::path::Path>) -> ThemeMode {
    path.and_then(|path| std::fs::read_to_string(path).ok())
        .map(|value| ThemeMode::parse(&value))
        .unwrap_or(ThemeMode::System)
}

pub fn save_mode(path: Option<&std::path::Path>, mode: ThemeMode) {
    let Some(path) = path else { return };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(path, mode.as_str().as_bytes());
}

/// Current Windows app appearance (`AppsUseLightTheme`). Read-only and cheap,
/// so it can be re-read when the window is activated or the theme is toggled.
#[cfg(windows)]
pub fn system_light() -> bool {
    use std::os::windows::ffi::OsStrExt;

    const HKEY_CURRENT_USER: usize = 0x8000_0001;
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            hkey: usize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }

    fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let subkey = wide(std::ffi::OsStr::new(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
    ));
    let name = wide(std::ffi::OsStr::new("AppsUseLightTheme"));
    let mut data: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let mut kind = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            &mut kind,
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && data != 0
}

#[cfg(not(windows))]
pub fn system_light() -> bool {
    false
}

/// One resolved palette. `light` selects the Go light overrides for both the
/// workspace tokens and the fixed surfaces that map 1:1 through
/// `lightWorkspaceColors`.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub light: bool,
    pub fg: u32,
    pub muted: u32,
    pub lime: u32,
    pub bg: u32,
    pub panel: u32,
    pub surface: u32,
    pub line: u32,
    pub selection: u32,
    pub danger: u32,
    /// Auth page tokens.
    pub page_base: u32,
    pub page_end: u32,
    pub page_grid: u32,
    pub page_violet: u32,
    pub card: u32,
    pub card_border: u32,
    pub field_bg: u32,
    pub field_border: u32,
    pub field_placeholder: u32,
    pub field_focus: u32,
    pub field_selection: u32,
    pub label: u32,
    pub accent_button_ink: u32,
    pub accent_button_hover: u32,
    pub story_accent: u32,
    pub error_bg: u32,
    pub error_border: u32,
    pub error_text: u32,
    pub toggle_bg: u32,
    pub toggle_border: u32,
    pub toggle_ink: u32,
}

pub const fn dark_palette() -> Palette {
    Palette {
        light: false,
        fg: 0xf5f3ec,
        muted: 0x96968e,
        lime: 0xe7ff5b,
        bg: 0x10100f,
        panel: 0x151513,
        surface: 0x191917,
        line: 0x292927,
        selection: 0x2a2a26,
        danger: 0xffa799,
        page_base: 0x10100f,
        page_end: 0x1e1e1b,
        page_grid: 0xffffff,
        page_violet: 0xa993ff,
        card: 0x1b1b19,
        card_border: 0x363633,
        field_bg: 0x141412,
        field_border: 0x343431,
        field_placeholder: 0x66665f,
        field_focus: 0x8b9b46,
        field_selection: 0xe7ff5b,
        label: 0xc9c7bf,
        accent_button_ink: 0x15150f,
        accent_button_hover: 0xf0ff91,
        story_accent: 0xe7ff5b,
        error_bg: 0x281a19,
        error_border: 0x57302e,
        error_text: 0xffaaa6,
        toggle_bg: 0x1e1e1b,
        toggle_border: 0x343431,
        toggle_ink: 0x96968e,
    }
}

pub const fn light_palette() -> Palette {
    Palette {
        light: true,
        fg: 0x20211d,
        muted: 0x62635b,
        lime: 0xb5cf16,
        bg: 0xf2f1ec,
        panel: 0xf8f7f3,
        surface: 0xffffff,
        line: 0xdbdbd1,
        selection: 0xe7e8dc,
        danger: 0x9d2925,
        page_base: 0xf3f2ed,
        page_end: 0xf2f1ec,
        page_grid: 0x1f201b,
        page_violet: 0x7c64df,
        card: 0xf9f9f6,
        card_border: 0xd3d3ca,
        field_bg: 0xffffff,
        field_border: 0xd2d2c9,
        field_placeholder: 0x8a8b81,
        field_focus: 0x8b9b46,
        field_selection: 0xb5cf16,
        label: 0x62635b,
        accent_button_ink: 0x15150f,
        accent_button_hover: 0xf0ff91,
        story_accent: 0x4a5d13,
        error_bg: 0x281a19,
        error_border: 0x57302e,
        error_text: 0xffaaa6,
        toggle_bg: 0xf2f1ec,
        toggle_border: 0xd4d4ca,
        toggle_ink: 0x62635b,
    }
}

impl Palette {
    pub fn resolve(mode: ThemeMode, system_light: bool) -> Self {
        if mode.is_light(system_light) {
            light_palette()
        } else {
            dark_palette()
        }
    }
}

/// Go maps the workspace's fixed dark surfaces to light equivalents instead of
/// threading a theme through every widget. Keep the identical table so mixed
/// components stay consistent.
#[allow(dead_code)]
const LIGHT_SURFACE_MAP: &[(u32, u32)] = &[
    (0x171715, 0xf8f7f3),
    (0x191917, 0xf2f1ec),
    (0x1a1a17, 0xf8f7f3),
    (0x1e1e1b, 0xe8e8e0),
    (0x20201d, 0xecece3),
    (0x21211e, 0xf1f1ea),
    (0x22221f, 0xffffff),
    (0x232320, 0xeaeae0),
    (0x242421, 0xeaeae0),
    (0x252522, 0xf7f7ef),
    (0x262624, 0xe6e6de),
    (0x272722, 0xe6e6de),
    (0x26291b, 0xf0f2d9),
    (0x2e3220, 0xe4eab8),
    (0x30302c, 0xe3e3d9),
    (0x313522, 0xe9edbd),
    (0x343431, 0xd8d8ce),
    (0x363630, 0xd7d7cd),
    (0x393a2e, 0xe7ebc7),
    (0x3a3a35, 0xd7d7cd),
    (0x3b3b36, 0xd7d7cd),
];

/// Apply the light-theme surface override to a literal color token.
#[allow(dead_code)]
pub fn workspace_color(light: bool, color: u32) -> u32 {
    if !light {
        return color;
    }
    LIGHT_SURFACE_MAP
        .iter()
        .find_map(|(dark, light)| (*dark == color).then_some(*light))
        .unwrap_or(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing_matches_go() {
        assert_eq!(ThemeMode::parse("light"), ThemeMode::Light);
        assert_eq!(ThemeMode::parse(" dark "), ThemeMode::Dark);
        assert_eq!(ThemeMode::parse("system"), ThemeMode::System);
        assert_eq!(ThemeMode::parse("sepia"), ThemeMode::System);
        assert_eq!(ThemeMode::System.as_str(), "system");
    }

    #[test]
    fn system_mode_follows_windows() {
        assert!(ThemeMode::System.is_light(true));
        assert!(!ThemeMode::System.is_light(false));
        assert_eq!(ThemeMode::System.toggled(true), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.toggled(false), ThemeMode::Light);
    }

    #[test]
    fn dark_tokens_match_the_go_reference() {
        let p = dark_palette();
        assert_eq!(p.bg, 0x10100f);
        assert_eq!(p.fg, 0xf5f3ec);
        assert_eq!(p.muted, 0x96968e);
        assert_eq!(p.lime, 0xe7ff5b);
        assert_eq!(p.panel, 0x151513);
        assert_eq!(p.surface, 0x191917);
        assert_eq!(p.line, 0x292927);
        assert_eq!(p.danger, 0xffa799);
    }

    #[test]
    fn light_mode_reuses_the_go_surface_table() {
        assert_eq!(workspace_color(true, 0x191917), 0xf2f1ec);
        assert_eq!(workspace_color(true, 0x1e1e1b), 0xe8e8e0);
        assert_eq!(workspace_color(true, 0x123456), 0x123456);
        assert_eq!(workspace_color(false, 0x191917), 0x191917);
    }

    #[test]
    fn theme_file_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "voxhold-theme-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("theme");
        save_mode(Some(&path), ThemeMode::Light);
        assert_eq!(load_mode(Some(&path)), ThemeMode::Light);
        assert_eq!(load_mode(Some(&dir.join("missing"))), ThemeMode::System);
        std::fs::remove_dir_all(&dir).ok();
    }
}
