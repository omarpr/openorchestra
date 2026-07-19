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
    /// Elevated surface (popovers).
    pub surface2: Rgba,
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
    pub border2: Rgba,
    // Fonts
    pub font_ui: &'static str,
    pub font_mono: &'static str,
}

impl HarnessTheme {
    /// Values transcribed from the design reference `Harness.dc.html` CSS tokens
    /// (`--bg0`, `--bg1`, `--surface`, `--accent`, `--green/amber/red`, `--tx*`).
    pub fn dark() -> Self {
        Self {
            bg0: rgb(0x0e1013),
            bg1: rgb(0x16191d),
            bg2: rgb(0x1c2025),
            surface: rgb(0x1c2025),
            surface_hover: rgb(0x22272e),
            surface2: rgb(0x22272e),
            tx0: rgb(0xe6e9ee),
            tx1: rgb(0x9aa3af),
            tx2: rgb(0x646b76),
            accent: rgb(0x5b9bff),
            green: rgb(0x4ec26b),
            red: rgb(0xf0616d),
            amber: rgb(0xe0a83b),
            border: rgb(0x24272c),
            border2: rgb(0x2f333a),
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
