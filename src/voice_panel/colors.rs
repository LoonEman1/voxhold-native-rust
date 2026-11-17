//! Resolved colors for the voice room, the sidebar dock and the stream player.
//!
//! `SURFACE`/`LINE` are the fixed room literals that the Go client also
//! paints, and the light theme maps them through `theme::workspace_color`, the
//! same table. `FG`/`MUTED`/`LIME`/`DANGER` are the panel's own dark tokens
//! from the shipped room, not copies of the workspace palette. Accents that
//! carry text (`accent_text`) switch to `Palette::accent_ink` on a light
//! background so the green stays readable.

use crate::theme::{self, Palette};

const SURFACE: u32 = 0x1e1e1b;
const LINE: u32 = 0x363630;
const FG: u32 = 0xf3f4eb;
const MUTED: u32 = 0x9a9a90;
const LIME: u32 = 0x72b790;
const DANGER: u32 = 0xe06b5f;
const DARK: u32 = 0x10100f;

#[derive(Clone, Copy, Debug)]
pub(crate) struct VoiceColors {
    pub light: bool,
    pub surface: u32,
    pub line: u32,
    pub fg: u32,
    pub muted: u32,
    /// Dots and badge fills (kept saturated in both themes).
    pub accent: u32,
    /// Accent used as text and as an outline on a themed surface.
    pub accent_text: u32,
    /// Text drawn on top of an accent fill.
    pub ink: u32,
    pub danger: u32,
    pub danger_bg: u32,
    pub avatar_bg: u32,
    pub live_bg: u32,
}

impl VoiceColors {
    pub(crate) fn resolve(p: Palette) -> Self {
        if p.light {
            Self {
                light: true,
                surface: theme::workspace_color(p.light, SURFACE),
                line: theme::workspace_color(p.light, LINE),
                fg: p.fg,
                muted: p.muted,
                accent: LIME,
                // The shared accent ink keeps the ring above 3:1 on the light
                // card and sidebar instead of the 1.9:1 of the raw accent.
                accent_text: p.accent_ink,
                ink: DARK,
                danger: p.danger,
                danger_bg: 0xf7e2df,
                avatar_bg: 0xe3e4d9,
                live_bg: 0xf3dedb,
            }
        } else {
            Self {
                light: false,
                surface: SURFACE,
                line: LINE,
                fg: FG,
                muted: MUTED,
                accent: LIME,
                accent_text: LIME,
                ink: DARK,
                danger: DANGER,
                danger_bg: 0x2a1c1a,
                avatar_bg: 0x2b2b26,
                live_bg: 0x39201d,
            }
        }
    }
}
