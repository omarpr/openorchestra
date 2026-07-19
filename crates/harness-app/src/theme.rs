//! Harness design tokens (dark palette), named after the CSS custom properties
//! in the Claude Design reference (`Harness.dc.html`): --bg0..2, --surface*,
//! --tx*, --accent, semantic green/red/amber. See docs/architecture.md §3.4.
//!
//! In later spikes this struct also generates the zed editor/terminal crate
//! themes so panes match the chrome (§8 R6).

use gpui::{App, Global, Rgba, rgb};

#[derive(Clone)]
pub struct HarnessTheme {
    // Background ramp
    pub bg0: Rgba,
    pub bg1: Rgba,
    pub bg2: Rgba,
    // Surfaces
    pub surface: Rgba,
    pub surface_hover: Rgba,
    // Text ramp
    pub tx0: Rgba,
    pub tx1: Rgba,
    pub tx2: Rgba,
    // Accent + semantic
    pub accent: Rgba,
    pub green: Rgba,
    pub red: Rgba,
    pub amber: Rgba,
    // Chrome
    pub border: Rgba,
    // Fonts
    pub font_ui: &'static str,
    pub font_mono: &'static str,
}

impl HarnessTheme {
    pub fn dark() -> Self {
        Self {
            bg0: rgb(0x0d1117),
            bg1: rgb(0x11161f),
            bg2: rgb(0x161d28),
            surface: rgb(0x1b2330),
            surface_hover: rgb(0x222c3c),
            tx0: rgb(0xe6edf3),
            tx1: rgb(0x9aa7b4),
            tx2: rgb(0x5c6773),
            accent: rgb(0x4f8ff7),
            green: rgb(0x3fb950),
            red: rgb(0xf85149),
            amber: rgb(0xd29922),
            border: rgb(0x2a3441),
            font_ui: ui_font_family(),
            font_mono: mono_font_family(),
        }
    }

}

impl Global for HarnessTheme {}

pub fn theme(cx: &App) -> &HarnessTheme {
    cx.global::<HarnessTheme>()
}

pub fn with_alpha(color: Rgba, a: f32) -> Rgba {
    let mut c = color;
    c.a = a;
    c
}

/// Transparent-ish tint helper for pills / badges.
pub fn tint(color: Rgba, a: f32) -> Rgba {
    with_alpha(color, a)
}

/// Design calls for IBM Plex Sans / IBM Plex Mono; fall back to fonts that are
/// guaranteed present per-OS so the spike runs without installing anything.
fn ui_font_family() -> &'static str {
    if cfg!(target_os = "macos") {
        "Helvetica Neue"
    } else {
        "DejaVu Sans"
    }
}

fn mono_font_family() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}
