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
mod model;
mod terminal_pane;
mod theme;
mod workspace;

use std::path::PathBuf;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
    actions, px, size,
};

use crate::theme::HarnessTheme;
use crate::workspace::HarnessWorkspace;

actions!(harness, [Quit]);

fn main() {
    env_logger::init();

    let worktree_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::var("HARNESS_WORKTREE_ROOT").ok().map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .expect("could not determine a worktree root");

    gpui_platform::application().run(move |cx: &mut App| {
        // Globals required by zed's `terminal` + `editor` crates (settings
        // store, base theme, app version), in dependency order.
        release_channel::init(semver::Version::new(0, 1, 0), cx);
        settings::init(cx);
        theme_settings::init(::theme::LoadThemes::JustBase, cx);

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
