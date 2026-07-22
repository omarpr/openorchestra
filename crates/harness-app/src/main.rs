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
mod diff_review;
mod harness_zed_theme;
mod markdown_preview;
mod notifier;
mod shortcuts;
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

actions!(
    harness,
    [
        Quit,
        /// New plain-shell tab in the active worktree (§3.4).
        NewTerminal,
        /// New editor tab (a hosted zed Workspace).
        NewEditor,
        /// New agent-chat tab.
        NewChat,
        /// Copy the selected diff lines.
        CopyDiffSelection,
        /// Open the settings screen (⌘,).
        OpenSettings,
        /// Close the active tab in the current worktree pane (⌘W outside an
        /// editor; inside one, the Workspace-scoped binding closes the file).
        CloseTab,
    ]
);

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

    // zed's embedded assets (SVG icons, fonts). The hosted workspace resolves
    // every ui::Icon — file-type icons, folder chevrons, search-bar buttons —
    // through the app's AssetSource at paint time, and gpui's default source
    // is empty: without this the file tree renders as bare text with
    // invisible, unclickable disclosure arrows.
    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        // Fonts ship inside the same asset tree but are NOT auto-loaded;
        // zed's main.rs does this same walk. Without it the default ui font
        // ("Zed Plex Sans", from the default settings we DO load) resolves to
        // a fallback face and the workspace chrome typesets slightly off.
        {
            use gpui::AssetSource as _;
            let mut fonts = Vec::new();
            for path in assets::Assets.list("fonts").unwrap_or_default() {
                if path.ends_with(".ttf")
                    && let Ok(Some(bytes)) = assets::Assets.load(&path)
                {
                    fonts.push(bytes);
                }
            }
            // Fira Code ships with the app (OFL — license vendored beside
            // the TTFs). Bundled rather than a candidate-if-installed: the
            // user asked for it by name, and a font picker offering a font
            // most machines lack is a chip that never appears.
            fonts.push(std::borrow::Cow::Borrowed(&include_bytes!("../fonts/FiraCode-Regular.ttf")[..]));
            fonts.push(std::borrow::Cow::Borrowed(&include_bytes!("../fonts/FiraCode-Bold.ttf")[..]));
            if let Err(e) = cx.text_system().add_fonts(fonts) {
                log::warn!("could not load embedded fonts: {e}");
            }
        }
        // Native notification wiring (delegate, category with View/Dismiss,
        // authorization request). Main thread, before anything can post.
        notifier::init();
        // Globals required by zed's `terminal` + `editor` crates (settings
        // store, base theme, app version), in dependency order.
        release_channel::init(semver::Version::new(0, 1, 0), cx);
        settings::init(cx);
        // All bundled zed themes, not just the base: the settings screen lists
        // them, and the asset source is already embedded for icons/fonts.
        theme_settings::init(::theme::LoadThemes::All(Box::new(assets::Assets)), cx);

        // Zed's default buffer/terminal font is a bundled font not present on the
        // user's machine, which falls back to a proportional face (spaced-out
        // terminal glyphs). Pin a real macOS monospace so editor + terminal read
        // correctly.
        // Fonts start as Menlo (a guaranteed macOS monospace) and can be
        // changed from the settings screen; apply_zed_settings is the single
        // place the zed settings JSON is built, so runtime font changes go
        // through exactly the same path as startup.
        apply_zed_settings(cx, "Zed Plex Sans", "Menlo", 13.0);

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
        // `languages::init` registers language definitions and LSP adapters,
        // but with its `load-grammars` feature off (§3.2 — one grammar, not
        // eighteen) it strips `grammar` from every config it registers. A
        // grammar registered separately in the registry never re-attaches to
        // those languages, so buffers resolved to a Rust with grammar=None:
        // detection and rust-analyzer worked, parsing never happened, and
        // syntax highlighting silently stayed off.
        languages::init(
            ::workspace::AppState::global(cx).languages.clone(),
            ::workspace::AppState::global(cx).fs.clone(),
            ::workspace::AppState::global(cx).node_runtime.clone(),
            cx,
        );
        // Then add OUR languages: full unstripped configs with grammars
        // attached and queries compiled. Added after init so they take
        // precedence for their extensions, while keeping init's adapter and
        // settings registrations (those key on the language name).
        //
        // A curated set, not one, not eighteen (§3.2 revised): the languages
        // this project and its worktrees actually meet. Each tuple is (config
        // name in the grammars crate, grammar) — javascript deliberately uses
        // the TSX grammar, exactly as zed's own config.toml declares.
        let langs: &[(&str, tree_sitter_language::LanguageFn)] = &[
            ("rust", tree_sitter_rust::LANGUAGE),
            ("javascript", tree_sitter_typescript::LANGUAGE_TSX),
            ("typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            ("tsx", tree_sitter_typescript::LANGUAGE_TSX),
            ("go", tree_sitter_go::LANGUAGE),
            ("json", tree_sitter_json::LANGUAGE),
            ("python", tree_sitter_python::LANGUAGE),
            ("yaml", tree_sitter_yaml::LANGUAGE),
            ("css", tree_sitter_css::LANGUAGE),
            ("bash", tree_sitter_bash::LANGUAGE),
        ];
        for (name, grammar) in langs {
            ::workspace::AppState::global(cx).languages.add(std::sync::Arc::new(
                language::Language::new(
                    grammars::load_config(name),
                    Some((*grammar).into()),
                )
                .with_queries(grammars::load_queries(name))
                .unwrap_or_else(|e| panic!("{name} queries compile against the shipped grammar: {e}")),
            ));
        }
        // The initial install() ran before AppState existed, so its set_theme
        // guard skipped — push the theme into the registry now that both
        // exist, or every HighlightMap stays empty and highlighting is
        // silently colorless. (Toggles are covered inside install itself.)
        {
            use ::theme::ActiveTheme as _;
            ::workspace::AppState::global(cx)
                .languages
                .set_theme(cx.theme().clone());
        }
        file_finder::init(cx);
        search::init(cx);

        cx.set_global(hx);

        cx.on_action(|_: &Quit, cx| cx.quit());
        // Every shortcut binds from one catalog (shortcuts.rs) plus the
        // user's persisted rebindings; the settings screen renders and edits
        // the same data, so the three cannot drift.
        install_keymap(&workspace::persisted_shortcut_overrides(), cx);

        install_menus(cx);

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
                                let open = hosted.update(cx, |ws, cx| {
                                    let dock = ws.left_dock().read(cx);
                                    (dock.is_open(), dock.active_panel_index(), ws.panes().len())
                                });
                                // Dock::render bails unless the dock is BOTH
                                // open and has an active panel. Assert both, so
                                // a regression shows up here rather than as a
                                // blank editor pane nobody can explain.
                                match open {
                                    (true, Some(_), _) => {
                                        println!("SPIKE5 N1: file tree is renderable {open:?}")
                                    }
                                    _ => println!(
                                        "SPIKE5 FAILED: file tree registered but NOT renderable {open:?}"
                                    ),
                                }
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
                                // How much of the tree actually materialized.
                                // A panel with no entries renders blank no
                                // matter how correct the dock state is.
                                for _ in 0..8 {
                                    let counts = hosted.update(cx, |ws, cx| {
                                        ws.project()
                                            .read(cx)
                                            .visible_worktrees(cx)
                                            .map(|wt| {
                                                let wt = wt.read(cx);
                                                (wt.visible_entry_count(), wt.entry_count())
                                            })
                                            .collect::<Vec<_>>()
                                    });
                                    println!("SPIKE5 probe: worktree (visible, total) entries = {counts:?}");
                                    cx.background_executor()
                                        .timer(std::time::Duration::from_secs(2))
                                        .await;
                                }
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
                // Maximized, deliberately not Fullscreen: a macOS fullscreen
                // Space hides the menu bar, and the menu is now where
                // Settings lives. The bounds are the restore size.
                window_bounds: Some(WindowBounds::Maximized(bounds)),
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

/// Build and apply the zed user-settings JSON. The one place it is built:
/// startup and every runtime settings change call this, so the two can never
/// disagree about anything but the fonts they pass.
pub fn apply_zed_settings(cx: &mut gpui::App, ui_font: &str, mono_font: &str, font_size: f32) {
    let json = format!(
        r#"{{ "buffer_font_family": "{mono_font}", "buffer_font_size": {font_size},
             "ui_font_family": "{ui_font}",
             "terminal": {{ "font_family": "{mono_font}", "font_size": {font_size} }},
             "project_panel": {{ "dock": "left" }},
             "languages": {{
               "Rust": {{ "language_servers": ["rust-analyzer"] }}
             }},
             "session": {{ "trust_all_worktrees": true }} }}"#,
    );
    settings::SettingsStore::update_global(cx, |store, cx| {
        let applied = store.set_user_settings(&json, cx);
        // Not discarded: a settings string that fails to parse silently
        // reverts *every* setting in it, which looks identical to the
        // settings simply having no effect — an hour-costing failure mode.
        log::debug!("harness settings parse status: {:?}", applied.parse_status);
    });
}

