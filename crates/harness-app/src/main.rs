//! omar-harness Spike 1: GPUI shell + three-column skeleton + embedded
//! terminal running pi's TUI. Spike 3 adds a zed `editor` pane (see
//! SPIKE_NOTES.md and docs/architecture.md §9).
//!
//! Usage: harness-app [WORKTREE_ROOT]
//! (or set HARNESS_WORKTREE_ROOT; defaults to the current directory)

// The harness-theme `json!` literal (Spike 3c) nests past the default limit.
#![recursion_limit = "256"]

mod chat_pane;
mod data;
mod editor_pane;
mod harness_zed_theme;
mod notifier;
mod terminal_pane;
mod theme;
mod workspace;
mod workspace_host;

use std::path::PathBuf;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, UpdateGlobal, WindowBounds,
    WindowOptions, actions, px, size,
};

use crate::theme::HarnessTheme;
use crate::workspace::HarnessWorkspace;

actions!(harness, [Quit]);

/// First `.rs` file under `root`, for the Spike 5 probe to open so a language
/// server has a reason to start.
fn first_rust_file(root: &std::path::Path) -> Option<PathBuf> {
    fn walk(dir: &std::path::Path, depth: usize) -> Option<PathBuf> {
        if depth > 4 {
            return None;
        }
        let mut dirs = Vec::new();
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !path.ends_with("target") && !path.ends_with(".git") {
                    dirs.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                return Some(path);
            }
        }
        dirs.into_iter().find_map(|d| walk(&d, depth + 1))
    }
    walk(root, 0)
}

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
            Ok(d) if d != *"/" => Some(d),
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
            let applied = store.set_user_settings(
                r#"{ "buffer_font_family": "Menlo", "buffer_font_size": 13,
                     "terminal": { "font_family": "Menlo", "font_size": 13 },
                     "project_panel": { "dock": "left" },
                     // N2: zed's *default settings file* is what normally
                     // supplies per-language server lists, and we load a
                     // minimal settings set rather than that file — so
                     // without this, no server is ever selected for a Rust
                     // buffer even though the adapter is registered.
                     "languages": {
                       "Rust": { "language_servers": ["rust-analyzer"] }
                     },
                     // N2 root cause: `start_language_server` blocks on
                     // `wait_until_worktree_trust`, and Spike 3's
                     // `init_worktree_trust: false` — which avoids the *startup*
                     // stall — leaves the worktree untrusted, so LSP waits
                     // forever on a prompt our chrome has no way to answer.
                     // Trusting is correct here: worktrees are created by the
                     // harness from repos the user explicitly added (§6.1).
                     "session": { "trust_all_worktrees": true } }"#,
                cx,
            );
            // NOT `let _ =`: a settings string that fails to parse silently
            // reverts *every* setting in it, which is indistinguishable from
            // the settings having no effect.
            // Not discarded: a settings string that fails to parse silently
            // reverts *every* setting in it, which looks identical to the
            // settings simply having no effect — an hour-costing failure mode.
            log::debug!("harness settings parse status: {:?}", applied.parse_status);
        });

        // Harness design tokens (§3.4). Install them as the active zed theme
        // (Spike 3c) BEFORE creating editor panes, so editor + terminal read
        // their colors — including the rust SyntaxTheme — from one palette.
        // Honor the persisted palette (§12 cross-cutting): reading it here,
        // before the zed theme is generated, means a light-theme user never
        // sees a dark flash on launch.
        let hx = if workspace::persisted_dark_theme() {
            HarnessTheme::dark(cx)
        } else {
            HarnessTheme::light(cx)
        };
        harness_zed_theme::install(&hx, cx);

        // Editor crate wiring (Spike 3). Safe out-of-tree: registers defensive
        // globals + workspace item hooks; Editor::for_buffer works without it,
        // but calling it keeps parity with zed's own init order.
        editor::init(cx);

        // Navigation crates (N1, §3.6). Each registers its actions and its
        // Workspace hooks; without these, `project_panel` has no panel to
        // deploy and the ⌘P / ⌘⇧F actions do not exist to bind to.
        // NOTE the leading `::` — this crate has its own `workspace` module,
        // which otherwise shadows zed's crate of the same name.
        let app_state = workspace_host::app_state(cx);
        ::workspace::init(app_state, cx);
        project_panel::init(cx);
        // N2: register the language adapters, which is what makes
        // rust-analyzer available to a Project's LspStore. Grammars are still
        // ours (one, not eighteen — §3.2); this brings the LSP side only.
        // `languages::init` registers the language *definitions* and their LSP
        // adapters, but `register_native_grammars` is behind its
        // `load-grammars` feature — which we deliberately leave off (§3.2).
        // Without a grammar the buffer never resolves to the "rust" language,
        // so `rust-analyzer` never matches and LSP silently does nothing.
        // Register exactly the one grammar we already carry.
        ::workspace::AppState::global(cx)
            .languages
            .register_native_grammars([("rust", tree_sitter_rust::LANGUAGE)]);
        languages::init(
            ::workspace::AppState::global(cx).languages.clone(),
            ::workspace::AppState::global(cx).fs.clone(),
            ::workspace::AppState::global(cx).node_runtime.clone(),
            cx,
        );
        file_finder::init(cx);
        search::init(cx);

        cx.set_global(hx);

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("ctrl-q", Quit, None),
        ]);

        // Minimal editing keymap for zed editor panes (scoped to the "Editor"
        // key context). Without this, only IME text entry works — Enter,
        // Backspace, and arrows would do nothing. Full keymap is an MVP item.
        // Navigation keybindings (D14). Bound in the "Workspace" context so
        // they only fire inside a hosted workspace, not over the sidebar.
        // Action paths confirmed against zed v1.11.3's own default-macos keymap
        // and crate sources (Spike 5). The keymap *names* differ from the Rust
        // paths in every case here, which is exactly the sort of thing the
        // spike existed to pin down:
        //   "file_finder::Toggle"      -> ::workspace::ToggleFileFinder
        //   "pane::DeploySearch"       -> ::workspace::pane::DeploySearch
        //   "project_panel::ToggleFocus" -> zed_actions::project_panel::ToggleFocus
        cx.bind_keys([
            KeyBinding::new(
                "cmd-p",
                ::workspace::ToggleFileFinder::default(),
                Some("Workspace"),
            ),
            KeyBinding::new(
                "cmd-shift-f",
                ::workspace::pane::DeploySearch::default(),
                Some("Workspace"),
            ),
            KeyBinding::new(
                "cmd-shift-e",
                zed_actions::project_panel::ToggleFocus,
                Some("Workspace"),
            ),
        ]);

        use editor::actions::{Backspace, Delete, MoveLeft, MoveRight, Newline, Tab};
        cx.bind_keys([
            KeyBinding::new("enter", Newline, Some("Editor")),
            KeyBinding::new("backspace", Backspace, Some("Editor")),
            KeyBinding::new("delete", Delete, Some("Editor")),
            KeyBinding::new("left", MoveLeft, Some("Editor")),
            KeyBinding::new("right", MoveRight, Some("Editor")),
            KeyBinding::new("tab", Tab, Some("Editor")),
        ]);

        // Spike 5 self-check (R10). The Files tab builds its Workspace lazily on
        // first click, so an ordinary headless run never exercises
        // `Workspace::new` — the one thing the spike exists to prove. This flag
        // forces the construction at startup, reports it, and quits, so the
        // gate is verifiable without a human clicking a tab.
        if std::env::var_os("HARNESS_SPIKE5").is_some() {
            let probe_root = worktree_root.clone();
            cx.open_window(WindowOptions::default(), move |window, cx| {
                match workspace_host::workspace_for(&probe_root, window, cx) {
                    Ok(hosted) => {
                        // The worktree registers asynchronously (Project::local
                        // kicks off a scan), so poll briefly rather than
                        // reading a count that is guaranteed to be 0.
                        let root = probe_root.clone();
                        window.spawn(cx, async move |cx| {
                            let mut worktrees = 0;
                            for _ in 0..50 {
                                cx.background_executor()
                                    .timer(std::time::Duration::from_millis(100))
                                    .await;
                                worktrees = hosted.update(cx, |ws, cx| {
                                    ws.project().read(cx).visible_worktrees(cx).count()
                                });
                                if worktrees > 0 {
                                    break;
                                }
                            }
                            if worktrees > 0 {
                                println!(
                                    "SPIKE5 OK: hosted zed Workspace built for {} ({worktrees} visible worktree(s) scanned)",
                                    root.display()
                                );
                                // N1's panel load is async (it reads its
                                // serialized width from the db), and N2's
                                // server only starts once a buffer of that
                                // language is open — so open a real file and
                                // give both a moment before reporting.
                                // Evidence, not assumptions: report whether the
                                // file opened, what language the buffer got,
                                // and whether the panel load errored.
                                match first_rust_file(&root) {
                                    Some(rs) => {
                                        println!("SPIKE5 probe: opening {}", rs.display());
                                        let task = hosted.update_in(cx, |ws, window, cx| {
                                            ws.open_abs_path(
                                                rs,
                                                ::workspace::OpenOptions::default(),
                                                window,
                                                cx,
                                            )
                                        });
                                        match task {
                                            Ok(task) => match task.await {
                                                Ok(_) => println!("SPIKE5 probe: file opened OK"),
                                                Err(e) => println!("SPIKE5 probe: open FAILED: {e:#}"),
                                            },
                                            Err(e) => println!("SPIKE5 probe: update failed: {e:#}"),
                                        }
                                    }
                                    None => println!("SPIKE5 probe: no .rs file found under root"),
                                }
                                for _ in 0..60 {
                                    cx.background_executor()
                                        .timer(std::time::Duration::from_millis(250))
                                        .await;
                                    let ready = hosted.update(cx, |ws, cx| {
                                        ws.left_dock().read(cx).panels_len() > 0
                                            && ws
                                                .project()
                                                .read(cx)
                                                .language_server_statuses(cx)
                                                .next()
                                                .is_some()
                                    });
                                    if ready {
                                        break;
                                    }
                                }
                                // N1/N2: report what actually attached, so a
                                // silently-absent panel or language server is
                                // visible rather than assumed.
                                let (panels, servers) = hosted
                                    .update(cx, |ws, cx| {
                                        (
                                            ws.left_dock().read(cx).panels_len()
                                                + ws.right_dock().read(cx).panels_len()
                                                + ws.bottom_dock().read(cx).panels_len(),
                                            ws.project()
                                                .read(cx)
                                                .language_server_statuses(cx)
                                                .map(|(_, s)| s.name.to_string())
                                                .collect::<Vec<_>>(),
                                        )
                                    });
                                println!("SPIKE5 N1: {panels} docked panel(s)");
                                println!("SPIKE5 N2: language servers = {servers:?}");
                                let langs = hosted.update(cx, |ws, cx| {
                                    ws.project()
                                        .read(cx)
                                        .opened_buffers(cx)
                                        .iter()
                                        .map(|b| {
                                            b.read(cx)
                                                .language()
                                                .map(|l| l.name().to_string())
                                                .unwrap_or_else(|| "<none>".into())
                                        })
                                        .collect::<Vec<_>>()
                                });
                                println!("SPIKE5 probe: open buffer languages = {langs:?}");
                            } else {
                                println!("SPIKE5 FAILED: workspace built but no worktree ever registered");
                            }
                            let _ = cx.update(|_, cx| cx.quit());
                        })
                        .detach();
                    }
                    Err(e) => println!("SPIKE5 FAILED: {e:#}"),
                }
                cx.new(|cx| HarnessWorkspace::new(probe_root.clone(), window, cx))
            })
            .expect("probe window");
            return;
        }

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
