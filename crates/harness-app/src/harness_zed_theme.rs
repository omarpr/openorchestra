//! Spike 3c — generate a real zed `Theme` from the harness design tokens.
//!
//! §3.4 / §8 R6 want one palette across chrome, editor, and terminal. We build
//! a `ThemeFamilyContent` (zed's serialized-theme schema) from the `HarnessTheme`
//! tokens and install it as the active `GlobalTheme`. Because every field in the
//! content schema is `Option` + `#[serde(default)]`, a partial JSON object
//! deserializes cleanly and `refine_theme_family` fills the ~100 remaining color
//! fields with coherent defaults for the theme's appearance — we only supply the
//! tokens we own.
//!
//! Once installed, zed's `editor` and `terminal` crates read their colors from
//! `cx.theme()`, so both panes match the GPUI chrome without per-pane theming.
//!
//! Callers must also write the active theme name into zed `ThemeSettings`
//! (via `apply_zed_settings`). A hosted `Workspace` observes window appearance
//! and calls `theme_settings::reload_theme`, which re-reads that setting — if it
//! still names One Light/One Dark, the editor silently leaves the harness pair.

use std::sync::Arc;

use gpui::{App, Rgba, rgb};
use serde_json::json;
use theme::{GlobalTheme, Theme, ThemeRegistry};
use theme_settings::{ThemeFamilyContent, refine_theme_family};

use crate::theme::HarnessTheme;

pub const HARNESS_DARK_NAME: &str = "Harness Dark";
pub const HARNESS_LIGHT_NAME: &str = "Harness Light";

/// Syntax hues not carried by the chrome token set (the design's "purple" plus
/// a teal/orange pair). Extending `HarnessTheme` with a first-class syntax ramp
/// is the natural follow-up — for now they live here, tuned to the token bg.
const SYN_FN: u32 = 0xd2a8ff; // functions — purple
const SYN_TYPE: u32 = 0x4fd6be; // types — teal
const SYN_NUM: u32 = 0xf0883e; // numbers / constants — orange

/// Registry name for the active harness palette half.
pub fn theme_name(hx: &HarnessTheme) -> &'static str {
    if hx.is_dark() {
        HARNESS_DARK_NAME
    } else {
        HARNESS_LIGHT_NAME
    }
}

/// Whether `name` is one of the generated harness themes (excluded from the
/// settings chip list so the built-in pair entry is the only clickable one).
pub fn is_harness_theme_name(name: &str) -> bool {
    name == HARNESS_DARK_NAME || name == HARNESS_LIGHT_NAME
}

/// Build the harness theme, register it, and make it the active theme.
/// Returns the resolved `Arc<Theme>` so callers can bind it to a
/// `LanguageRegistry`/`Language` for syntax highlighting.
pub fn install(hx: &HarnessTheme, cx: &mut App) -> Arc<Theme> {
    let name = theme_name(hx);
    let family = refine_theme_family(theme_family_content(hx));

    let registry = ThemeRegistry::global(cx);
    registry.insert_theme_families([family]);
    let theme = registry
        .get(name)
        .unwrap_or_else(|_| panic!("harness theme {name:?} registered above"));

    GlobalTheme::update_theme(cx, theme.clone());
    // zed's main.rs does this after every theme change, and it is load-bearing
    // for syntax highlighting: each Language maps tree-sitter captures to
    // styles through a HighlightMap built by set_theme. Without this call the
    // map is empty — buffers parse (syntax layers exist) and no color ever
    // reaches the screen. Guarded because the first install() runs before the
    // AppState exists; main.rs covers that window with an explicit set_theme.
    if let Some(state) = workspace::AppState::try_global(cx) {
        state.languages.set_theme(theme.clone());
    }
    theme
}

/// Pure token → `ThemeFamilyContent` mapping (no `App` needed), so the palette
/// generation is unit-testable headless (§8 R6).
pub fn theme_family_content(hx: &HarnessTheme) -> ThemeFamilyContent {
    let name = theme_name(hx);
    let appearance = if hx.is_dark() { "dark" } else { "light" };
    let content = json!({
        "name": "Harness",
        "author": "openOrchestra",
        "themes": [{
            "name": name,
            "appearance": appearance,
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

/// Derive the harness chrome tokens from an arbitrary zed theme, so picking
/// "One Dark" recolors the sidebar and diff panel too — a chrome that keeps
/// its own palette next to a themed editor looks broken, not customized.
///
/// Fonts pass through untouched: theme and typography are separate choices.
pub fn harness_from_zed(
    theme: &Theme,
    font_ui: &'static str,
    font_mono: &'static str,
) -> HarnessTheme {
    let c = theme.colors();
    let st = theme.status();
    let rgba = |h: gpui::Hsla| -> gpui::Rgba { h.into() };
    HarnessTheme {
        bg0: rgba(c.editor_background),
        bg1: rgba(c.surface_background),
        bg2: rgba(c.elevated_surface_background),
        surface: rgba(c.elevated_surface_background),
        surface_hover: rgba(c.element_hover),
        surface2: rgba(c.elevated_surface_background),
        tx0: rgba(c.text),
        tx1: rgba(c.text_muted),
        tx2: rgba(c.text_disabled),
        accent: rgba(c.text_accent),
        green: rgba(st.created),
        red: rgba(st.deleted),
        amber: rgba(st.modified),
        border: rgba(c.border_variant),
        border2: rgba(c.border),
        font_ui,
        font_mono,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use theme::try_parse_color;

    // The chrome tokens must survive the JSON round-trip + refine into the exact
    // same colors on the resolved editor/terminal/syntax theme (R6 invariant).
    #[test]
    fn tokens_map_into_resolved_theme() {
        let hx = HarnessTheme::dark_colors();
        let family = refine_theme_family(theme_family_content(&hx));
        let theme = family
            .themes
            .iter()
            .find(|t| t.name.as_ref() == HARNESS_DARK_NAME)
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

    #[test]
    fn light_palette_registers_under_its_own_name() {
        let hx = HarnessTheme::light_colors();
        let family = refine_theme_family(theme_family_content(&hx));
        assert!(
            family
                .themes
                .iter()
                .any(|t| t.name.as_ref() == HARNESS_LIGHT_NAME),
            "light tokens must register as Harness Light, not overwrite Harness Dark",
        );
    }
}