/// Bind the whole keymap: catalog + user rebindings + the uncataloged
/// ctrl-q quit chord. `main` calls it at startup; the settings screen calls
/// it again (after clear_key_bindings) whenever a chord changes.
pub fn install_keymap(
    overrides: &std::collections::BTreeMap<String, String>,
    cx: &mut gpui::App,
) {
    cx.bind_keys(shortcuts::entries_with(overrides).into_iter().filter_map(|e| e.binding));
    cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
}

/// (Re)build the native menu bar. Called after every keymap install because
/// the menus read their key equivalents from the keymap at set_menus time.
pub fn install_menus(cx: &mut gpui::App) {
    // The native macOS menu bar. Set AFTER bind_keys: gpui reads the
    // keymap when building the menus, which is how "Settings…" shows ⌘,
    // as its key equivalent without restating it here.
    //
    // The Edit menu uses os_action so macOS treats the items as the
    // standard editing verbs (Services integration, correct enablement).
    // macOS enables an item only when a handler exists on the current
    // focus path — Edit items grey out outside an editor, which is the
    // correct native behavior, not a bug to fix.
    cx.set_menus(vec![
        gpui::Menu {
            name: "openOrchestra".into(),
            disabled: false,
            items: vec![
                gpui::MenuItem::action("Settings…", OpenSettings),
                gpui::MenuItem::separator(),
                gpui::MenuItem::action("Quit openOrchestra", Quit),
            ],
        },
        gpui::Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                gpui::MenuItem::os_action("Undo", editor::actions::Undo, gpui::OsAction::Undo),
                gpui::MenuItem::os_action("Redo", editor::actions::Redo, gpui::OsAction::Redo),
                gpui::MenuItem::separator(),
                gpui::MenuItem::os_action("Cut", editor::actions::Cut, gpui::OsAction::Cut),
                gpui::MenuItem::os_action("Copy", editor::actions::Copy, gpui::OsAction::Copy),
                gpui::MenuItem::os_action("Paste", editor::actions::Paste, gpui::OsAction::Paste),
                gpui::MenuItem::separator(),
                gpui::MenuItem::os_action(
                    "Select All",
                    editor::actions::SelectAll,
                    gpui::OsAction::SelectAll,
                ),
            ],
        },
    ]);
}
