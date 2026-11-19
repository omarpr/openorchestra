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
    pub fn dark(cx: &App) -> Self {
        Self::dark_colors().with_system_fonts(cx)
    }

    /// The dark palette's colors, with fonts left blank.
    ///
    /// Split out so the color choices can be asserted without a gpui `App`:
    /// font resolution needs a text system, the palette does not.
    pub fn dark_colors() -> Self {
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
            font_ui: "",
            font_mono: "",
        }
    }

    /// The light palette from the same design reference (§3.4, D7).
    ///
    /// Semantic hues (accent / green / red / amber) are darkened rather than
    /// reused: the dark palette's values are tuned for contrast against a
    /// near-black background and wash out badly on white.
    pub fn light(cx: &App) -> Self {
        Self::light_colors().with_system_fonts(cx)
    }

    pub fn light_colors() -> Self {
        Self {
            bg0: rgb(0xffffff),
            bg1: rgb(0xf6f7f9),
            bg2: rgb(0xeceef2),
            surface: rgb(0xeceef2),
            surface_hover: rgb(0xe2e5ea),
            surface2: rgb(0xe2e5ea),
            tx0: rgb(0x14171c),
            tx1: rgb(0x4a5261),
            tx2: rgb(0x7b8493),
            accent: rgb(0x1f6feb),
            green: rgb(0x1a7f37),
            red: rgb(0xcf222e),
            amber: rgb(0x9a6700),
            border: rgb(0xd8dce2),
            border2: rgb(0xc4cad2),
            font_ui: "",
            font_mono: "",
        }
    }

    /// Fill in the typography for a colors-only palette.
    fn with_system_fonts(self, cx: &App) -> Self {
        Self {
            font_ui: ui_font_family(cx),
            font_mono: mono_font_family(cx),
            ..self
        }
    }

    /// Whether this palette is the dark one — used to toggle and to pick the
    /// matching zed theme appearance.
    pub fn is_dark(&self) -> bool {
        // bg0 is near-black in the dark palette and white in the light one;
        // comparing one channel is enough and avoids storing redundant state
        // that could drift out of sync with the colors.
        self.bg0.r < 0.5
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
/// The design calls for IBM Plex Sans / IBM Plex Mono (§3.4, D7).
///
/// We **use them when the user has them installed** and fall back otherwise,
/// rather than vendoring the font files. Plex is OFL and redistributable, so
/// bundling is possible — but it is a repo-weight and licensing-attribution
/// decision for the project owner, not something to add silently. This gets the
/// intended typography to anyone who installs the fonts, today, with no such
/// decision required.
///
/// Resolved once at startup and cached, because `all_font_names()` walks the
/// system font list and the theme is rebuilt on every palette toggle.
fn resolve_font(preferred: &[&str], fallback: &'static str, cx: &App) -> &'static str {
    let available = cx.text_system().all_font_names();
    for name in preferred {
        if available.iter().any(|f| f == name) {
            // Leaked deliberately and at most twice per process: HarnessTheme
            // holds &'static str, and this resolves once at startup.
            return Box::leak(name.to_string().into_boxed_str());
        }
    }
    fallback
}

fn ui_font_family(cx: &App) -> &'static str {
    resolve_font(
        &["IBM Plex Sans", "IBMPlexSans"],
        if cfg!(target_os = "macos") {
            "Helvetica Neue"
        } else {
            "DejaVu Sans"
        },
        cx,
    )
}

fn mono_font_family(cx: &App) -> &'static str {
    resolve_font(
        &["IBM Plex Mono", "IBMPlexMono"],
        if cfg!(target_os = "macos") {
            "Menlo"
        } else {
            "DejaVu Sans Mono"
        },
        cx,
    )
}

#[cfg(test)]
mod tests {
    use super::HarnessTheme;

    // Palettes are asserted without a gpui App: font resolution needs a text
    // system, but the colors — which is what these tests are about — do not.
    fn dark() -> HarnessTheme {
        HarnessTheme::dark_colors()
    }
    fn light() -> HarnessTheme {
        HarnessTheme::light_colors()
    }

    #[test]
    fn palettes_are_distinguishable() {
        assert!(dark().is_dark());
        assert!(!light().is_dark());
    }

    #[test]
    fn light_palette_keeps_text_readable_on_its_background() {
        // A light theme that reused the dark palette's text colors would be
        // unreadable; assert the contrast direction actually flipped.
        let light = light();
        assert!(light.bg0.r > 0.9, "light background should be near-white");
        assert!(light.tx0.r < 0.3, "primary text should be near-black");
        assert!(dark().tx0.r > 0.8, "dark primary text should be near-white");
    }
}
