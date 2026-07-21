//! omar-harness Spike 1: GPUI shell + three-column skeleton + embedded
//! terminal running pi's TUI. Spike 3 adds a zed `editor` pane (see
//! SPIKE_NOTES.md and docs/architecture.md §9).
//!
//! Usage: harness-app [WORKTREE_ROOT]
//! (or set HARNESS_WORKTREE_ROOT; defaults to the current directory)

// The harness-theme `json!` literal (Spike 3c) nests past the default limit.
#![recursion_limit = "256"]

mod data;
mod editor_pane;
mod harness_zed_theme;
mod notifier;
mod terminal_pane;
mod theme;
mod workspace;

use std::path::PathBuf;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, UpdateGlobal, WindowBounds,
    WindowOptions, actions, px, size,
};

use crate::theme::HarnessTheme;
use crate::workspace::HarnessWorkspace;

actions!(harness, [Quit]);

fn main() {
    env_logger::init();

    // Launch hint: an explicit arg/env, else the cwd (unless it's "/", which is
    // what Finder-launched .app bundles get), else the user's home. If the hint
    // is a git repo it's auto-added to the sidebar; otherwise the app opens on
    // its empty state and the user picks a folder.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let worktree_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::var("HARNESS_WORKTREE_ROOT").ok().map(PathBuf::from))
        .or_else(|| match std::env::current_dir() {
            Ok(d) if d != PathBuf::from("/") => Some(d),
            _ => None,
        })
        .or_else(|| home.clone())
        .unwrap_or_else(|| PathBuf::from("."));

    gpui_platform::application().run(move |cx: &mut App| {
        // Globals required by zed's `terminal` + `editor` crates (settings
        // store, base theme, app version), in dependency order.
        release_channel::init(semver::Version::new(0, 1, 0), cx);
        settings::init(cx);
        theme_settings::init(::theme::LoadThemes::JustBase, cx);

        // Zed's default buffer/terminal font is a bundled font not present on the
        // user's machine, which falls back to a proportional face (spaced-out
        // terminal glyphs). Pin a real macOS monospace so editor + terminal read
        // correctly.
        settings::SettingsStore::update_global(cx, |store, cx| {
            let _ = store.set_user_settings(
                r#"{ "buffer_font_family": "Menlo", "buffer_font_size": 13,
                     "terminal": { "font_family": "Menlo", "font_size": 13 } }"#,
                cx,
            );
        });

        // Harness design tokens (§3.4). Install them as the active zed theme
        // (Spike 3c) BEFORE creating editor panes, so editor + terminal read
        // their colors — including the rust SyntaxTheme — from one palette.
        let hx = HarnessTheme::dark();
        harness_zed_theme::install(&hx, cx);

        // Editor crate wiring (Spike 3). Safe out-of-tree: registers defensive
        // globals + workspace item hooks; Editor::for_buffer works without it,
        // but calling it keeps parity with zed's own init order.
        editor::init(cx);

        cx.set_global(hx);

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("ctrl-q", Quit, None),
        ]);

        // Minimal editing keymap for zed editor panes (scoped to the "Editor"
        // key context). Without this, only IME text entry works — Enter,
        // Backspace, and arrows would do nothing. Full keymap is an MVP item.
        use editor::actions::{Backspace, Delete, MoveLeft, MoveRight, Newline, Tab};
        cx.bind_keys([
            KeyBinding::new("enter", Newline, Some("Editor")),
            KeyBinding::new("backspace", Backspace, Some("Editor")),
            KeyBinding::new("delete", Delete, Some("Editor")),
            KeyBinding::new("left", MoveLeft, Some("Editor")),
            KeyBinding::new("right", MoveRight, Some("Editor")),
            KeyBinding::new("tab", Tab, Some("Editor")),
        ]);

        let bounds = Bounds::centered(None, size(px(1400.0), px(880.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("omar-harness".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| HarnessWorkspace::new(worktree_root.clone(), window, cx)),
        )
        .expect("failed to open window");

        cx.activate(true);
    });
}
