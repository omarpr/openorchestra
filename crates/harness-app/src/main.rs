//! openOrchestra Spike 1: GPUI shell + three-column skeleton + embedded
//! terminal running pi's TUI. Spike 3 adds a zed `editor` pane (see
//! SPIKE_NOTES.md and docs/architecture.md §9).
//!
//! Usage: harness-app [WORKTREE_ROOT]
//! (or set HARNESS_WORKTREE_ROOT; defaults to the current directory)

// The harness-theme `json!` literal (Spike 3c) nests past the default limit.
#![recursion_limit = "256"]
// Dev builds only: the unoptimized binary's unwind table (__eh_frame, ~40 MB)
// is over the 16 MB that Apple's compact unwind index can address, so ld warns
// on every link. It is harmless: functions it can't index unwind through
// __eh_frame directly. Release builds (~14 MB) don't hit it, and keep every
// linker message. `-Wl,-no_compact_unwind` is NOT a fix: without the compact
// index, panics abort ("failed to initiate panic, error 5").
#![cfg_attr(debug_assertions, allow(linker_messages))]

mod app_assets;
mod chat_pane;
mod conductor;
mod curated_languages;
mod data;
mod diff_review;
mod diff_watch;
mod git_watch;
mod harness_zed_theme;
mod login_env;
mod markdown_preview;
mod markdown_preview_button;
mod notifier;
mod panes_state;
mod ports;
mod preflight;
mod resource_monitor;
mod review_overview;
mod shortcuts;
mod sounds;
mod state_files;
mod terminal_pane;
mod theme;
mod tmux;
mod update_check;
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
        /// Open (or switch to) the full editor tab: a complete zed workspace.
        NewEditor,
        /// New agent-chat tab.
        NewChat,
        /// Copy the selected diff lines.
        CopyDiffSelection,
        /// Open the settings screen (⌘,).
        OpenSettings,
        FocusNextModalControl,
        FocusPreviousModalControl,
        /// Open the tool-requirements screen on demand.
        FirstTimeChecks,
        /// Show the version and project link (app menu).
        About,
        /// Ask GitHub for the latest release, on demand only (app menu).
        CheckForUpdates,
        /// Close the active tab in the current worktree pane (⌘W outside an
        /// editor; inside one, the Workspace-scoped binding closes the file).
        CloseTab,
        /// Open the tools panel on the Files tab (file tree).
        FocusFilesTool,
        /// Open the tools panel on Files → Search (project find-in-files).
        FocusProjectSearch,
        /// Open the tools panel on the Git tab.
        FocusGitTool,
        /// Open the tools panel on the Review tab.
        FocusReviewTool,
        /// Switch to the Nth center tab (⌘1…⌘8); ⌘9 is always the last tab.
        ActivateTab1,
        ActivateTab2,
        ActivateTab3,
        ActivateTab4,
        ActivateTab5,
        ActivateTab6,
        ActivateTab7,
        ActivateTab8,
        ActivateLastTab,
        /// Cycle center tabs.
        NextTab,
        PreviousTab,
        /// Move the selection down / up the sidebar's worktree list.
        NextWorktree,
        PreviousWorktree,
        /// Jump to the next agent that is waiting on you.
        NextWaitingAgent,
        /// Open the new-task dialog.
        NewTask,
        /// Show / hide the left sidebar.
        ToggleSidebar,
        /// Show / hide the right tools panel.
        ToggleTools,
    ]
);

/// `HARNESS_PROBE_PAINT=1`: log the painted bounds of overlays and lists, to
/// tell "laid out to zero size" from "never rendered" (see CONTRIBUTING.md).
/// Read once; render paths call this every frame.
pub fn probe_paint() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("HARNESS_PROBE_PAINT").is_some())
}

