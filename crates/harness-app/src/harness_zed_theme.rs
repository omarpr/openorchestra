//! Spike 3c — generate a real zed `Theme` from the harness design tokens.
//!
//! §3.4 / §8 R6 want one palette across chrome, editor, and terminal. We build
//! a `ThemeFamilyContent` (zed's serialized-theme schema) from the `HarnessTheme`
//! tokens and install it as the active `GlobalTheme`. Because every field in the
//! content schema is `Option` + `#[serde(default)]`, a partial JSON object
//! deserializes cleanly and `refine_theme_family` fills the ~100 remaining color
//! fields with coherent dark defaults — we only supply the tokens we own.
//!
//! Once installed, zed's `editor` and `terminal` crates read their colors from
//! `cx.theme()`, so both panes match the GPUI chrome without per-pane theming.

use std::sync::Arc;

use gpui::{App, Rgba, rgb};
use serde_json::json;
use theme::{GlobalTheme, Theme, ThemeRegistry};
use theme_settings::{ThemeFamilyContent, refine_theme_family};

use crate::theme::HarnessTheme;

pub const HARNESS_THEME_NAME: &str = "Harness Dark";

/// Syntax hues not carried by the chrome token set (the design's "purple" plus
/// a teal/orange pair). Extending `HarnessTheme` with a first-class syntax ramp
/// is the natural follow-up — for now they live here, tuned to the token bg.
const SYN_FN: u32 = 0xd2a8ff; // functions — purple
const SYN_TYPE: u32 = 0x4fd6be; // types — teal
const SYN_NUM: u32 = 0xf0883e; // numbers / constants — orange

/// Build the harness theme, register it, and make it the active theme.
/// Returns the resolved `Arc<Theme>` so callers can bind it to a
/// `LanguageRegistry`/`Language` for syntax highlighting.
pub fn install(hx: &HarnessTheme, cx: &mut App) -> Arc<Theme> {
    let family = refine_theme_family(theme_family_content(hx));

    let registry = ThemeRegistry::global(cx);
    registry.insert_theme_families([family]);
    let theme = registry
        .get(HARNESS_THEME_NAME)
        .expect("harness theme registered above");

    GlobalTheme::update_theme(cx, theme.clone());
    theme
}

/// Pure token → `ThemeFamilyContent` mapping (no `App` needed), so the palette
/// generation is unit-testable headless (§8 R6).
pub fn theme_family_content(hx: &HarnessTheme) -> ThemeFamilyContent {
    let content = json!({
        "name": "Harness",
        "author": "omar-harness",
        "themes": [{
            "name": HARNESS_THEME_NAME,
            "appearance": "dark",
            "style": {
                // Chrome / surfaces
                "background": hex(hx.bg0),
                "border": hex(hx.border),
                "border.variant": hex(hx.border),
                "text": hex(hx.tx0),
                "text.muted": hex(hx.tx1),
                "text.disabled": hex(hx.tx2),
                "surface.background": hex(hx.bg1),
                "elevated_surface.background": hex(hx.surface),
                "element.background": hex(hx.surface),
                "element.hover": hex(hx.surface_hover),

                // Editor pane
                "editor.background": hex(hx.bg0),
                "editor.foreground": hex(hx.tx0),
                "editor.gutter.background": hex(hx.bg0),
                "editor.line_number": hex(hx.tx2),
                "editor.active_line_number": hex(hx.tx0),
                "editor.active_line.background": hex(hx.bg2),

                // Terminal pane (ANSI mapped onto the semantic tokens)
                "terminal.background": hex(hx.bg0),
                "terminal.foreground": hex(hx.tx0),
                "terminal.ansi.black": hex(hx.bg2),
                "terminal.ansi.red": hex(hx.red),
                "terminal.ansi.green": hex(hx.green),
                "terminal.ansi.yellow": hex(hx.amber),
                "terminal.ansi.blue": hex(hx.accent),
                "terminal.ansi.magenta": hex(rgb(SYN_FN)),
                "terminal.ansi.cyan": hex(rgb(SYN_TYPE)),
                "terminal.ansi.white": hex(hx.tx0),

                // Syntax — rust highlight captures (dotted captures inherit
                // their parent via SyntaxTheme's prefix fallback).
                "syntax": {
                    "keyword":   { "color": hex(hx.accent) },
                    "function":  { "color": hex(rgb(SYN_FN)) },
                    "type":      { "color": hex(rgb(SYN_TYPE)) },
                    "string":    { "color": hex(hx.green) },
                    "comment":   { "color": hex(hx.tx2), "font_style": "italic" },
                    "number":    { "color": hex(rgb(SYN_NUM)) },
                    "boolean":   { "color": hex(rgb(SYN_NUM)) },
                    "constant":  { "color": hex(rgb(SYN_NUM)) },
                    "variable":  { "color": hex(hx.tx0) },
                    "property":  { "color": hex(hx.tx0) },
                    "attribute": { "color": hex(hx.amber) },
                    "operator":  { "color": hex(hx.tx1) },
                    "punctuation": { "color": hex(hx.tx1) },
                    "lifetime":  { "color": hex(hx.red) },
                }
            }
        }]
    });

    serde_json::from_value(content).expect("harness theme content is a valid theme family")
}

/// gpui `Rgba` (linear 0..1 channels) → `#rrggbb` for zed's color parser.
fn hex(c: Rgba) -> String {
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to(c.r), to(c.g), to(c.b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use theme::try_parse_color;

    // The chrome tokens must survive the JSON round-trip + refine into the exact
    // same colors on the resolved editor/terminal/syntax theme (R6 invariant).
    #[test]
    fn tokens_map_into_resolved_theme() {
        let hx = HarnessTheme::dark();
        let family = refine_theme_family(theme_family_content(&hx));
        let theme = family
            .themes
            .iter()
            .find(|t| t.name.as_ref() == HARNESS_THEME_NAME)
            .expect("harness dark theme present");

        let want = |token: Rgba| try_parse_color(&hex(token)).unwrap();

        // Editor + terminal chrome pull straight from tokens.
        assert_eq!(theme.colors().editor_background, want(hx.bg0));
        assert_eq!(theme.colors().editor_foreground, want(hx.tx0));
        assert_eq!(theme.colors().terminal_ansi_red, want(hx.red));
        assert_eq!(theme.colors().terminal_ansi_blue, want(hx.accent));

        // Rust syntax captures resolve to the intended hues (keyword = accent,
        // string = green), proving the SyntaxTheme is populated from our tokens.
        let keyword = theme.syntax().style_for_name("keyword").expect("keyword style");
        assert_eq!(keyword.color, Some(want(hx.accent)));
        let string = theme.syntax().style_for_name("string").expect("string style");
        assert_eq!(string.color, Some(want(hx.green)));
    }
}