fn main() {
    env_logger::init();

    // Before anything reads PATH — the requirements probe, the agent-CLI
    // lookup, tmux, gh — and before any thread exists to race the write. A
    // Finder-launched bundle otherwise sees only launchd's four directories.
    login_env::hydrate_path();

    // Launch hint: an explicit arg/env, else the cwd (unless it's "/", which is
    // what Finder-launched .app bundles get), else the user's home. If the hint
    // is a git repo it's auto-added to the sidebar; otherwise the app opens on
    // its empty state and the user picks a folder.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let worktree_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HARNESS_WORKTREE_ROOT")
                .ok()
                .map(PathBuf::from)
        })
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
    gpui_platform::application()
        .with_assets(app_assets::AppAssets)
        .run(move |cx: &mut App| {
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
                fonts.push(std::borrow::Cow::Borrowed(
                    &include_bytes!("../fonts/FiraCode-Regular.ttf")[..],
                ));
                fonts.push(std::borrow::Cow::Borrowed(
                    &include_bytes!("../fonts/FiraCode-Bold.ttf")[..],
                ));
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
            // Harness design tokens (§3.4). Install them as the active zed theme
            // (Spike 3c) BEFORE creating editor panes, so editor + terminal read
            // their colors — including the rust SyntaxTheme — from one palette.
            // Honor the persisted palette (§12 cross-cutting): reading it here,
            // before the zed theme is generated, means a light-theme user never
            // sees a dark flash on launch.
            //
            // Fonts start as Menlo (a guaranteed macOS monospace) and can be
            // changed from the settings screen. apply_zed_settings runs *after*
            // install so the theme name is already in the registry when the
            // SettingsStore observer reloads — and so ThemeSettings names the
            // harness theme, not One Light/One Dark. A hosted Workspace observes
            // window appearance and calls reload_theme; without this sync the
            // editor silently reverts to the system default on first open.
            let hx = if workspace::persisted_dark_theme() {
                HarnessTheme::dark(cx)
            } else {
                HarnessTheme::light(cx)
            };
            let theme_name = harness_zed_theme::theme_name(&hx);
            harness_zed_theme::install(&hx, cx);
            apply_zed_settings(cx, "Zed Plex Sans", "Menlo", 13.0, theme_name, true);

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
            // The curated set and why it is shaped this way: curated_languages.rs.
            for (name, grammar) in curated_languages::curated() {
                let lang = curated_languages::build(name, grammar).unwrap_or_else(|e| {
                    panic!("{name} queries compile against the shipped grammar: {e}")
                });
                ::workspace::AppState::global(cx)
                    .languages
                    .add(std::sync::Arc::new(lang));
            }
            // The initial install() ran before AppState existed, so its set_theme
            // guard skipped — push the theme into the registry now that both
            // exist, or every HighlightMap stays empty and highlighting is
            // silently colorless. Keep observing GlobalTheme the way zed's main
            // does: Workspace's appearance observer reloads the theme without
            // going through install(), and that path must rebuild HighlightMaps
            // too or coloring dies after the first editor open.
            {
                use ::theme::{ActiveTheme as _, GlobalTheme};
                let languages = ::workspace::AppState::global(cx).languages.clone();
                languages.set_theme(cx.theme().clone());
                cx.observe_global::<GlobalTheme>({
                    let languages = languages.clone();
                    move |cx| {
                        languages.set_theme(cx.theme().clone());
                    }
                })
                .detach();
            }
            file_finder::init(cx);
            search::init(cx);
            // The git panel's constructor subscribes to the LanguageModelRegistry
            // global unconditionally (its "generate commit message" button watches
            // for a configured model), and `App::global` panics when the global is
            // absent — so without this line docking the panel aborts the process on
            // launch. `language_model::init` only installs an EMPTY registry; the
            // providers come from zed's `language_models::init`, which we
            // deliberately do not call. That is the state we want: no LLM
            // providers, so the AI commit-message affordance stays unconfigured
            // while the rest of the panel works.
            language_model::init(cx);
            // Native git/diff (project-diff view, git panel, gutter change-bars) and
            // native Markdown preview. Both register via `cx.observe_new::<Workspace>`,
            // so they auto-attach their actions to every hosted Workspace we build —
            // no per-instance wiring. git_ui also installs the blame renderer and
            // per-editor conflict views.
            git_ui::init(cx);
            // Leading `::` — this crate has its own `markdown_preview` module (the
            // diff-panel renderer), which otherwise shadows zed's crate here.
            ::markdown_preview::init(cx);

            cx.set_global(hx);

            install_menu_actions(cx);
            // Every shortcut binds from one catalog (shortcuts.rs) plus the
            // user's persisted rebindings; the settings screen renders and edits
            // the same data, so the three cannot drift.
            install_keymap(&workspace::persisted_shortcut_overrides(), cx);

            install_menus(cx);

            let bounds = Bounds::centered(None, size(px(1400.0), px(880.0)), cx);
            cx.open_window(
                WindowOptions {
                    // Maximized, deliberately not Fullscreen: a macOS fullscreen
                    // Space hides the menu bar, and the menu is now where
                    // Settings lives. The bounds are the restore size.
                    window_bounds: Some(WindowBounds::Maximized(bounds)),
                    // The top bar *is* the title bar: the native one only repeated
                    // the project name the top bar already shows, and cost a row.
                    // The traffic lights sit in the top bar's left padding,
                    // vertically centred on its 38px (see `TOP_BAR_H`).
                    titlebar: Some(TitlebarOptions {
                        title: Some("openOrchestra".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(gpui::point(gpui::px(12.0), gpui::px(12.0))),
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
/// disagree about anything but the fonts / theme they pass.
///
/// `theme` is a static ThemeSelection name (Harness Dark/Light, or a bundled
/// zed theme). It must stay in sync with `GlobalTheme`: hosted Workspaces call
/// `reload_theme` from the window-appearance observer, which re-reads this key.
pub fn apply_zed_settings(
    cx: &mut gpui::App,
    ui_font: &str,
    mono_font: &str,
    font_size: f32,
    theme: &str,
    show_ignored: bool,
) {
    // Gitignored files: `hide_gitignore` drops them from the project tree, and
    // the file finder's "smart" mode only surfaces them when ⌘P is invoked from
    // inside an ignored directory. "all" searches every ignored file too.
    let hide_gitignore = !show_ignored;
    let finder_ignored = if show_ignored { "all" } else { "smart" };
    // The panels' status-bar dock buttons stay ON here: these settings are
    // global, and the Editor tab's status bar is where you switch between
    // its file tree and Git panel. `"button": false` once hid them for every
    // workspace and left the Editor tab with an empty bottom bar. The service
    // workspace (whose docks stay closed) drops its buttons itself instead —
    // see `workspace_host::build_workspace`.
    // No global `"tab_bar": {"show": false}` for the same reason: it took
    // the Editor tab's file tabs with it. The service workspace hides its
    // own panes' tab bars (`wire_service_workspace`), since harness draws
    // those tabs itself.
    // Git primary click opens a single-file SoloDiff instead of ProjectDiff
    // (all changes), which is faster when reviewing one path at a time.
    //
    // Keep the literal free of `//` comments: it is parsed as JSON, and a
    // parse failure silently reverts EVERY setting in it (parse_status below).
    let json = format!(
        r#"{{ "theme": "{theme}",
             "buffer_font_family": "{mono_font}", "buffer_font_size": {font_size},
             "ui_font_family": "{ui_font}",
             "terminal": {{ "font_family": "{mono_font}", "font_size": {font_size} }},
             "project_panel": {{ "dock": "left", "hide_gitignore": {hide_gitignore} }},
             "file_finder": {{ "include_ignored": "{finder_ignored}" }},
             "git_panel": {{
               "dock": "left",
               "entry_primary_click_action": "file_diff"
             }},
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
pub fn install_keymap(overrides: &std::collections::BTreeMap<String, String>, cx: &mut gpui::App) {
    let bindings = shortcuts::bindings_with(overrides, cx);
    cx.bind_keys(bindings);
    cx.bind_keys([
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("escape", menu::Cancel, Some("HarnessModal")),
    ]);
    // Match at the editor node too, so form navigation wins over editor indentation.
    for context in ["HarnessModal", "HarnessModal > Editor"] {
        cx.bind_keys([
            KeyBinding::new("tab", FocusNextModalControl, Some(context)),
            KeyBinding::new("shift-tab", FocusPreviousModalControl, Some(context)),
        ]);
    }
}

fn run_with_harness_workspace(
    cx: &mut App,
    on_workspace: impl FnOnce(
        &mut HarnessWorkspace,
        &mut gpui::Window,
        &mut gpui::Context<HarnessWorkspace>,
    ) + 'static,
    on_missing: impl FnOnce(&mut App) + 'static,
) {
    let target = cx
        .active_window()
        .and_then(|w| w.downcast::<HarnessWorkspace>())
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|w| w.downcast::<HarnessWorkspace>())
        });
    if let Some(window) = target
        && window
            .update(cx, |ws, window, cx| on_workspace(ws, window, cx))
            .is_ok()
    {
        return;
    }
    on_missing(cx);
}

fn with_harness_workspace_or(
    cx: &mut App,
    on_workspace: impl FnOnce(
        &mut HarnessWorkspace,
        &mut gpui::Window,
        &mut gpui::Context<HarnessWorkspace>,
    ) + 'static,
    on_missing: impl FnOnce(&mut App) + 'static,
) {
    // `App::dispatch_action` invokes global action listeners while holding the
    // active `Window` via `Option::take`, so a synchronous `window.update`
    // inside `cx.on_action` fails with "window not found" (and `cx.windows()`
    // omits that window). Deferring runs immediately after `update_window`
    // returns the `Window` to `cx.windows`.
    if cx.active_window().is_some() {
        cx.defer(move |cx| run_with_harness_workspace(cx, on_workspace, on_missing));
    } else {
        run_with_harness_workspace(cx, on_workspace, on_missing);
    }
}

fn with_harness_workspace(
    cx: &mut App,
    f: impl FnOnce(&mut HarnessWorkspace, &mut gpui::Window, &mut gpui::Context<HarnessWorkspace>)
    + 'static,
) {
    with_harness_workspace_or(cx, f, |_| {});
}

/// Register app-level fallback handlers for every non-OS menu item.
///
/// macOS enables a menu item when `App::is_action_available` finds a listener on
/// the focused element's dispatch path *or* in `global_action_listeners`. When
/// no focusable element inside `HarnessWorkspace` has focus (e.g. empty startup
/// state or right after a modal closes), GPUI's dispatch path starts at the
/// window root above `HarnessWorkspace`, so listeners attached only to
/// `HarnessWorkspace`'s `div` would grey out every menu item except `Quit`.
/// `HarnessWorkspace` handles these first with `stop_propagation()` when focused;
/// these globals keep the menu items clickable and functional in every state.
pub fn install_menu_actions(cx: &mut gpui::App) {
    cx.on_action(|_: &Quit, cx| {
        with_harness_workspace_or(
            cx,
            |ws, _, cx| {
                ws.request_quit(cx);
            },
            |cx| cx.quit(),
        );
    });
    cx.on_action(|_: &About, cx| {
        with_harness_workspace_or(
            cx,
            |_, window, cx| update_check::show_about(window, cx),
            |_| {
                #[cfg(target_os = "macos")]
                update_check::show_native_about();
            },
        );
    });
    cx.on_action(|_: &CheckForUpdates, cx| {
        with_harness_workspace(cx, |_, window, cx| {
            update_check::check_for_updates(window, cx);
        });
    });
    cx.on_action(|_: &OpenSettings, cx| {
        with_harness_workspace(cx, |ws, window, cx| ws.toggle_settings(window, cx));
    });
    cx.on_action(|_: &FirstTimeChecks, cx| {
        with_harness_workspace(cx, |ws, _, cx| ws.open_first_time_checks(cx));
    });
    cx.on_action(|_: &NewTask, cx| {
        with_harness_workspace(cx, |ws, window, cx| ws.open_new_task(window, cx));
    });
    cx.on_action(|_: &NextWorktree, cx| {
        with_harness_workspace(cx, |ws, window, cx| ws.cycle_worktree(true, window, cx));
    });
    cx.on_action(|_: &PreviousWorktree, cx| {
        with_harness_workspace(cx, |ws, window, cx| ws.cycle_worktree(false, window, cx));
    });
    cx.on_action(|_: &NextWaitingAgent, cx| {
        with_harness_workspace(cx, |ws, window, cx| ws.next_waiting_agent(window, cx));
    });
    cx.on_action(|_: &NextTab, cx| {
        with_harness_workspace(cx, |ws, window, cx| ws.cycle_tab(true, window, cx));
    });
    cx.on_action(|_: &PreviousTab, cx| {
        with_harness_workspace(cx, |ws, window, cx| ws.cycle_tab(false, window, cx));
    });
    cx.on_action(|_: &ToggleSidebar, cx| {
        with_harness_workspace(cx, |ws, _, cx| ws.toggle_sidebar(cx));
    });
    cx.on_action(|_: &ToggleTools, cx| {
        with_harness_workspace(cx, |ws, window, cx| ws.toggle_diff(window, cx));
    });
}

/// Build the native menu bar definition.
pub fn app_menus() -> Vec<gpui::Menu> {
    vec![
        gpui::Menu {
            name: "openOrchestra".into(),
            disabled: false,
            items: vec![
                gpui::MenuItem::action("About openOrchestra", About),
                gpui::MenuItem::action("Check for updates…", CheckForUpdates),
                gpui::MenuItem::separator(),
                gpui::MenuItem::action("Settings…", OpenSettings),
                gpui::MenuItem::action("First-time Checks…", FirstTimeChecks),
                gpui::MenuItem::separator(),
                gpui::MenuItem::action("Quit openOrchestra", Quit),
            ],
        },
        // Every navigation shortcut is also a menu item, so they can be found
        // without reading Settings → Shortcuts first.
        gpui::Menu {
            name: "Go".into(),
            disabled: false,
            items: vec![
                gpui::MenuItem::action("New Task…", NewTask),
                gpui::MenuItem::separator(),
                gpui::MenuItem::action("Next Worktree", NextWorktree),
                gpui::MenuItem::action("Previous Worktree", PreviousWorktree),
                gpui::MenuItem::action("Next Agent Waiting on You", NextWaitingAgent),
                gpui::MenuItem::separator(),
                gpui::MenuItem::action("Next Tab", NextTab),
                gpui::MenuItem::action("Previous Tab", PreviousTab),
            ],
        },
        gpui::Menu {
            name: "View".into(),
            disabled: false,
            items: vec![
                gpui::MenuItem::action("Toggle Sidebar", ToggleSidebar),
                gpui::MenuItem::action("Toggle Tools Panel", ToggleTools),
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
    ]
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
    cx.set_menus(app_menus());
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, DummyKeyboardMapper,
        ForegroundExecutor, MenuItem, NoopTextSystem, Platform, PlatformDispatcher,
        PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
        PlatformWindow, Priority, RunnableVariant, Task, ThermalState, WindowAppearance,
        WindowParams,
    };
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    struct StubDispatcher;

    impl PlatformDispatcher for StubDispatcher {
        fn is_main_thread(&self) -> bool {
            true
        }
        fn dispatch(&self, runnable: RunnableVariant, _priority: Priority) {
            runnable.run();
        }
        fn dispatch_on_main_thread(&self, runnable: RunnableVariant, _priority: Priority) {
            runnable.run();
        }
        fn dispatch_after(&self, _duration: Duration, runnable: RunnableVariant) {
            drop(runnable);
        }
        fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
            f();
        }
    }

    struct StubKeyboardLayout;

    impl PlatformKeyboardLayout for StubKeyboardLayout {
        fn id(&self) -> &str {
            "stub"
        }
        fn name(&self) -> &str {
            "Stub"
        }
    }

    struct StubPlatform {
        dispatcher: Arc<dyn PlatformDispatcher>,
        text_system: Arc<dyn PlatformTextSystem>,
        quit_called: Arc<AtomicBool>,
    }

    impl StubPlatform {
        fn new() -> Self {
            Self {
                dispatcher: Arc::new(StubDispatcher),
                text_system: Arc::new(NoopTextSystem::new()),
                quit_called: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl Platform for StubPlatform {
        fn platform_info(&self) -> gpui::PlatformInfo {
            gpui::PlatformInfo {
                os: gpui::OperatingSystem::Mac,
                target: gpui::PlatformTarget::Native,
            }
        }
        fn background_executor(&self) -> BackgroundExecutor {
            BackgroundExecutor::new(self.dispatcher.clone())
        }
        fn foreground_executor(&self) -> ForegroundExecutor {
            ForegroundExecutor::new(self.dispatcher.clone())
        }
        fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
            self.text_system.clone()
        }
        fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
            on_finish_launching();
        }
        fn quit(&self) {
            self.quit_called.store(true, Ordering::SeqCst);
        }
        fn restart(&self, _binary_path: Option<PathBuf>, _arguments: Vec<std::ffi::OsString>) {}
        fn activate(&self, _ignoring_other_apps: bool) {}
        fn hide(&self) {}
        fn hide_other_apps(&self) {}
        fn unhide_other_apps(&self) {}
        fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
            Vec::new()
        }
        fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
            None
        }
        fn on_displays_changed(&self, _callback: Box<dyn FnMut()>) {}
        fn active_window(&self) -> Option<AnyWindowHandle> {
            None
        }
        fn open_window(
            &self,
            _handle: AnyWindowHandle,
            _options: WindowParams,
        ) -> anyhow::Result<Box<dyn PlatformWindow>> {
            Err(anyhow::anyhow!("stub platform has no windows"))
        }
        fn window_appearance(&self) -> WindowAppearance {
            WindowAppearance::Dark
        }
        fn open_url(&self, _url: &str) {}
        fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {}
        fn register_url_scheme(&self, _url: &str) -> Task<gpui::Result<()>> {
            Task::ready(Ok(()))
        }
        fn prompt_for_paths(
            &self,
            _options: gpui::PathPromptOptions,
        ) -> futures::channel::oneshot::Receiver<gpui::Result<Option<Vec<PathBuf>>>> {
            futures::channel::oneshot::channel().1
        }
        fn prompt_for_new_path(
            &self,
            _directory: &Path,
            _suggested_name: Option<&str>,
        ) -> futures::channel::oneshot::Receiver<gpui::Result<Option<PathBuf>>> {
            futures::channel::oneshot::channel().1
        }
        fn can_select_mixed_files_and_dirs(&self) -> bool {
            false
        }
        fn reveal_path(&self, _path: &Path) {}
        fn open_with_system(&self, _path: &Path) {}
        fn on_quit(&self, _callback: Box<dyn FnMut() -> bool>) {}
        fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}
        fn on_system_sleep(&self, _callback: Box<dyn FnMut()>) {}
        fn on_system_wake(&self, _callback: Box<dyn FnMut()>) {}
        fn set_menus(&self, _menus: Vec<gpui::Menu>, _keymap: &gpui::Keymap) {}
        fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &gpui::Keymap) {}
        fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn gpui::Action)>) {}
        fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}
        fn on_validate_app_menu_command(
            &self,
            _callback: Box<dyn FnMut(&dyn gpui::Action) -> bool>,
        ) {
        }
        fn thermal_state(&self) -> ThermalState {
            ThermalState::Nominal
        }
        fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {}
        fn prevent_idle_sleep(&self, _reason: &str) -> Task<gpui::Result<gpui::ActivityGuard>> {
            Task::ready(Ok(gpui::ActivityGuard::noop()))
        }
        fn app_path(&self) -> gpui::Result<PathBuf> {
            Err(anyhow::anyhow!("stub"))
        }
        fn path_for_auxiliary_executable(&self, _name: &str) -> gpui::Result<PathBuf> {
            Err(anyhow::anyhow!("stub"))
        }
        fn set_cursor_style(&self, _style: CursorStyle) {}
        fn hide_cursor_until_mouse_moves(&self) {}
        fn is_cursor_visible(&self) -> bool {
            true
        }
        fn should_auto_hide_scrollbars(&self) -> bool {
            false
        }
        fn read_from_clipboard(&self) -> Option<ClipboardItem> {
            None
        }
        fn write_to_clipboard(&self, _item: ClipboardItem) {}
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        fn read_from_primary(&self) -> Option<ClipboardItem> {
            None
        }
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        fn write_to_primary(&self, _item: ClipboardItem) {}
        #[cfg(target_os = "macos")]
        fn read_from_find_pasteboard(&self) -> Option<ClipboardItem> {
            None
        }
        #[cfg(target_os = "macos")]
        fn write_to_find_pasteboard(&self, _item: ClipboardItem) {}
        fn write_credentials(
            &self,
            _url: &str,
            _username: &str,
            _password: &[u8],
        ) -> Task<gpui::Result<()>> {
            Task::ready(Ok(()))
        }
        fn read_credentials(&self, _url: &str) -> Task<gpui::Result<Option<(String, Vec<u8>)>>> {
            Task::ready(Ok(None))
        }
        fn delete_credentials(&self, _url: &str) -> Task<gpui::Result<()>> {
            Task::ready(Ok(()))
        }
        fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
            Box::new(StubKeyboardLayout)
        }
        fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
            Rc::new(DummyKeyboardMapper)
        }
        fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {}
    }

    fn collect_menu_actions(
        menus: &[gpui::Menu],
        out: &mut Vec<(String, Box<dyn gpui::Action>, bool)>,
    ) {
        for menu in menus {
            for item in &menu.items {
                match item {
                    MenuItem::Action {
                        name,
                        action,
                        os_action,
                        ..
                    } => {
                        out.push((
                            format!("{} → {}", menu.name, name),
                            action.boxed_clone(),
                            os_action.is_some(),
                        ));
                    }
                    MenuItem::Submenu(sub) => {
                        collect_menu_actions(std::slice::from_ref(sub), out);
                    }
                    MenuItem::Separator | MenuItem::SystemMenu(_) => {}
                }
            }
        }
    }

    #[test]
    fn every_non_os_menu_item_is_available_without_window_focus() {
        let platform = Rc::new(StubPlatform::new());
        let handle = gpui::Application::with_platform(platform).run_embedded(|cx| {
            install_menu_actions(cx);
            install_menus(cx);
        });
        handle.update(|cx| {
            let mut items = Vec::new();
            collect_menu_actions(&app_menus(), &mut items);
            assert!(!items.is_empty());

            for (label, action, is_os_action) in items {
                let available = cx.is_action_available(action.as_ref());
                if is_os_action {
                    assert!(
                        !available,
                        "Edit OS action `{label}` should stay disabled when no editor is focused"
                    );
                } else {
                    assert!(
                        available,
                        "menu item `{label}` must remain enabled even when no element is focused"
                    );
                }
            }
        });
    }

    #[test]
    fn quit_menu_action_dispatches_globally_when_no_window_is_active() {
        let platform = Rc::new(StubPlatform::new());
        let quit_called = platform.quit_called.clone();
        let handle = gpui::Application::with_platform(platform).run_embedded(|cx| {
            install_menu_actions(cx);
            install_menus(cx);
        });
        handle.update(|cx| {
            cx.dispatch_action(&Quit);
        });
        assert!(
            quit_called.load(Ordering::SeqCst),
            "Quit must still execute via global listener when no window is active"
        );
    }
}
