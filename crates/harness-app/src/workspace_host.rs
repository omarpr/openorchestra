//! **Spike 5 — the R10 gate.** Hosting a real zed `Workspace` inside
//! openOrchestra's own chrome (architecture §3.6, D13).
//!
//! R10 asks whether `Workspace` — which in zed *is* the window — can live as a
//! child of our custom three-column layout. Everything the N-track wants hangs
//! off that: `project_panel` (file tree), `file_finder` (⌘P), and `search`
//! (⌘⇧F) are all built around a `Workspace` backed by a real `Project`.
//!
//! ## What this module establishes
//!
//! 1. A **minimal but real `AppState`** can be built out of tree. zed's own
//!    `main.rs` builds one across ~200 lines entangled with telemetry,
//!    installation IDs, and auto-update. None of that is load-bearing for
//!    `Workspace::new`; the struct needs eight fields, and each has a
//!    constructor we can call directly.
//! 2. A `Project::local` over a worktree directory, with the Spike 3 gotcha
//!    applied: `LocalProjectFlags { init_worktree_trust: false, .. }`, or the
//!    project stalls on a trust prompt no headless caller can answer.
//! 3. A `Workspace` entity constructed against that project — the actual R10
//!    question.
//!
//! ## Deliberately *not* a window
//!
//! `Workspace::new` takes a `&mut Window` but does not itself claim the
//! window; it is a `Render` entity like any other, so it can be placed inside
//! our center column. That is the finding R10 was waiting on.

use std::sync::Arc;
use std::path::Path;

use anyhow::Result;
use gpui::{App, AppContext, Entity, Window};
// `detach_and_log_err` lives on this gpui extension trait, not on Task.
use gpui::TaskExt as _;
use project::{Project, LocalProjectFlags};
use gpui::Focusable as _;
use settings::Settings as _;
use workspace::{AppState, Workspace};

/// The harness state root (`~/.openorchestra`), matching `workspace.rs`.
fn harness_state_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".openorchestra")
}

/// Build the minimal real `AppState` a `Workspace` needs.
///
/// Called once and cached as a gpui global, because every hosted workspace
/// shares it and re-creating the client/user-store per worktree would be both
/// wasteful and wrong (they carry connection state).
pub fn app_state(cx: &mut App) -> Arc<AppState> {
    if let Some(existing) = AppState::try_global(cx) {
        return existing;
    }

    let fs = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
    let mut registry = language::LanguageRegistry::new(cx.background_executor().clone());
    // Language servers need somewhere to live even when they are NOT
    // downloaded: the adapter consults this directory before falling back to a
    // user-installed binary, and without it `start_language_server` fails with
    // "no language server download dir defined". Kept under the harness state
    // root (§6.1) rather than zed's, so openOrchestra owns everything it
    // writes.
    registry.set_language_server_download_dir(harness_state_dir().join("language-servers"));
    let languages = Arc::new(registry);
    let clock = Arc::new(clock::RealSystemClock);
    // The harness is offline by design: it drives local agents and shells out
    // to `gh` for forge data. A blocked HTTP client keeps zed's collab/telemetry
    // paths inert rather than reaching out from under us.
    let http = Arc::new(http_client::HttpClientWithUrl::new(
        Arc::new(http_client::BlockedHttpClient),
        "http://localhost",
        None,
    ));
    let client = client::Client::new(clock, http, cx);
    let user_store = cx.new(|cx| client::UserStore::new(client.clone(), cx));
    let workspace_store = cx.new(|cx| workspace::WorkspaceStore::new(client.clone(), cx));
    // `Workspace` reads exactly one thing off this: `session.id()`, a string it
    // registers with the WorkspaceStore. openOrchestra keeps its own state
    // (§6.1) and deliberately does not restore zed sessions, so "a fresh id
    // with no prior history" is precisely correct — which is what
    // `Session::test()` constructs. Its name is unfortunate; the semantics are
    // the ones we want, and the alternative is opening zed's sqlite session
    // database purely to discard what it returns.
    let session = cx.new(|cx| session::AppSession::new(session::Session::test(), cx));

    let state = Arc::new(AppState {
        languages,
        client,
        user_store,
        workspace_store,
        fs,
        build_window_options: |_, _| Default::default(),
        // A REAL node runtime, pointed at the system node (already a hard
        // requirement for pi and the hook shims, R4). This is what brings the
        // node-based language servers to life — vtsls/typescript, pyright,
        // eslint, yaml — for anyone who has them installed. Downloads stay
        // off: the harness is offline by design (the Blocked http client
        // above), so adapters use PATH lookups and user installs only.
        node_runtime: {
            let (_tx, options_rx) = watch::channel(Some(node_runtime::NodeBinaryOptions {
                allow_path_lookup: true,
                allow_binary_download: false,
                use_paths: None,
            }));
            node_runtime::NodeRuntime::new(
                Arc::new(http_client::BlockedHttpClient),
                None,
                options_rx,
            )
        },
        session,
    });
    AppState::set_global(state.clone(), cx);
    state
}

/// A `Project` rooted at one worktree directory.
pub fn project_for(worktree_root: &Path, cx: &mut App) -> Entity<Project> {
    let state = app_state(cx);
    let project = Project::local(
        state.client.clone(),
        state.node_runtime.clone(),
        state.user_store.clone(),
        state.languages.clone(),
        state.fs.clone(),
        // Pass our environment explicitly rather than `None`.
        //
        // With `None`, `ProjectEnvironment::worktree_environment` falls through
        // to spawning a *login shell* per worktree to discover the environment,
        // and `LspAdapterDelegate::shell_env().await` blocks on that. In this
        // host that never resolves, so `start_language_server` logs "attempting
        // to start" and then silently stalls — no error, no process. Supplying
        // a CLI environment short-circuits it (`get_cli_environment`), and is
        // also simply more correct: the harness already decides what
        // environment its agents and tools run in.
        Some(std::env::vars().collect()),
        LocalProjectFlags {
            // Spike 3's finding: the default `true` stalls on a trust prompt
            // that nothing in our chrome can answer, which silently wedges LSP
            // and worktree scanning.
            init_worktree_trust: false,
            ..Default::default()
        },
        cx,
    );
    // A Project with no worktree has nothing for project_panel to show and
    // nothing for file_finder/search to scan, so registering the directory is
    // part of "a Project for this worktree", not a later step.
    project.update(cx, |project, cx| {
        project
            .find_or_create_worktree(worktree_root, true, cx)
            .detach_and_log_err(cx);
    });
    project
}

/// A `Workspace` hosting `worktree_root`, ready to be rendered as a child view.
pub fn workspace_for(
    worktree_root: &Path,
    window: &mut Window,
    cx: &mut App,
) -> Result<Entity<Workspace>> {
    let state = app_state(cx);
    let project = project_for(worktree_root, cx);
    let workspace = cx.new(|cx| {
        // `workspace_id: None` keeps this out of zed's session database —
        // openOrchestra owns worktree persistence in its own registry (§6.1),
        // and two systems restoring layout would fight.
        Workspace::new(None, project, state, window, cx)
    });

    // Pane toolbars — the FOURTH window-root duty this embed adopts (after
    // action wiring, key context, and the modal layer): zed's app crate
    // installs toolbar items on every pane in its initialize_pane, and the
    // Workspace itself installs none. Without the ProjectSearchBar item,
    // ⌘⇧F deploys a results view with NO query bar — nothing to type into,
    // which reads as "search is broken". BufferSearchBar is ⌘F's in-buffer
    // search. The zed-specific extras (quick actions, diagnostics, log
    // views) stay out: they live in zed's app crate and are not part of
    // "an editor that can search".
    //
    // The git/diff toolbars below are the same lesson a second time. Registering
    // an action that DEPLOYS a view is not the same as providing the surface
    // that view renders into: `git_ui::init` registers ⌘⇧D's `git::Diff`
    // handler, but with no ProjectDiffToolbar the diff opens with no stage-all
    // or commit header — the search-bar failure again, wearing a different hat.
    {
        let init_toolbar = |workspace: &Workspace,
                           pane: &Entity<workspace::Pane>,
                           window: &mut Window,
                           cx: &mut gpui::Context<Workspace>| {
            let languages = workspace.project().read(cx).languages().clone();
            // Built outside the `pane.update` because each constructor wants
            // `&Workspace`, which is borrowed for the duration of that closure.
            let project_diff_toolbar =
                cx.new(|cx| git_ui::project_diff::ProjectDiffToolbar::new(workspace, cx));
            let staged_diff_toolbar =
                cx.new(|cx| git_ui::staged_diff::StagedDiffToolbar::new(workspace, cx));
            let unstaged_diff_toolbar =
                cx.new(|cx| git_ui::unstaged_diff::UnstagedDiffToolbar::new(workspace, cx));
            let branch_diff_toolbar =
                cx.new(git_ui::branch_diff::BranchDiffToolbar::new);
            let markdown_preview_button =
                cx.new(|_| crate::markdown_preview_button::MarkdownPreviewButton::new(workspace));
            pane.update(cx, |pane, cx| {
                pane.toolbar().update(cx, |toolbar, cx| {
                    let buffer_search_bar = cx.new(|cx| {
                        search::BufferSearchBar::new(Some(languages), window, cx)
                    });
                    toolbar.add_item(buffer_search_bar, window, cx);
                    let project_search_bar = cx.new(|_| search::project_search::ProjectSearchBar::new());
                    toolbar.add_item(project_search_bar, window, cx);
                    // Each of these renders only for its own item type, so all
                    // four can coexist on every pane exactly as they do in zed.
                    toolbar.add_item(project_diff_toolbar, window, cx);
                    toolbar.add_item(staged_diff_toolbar, window, cx);
                    toolbar.add_item(unstaged_diff_toolbar, window, cx);
                    toolbar.add_item(branch_diff_toolbar, window, cx);
                    // The ⌘⇧M affordance: an eye icon on Markdown buffers.
                    toolbar.add_item(markdown_preview_button, window, cx);
                });
            });
        };
        workspace.update(cx, |ws, cx| {
            let panes: Vec<_> = ws.panes().to_vec();
            for pane in panes {
                init_toolbar(ws, &pane, window, cx);
            }
        });
        workspace.update(cx, |_, cx| {
            cx.subscribe_in(
                &cx.entity(),
                window,
                move |ws, _, event: &workspace::Event, window, cx| {
                    if let workspace::Event::PaneAdded(pane) = event {
                        let pane = pane.clone();
                        init_toolbar(ws, &pane, window, cx);
                    }
                },
            )
            .detach();
        });
    }

    // N1: dock the file explorer, and beside it the git panel. Both `load`
    // calls are async because each reads its serialized width from the
    // database, so the panels appear a frame or two after the workspace does —
    // which is why the Files tab renders the workspace immediately rather than
    // waiting on either.
    //
    // The git panel is the FIFTH window-root duty (see the toolbar block
    // above): `git_ui::init` registers its actions on every hosted Workspace,
    // but zed's app crate is what actually loads and docks the panel
    // (`zed.rs` initialize_panels). Out of tree, nobody did — so the panel
    // existed as a set of actions with nothing to act on.
    // The file tree opens with the tab — it IS the editor's navigation, and
    // that is the behavior N1 shipped.
    dock_panel(
        &workspace,
        "project_panel",
        project_panel::ProjectPanel::load,
        StartOpen::Yes,
        window,
        cx,
    );
    // The git panel is docked but CLOSED, and — per `git_panel.dock` in
    // main.rs — it docks LEFT, sharing that dock with the file tree. Forcing it
    // open would spend a third of the editor's width on every tab, in a window
    // that already gives two columns to the harness chrome, and it would take
    // the left dock's one visible slot from the file tree. ⌘⇧G brings it up:
    // `focus_panel` activates it by index on its way in, so it does not need
    // `dock_panel` to have claimed the slot for it.
    dock_panel(
        &workspace,
        "git_panel",
        git_ui::git_panel::GitPanel::load,
        StartOpen::No,
        window,
        cx,
    );

    Ok(workspace)
}

/// Whether a docked panel's dock should be open the moment the tab appears.
///
/// A bool at these call sites reads as `dock_panel(.., true, ..)`, which says
/// nothing about *what* is true.
#[derive(PartialEq, Eq, Clone, Copy)]
enum StartOpen {
    Yes,
    No,
}

/// Load a zed dock panel and make it actually visible.
///
/// Factored out because docking a panel out of tree takes three steps that zed
/// itself never performs together, and doing two of the three leaves a panel
/// that is registered and invisible — indistinguishable from "the crate is
/// broken".
///
/// `add_panel` alone is not enough to show anything: it leaves the dock CLOSED
/// *and* leaves `active_panel_index` as None. `Dock::render` bails out unless
/// the dock is open AND has an active panel, so the file tree was registered
/// and invisible and the editor tab looked empty. Zed only activates on its
/// panel-move path, so out-of-tree callers must do it themselves.
///
/// The dock is resolved from the panel's own `position()` rather than assumed,
/// because that position is a *setting* each panel reads (`project_panel.dock`,
/// `git_panel.dock` — both "left" here, see main.rs) and a user may drag a panel
/// to another edge, which rewrites it. Activating "the last panel of the left
/// dock" for a panel that had docked elsewhere would miss its target and steal
/// the file tree's slot at the same time.
fn dock_panel<T, Fut>(
    workspace: &Entity<Workspace>,
    name: &'static str,
    load: impl FnOnce(gpui::WeakEntity<Workspace>, gpui::AsyncWindowContext) -> Fut + 'static,
    start_open: StartOpen,
    window: &mut Window,
    cx: &mut App,
) where
    T: workspace::Panel,
    Fut: std::future::Future<Output = Result<Entity<T>>>,
{
    workspace.update(cx, |_, cx| {
        let handle = cx.entity().downgrade();
        cx.spawn_in(window, async move |_, cx| {
            match load(handle.clone(), cx.clone()).await {
                Ok(panel) => {
                    let added = handle.update_in(cx, |workspace, window, cx| {
                        // Read through the entity rather than the PanelHandle
                        // blanket impl, so this needs no extra trait in scope.
                        let position = panel.read(cx).position(window, cx);
                        workspace.add_panel(panel, window, cx);
                        let dock = workspace.dock_at_position(position).clone();
                        dock.update(cx, |dock, cx| {
                            // A dock with no active panel renders nothing even
                            // once opened, so SOMETHING has to claim the slot
                            // now or ⌘⇧G later opens onto blank space — the
                            // exact bug this helper exists to prevent.
                            //
                            // But claim it only when it is free, or when this
                            // panel is the one meant to be showing. Both
                            // panels dock left now, and both loads are
                            // independent spawns: activating unconditionally
                            // meant whichever finished last became the visible
                            // one, so the file tree — open, and the reason the
                            // tab exists — could be replaced by a git panel
                            // that was asked to start closed. Skipping the
                            // activation costs the git panel nothing:
                            // `focus_panel` activates by index on its way in,
                            // so ⌘⇧G still brings it up.
                            let claimed = dock.active_panel_index().is_some();
                            if !claimed || start_open == StartOpen::Yes {
                                let last = dock.panels_len().saturating_sub(1);
                                dock.activate_panel(last, window, cx);
                            }
                            if start_open == StartOpen::Yes {
                                dock.set_open(true, window, cx);
                            }
                        });
                        let dock = dock.read(cx);
                        (
                            position,
                            dock.panels_len(),
                            dock.is_open(),
                            dock.active_panel_index(),
                        )
                    });
                    log::debug!("{name} added; dock (position, panels, open, active) = {added:?}");
                }
                Err(e) => log::error!("{name} failed to load: {e:#}"),
            }
        })
        .detach();
    });
}


/// Report whether a hosted workspace is actually usable, as opposed to merely
/// constructed.
///
/// A blank editor pane is undiagnosable from the outside: every layer reports
/// success individually while the tab shows nothing. This asserts the whole
/// chain instead — the file tree is registered, its dock is open *and* has an
/// active panel (both are required, see `workspace_for`), and the worktree scan
/// found files. `HARNESS_PROBE_OPEN=<path-in-worktree>` additionally opens that
/// file and reports how many items the pane ends up holding, which is the one
/// question the state above cannot answer: whether editing works at all.
///
/// Deliberately state-based rather than screenshot-based, so it answers in CI
/// and over ssh. What it cannot cover is layout — an element correct in every
/// respect but sized to zero still logs clean here.
pub fn probe(hosted: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let hosted = hosted.clone();
    window
        .spawn(cx, async move |cx| {
            // The worktree scan and `ProjectPanel::load` are both async, so an
            // immediate read is guaranteed to report zeros and prove nothing.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1500))
                .await;

            if std::env::var_os("HARNESS_PROBE_KEYS").is_some() {
                probe_keys(&hosted, cx).await;
            }
            if std::env::var_os("HARNESS_PROBE_FINDER").is_some() {
                probe_finder(&hosted, cx).await;
            }
            if std::env::var_os("HARNESS_PROBE_SEARCH").is_some() {
                probe_search(&hosted, cx).await;
            }
            if std::env::var_os("HARNESS_PROBE_FONTS").is_some() {
                hosted.update(cx, |_, cx| {
                    let names = cx.text_system().all_font_names();
                    let fira = names.iter().any(|n| n == "Fira Code");
                    log::info!("HARNESS_PROBE_FONTS: fira_code_available = {fira}");
                });
            }
            if let Some(list) = std::env::var_os("HARNESS_PROBE_OPEN") {
                // Comma-separated, because "one file opens" and "a second file
                // opens beside it in its own tab" are different claims and the
                // second is the one a tab bar depends on.
                for rel in list.to_string_lossy().split(',') {
                    probe_open(&hosted, rel.trim(), cx).await;
                }
            }

            hosted.update(cx, |ws, cx| {
                let dock = ws.left_dock().read(cx);
                let entries: usize = ws
                    .project()
                    .read(cx)
                    .visible_worktrees(cx)
                    .map(|wt| wt.read(cx).visible_entry_count())
                    .sum();
                let tab_bar = workspace::TabBarSettings::get_global(cx).show;
                log::info!(
                    "editor tab: dock panels={} open={} active={:?} \
                     tree_entries={entries} pane_items={} tab_bar={tab_bar}",
                    dock.panels_len(),
                    dock.is_open(),
                    dock.active_panel_index(),
                    ws.active_pane().read(cx).items_len(),
                );
            });
        })
        .detach();
}

/// Press real keystrokes against a focused editor and report their effects.
///
/// This is the strictest probe in the file: it goes through
/// `Window::dispatch_keystroke`, i.e. the actual keymap — context matching,
/// precedence, the catalog bindings — not `dispatch_action`. What it proves is
/// exactly what a user's fingers get.
async fn probe_editor_keys(hosted: &Entity<Workspace>, cx: &mut gpui::AsyncWindowContext) {
    let pause = std::time::Duration::from_millis(400);
    // Focus the open editor item directly, as a click into the buffer would.
    let _ = hosted.update_in(cx, |ws, window, cx| {
        if let Some(editor) = ws.active_item(cx).and_then(|i| i.act_as::<editor::Editor>(cx)) {
            editor.update(cx, |_, cx| cx.focus_self(window));
        }
    });
    cx.background_executor().timer(pause).await;

    let text_of = |hosted: &Entity<Workspace>, cx: &mut gpui::AsyncWindowContext| {
        hosted
            .update(cx, |ws, cx| {
                let editor = ws.active_item(cx).and_then(|i| i.act_as::<editor::Editor>(cx))?;
                let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
                Some(buffer.read(cx).text())
            })
            .unwrap_or_default()
    };

    let original = text_of(hosted, cx);

    // ⌘A then ⌘C: select-all + copy through the keymap, then read the
    // clipboard back. Proves the Editor-scoped ⌘C wins over the global
    // diff-copy ⌘C — the exact shadowing this session's bindings must get
    // right — because the clipboard fills with buffer text.
    for keys in ["cmd-a", "cmd-c"] {
        let ks = gpui::Keystroke::parse(keys).expect("parses");
        let _ = cx.update(|window, cx| window.dispatch_keystroke(ks, cx));
        cx.background_executor().timer(pause).await;
    }
    let copied = cx
        .update(|_, cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    log::info!(
        "HARNESS_PROBE_EDITOR_KEYS: cmd-a/cmd-c copied {} of {} chars ({})",
        copied.len(),
        original.len(),
        if copied == original { "MATCH" } else { "MISMATCH" },
    );

    // Type over the selection, then ⌘Z: the buffer must return to its
    // original text. Undo through the keymap, not the action.
    let x = gpui::Keystroke::parse("x").expect("parses");
    let _ = cx.update(|window, cx| window.dispatch_keystroke(x, cx));
    cx.background_executor().timer(pause).await;
    let replaced = text_of(hosted, cx);
    let z = gpui::Keystroke::parse("cmd-z").expect("parses");
    let _ = cx.update(|window, cx| window.dispatch_keystroke(z, cx));
    cx.background_executor().timer(pause).await;
    let restored = text_of(hosted, cx);
    log::info!(
        "HARNESS_PROBE_EDITOR_KEYS: typed-over={} chars, cmd-z restored={}",
        replaced.len(),
        restored == original,
    );

    // ⌘S first: the typing/undo above leaves the buffer DIRTY (undo is an
    // edit; the version advances even though the content matches), and
    // closing a dirty buffer opens a native save prompt no probe can answer.
    // This is also the ⌘S verification.
    let save = gpui::Keystroke::parse("cmd-s").expect("parses");
    let _ = cx.update(|window, cx| window.dispatch_keystroke(save, cx));
    cx.background_executor().timer(pause).await;
    cx.background_executor().timer(pause).await;

    // ⌘W — with a file open and editor focus, this must close the FILE
    // (Workspace-scoped CloseActiveItem), not the harness tab: item count
    // drops by one and the workspace survives.
    let items_before = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());
    let w = gpui::Keystroke::parse("cmd-w").expect("parses");
    let _ = cx.update(|window, cx| window.dispatch_keystroke(w, cx));
    cx.background_executor().timer(pause).await;
    let items_after = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());
    log::info!(
        "HARNESS_PROBE_EDITOR_KEYS: cmd-w file close, pane items {items_before:?} -> {items_after:?}"
    );

    // ⌘, — the settings screen. With HARNESS_PROBE_PAINT set, the overlay
    // carries a canvas that logs its painted bounds, so this proves the
    // screen actually reaches pixels (it once opened *underneath* the
    // content row: state toggled perfectly, nothing showed).
    let comma = gpui::Keystroke::parse("cmd-,").expect("parses");
    let _ = cx.update(|window, cx| window.dispatch_keystroke(comma, cx));
    cx.background_executor().timer(pause).await;
}

/// Open one file in a hosted workspace and report the pane's item count.
async fn probe_open(hosted: &Entity<Workspace>, rel: &str, cx: &mut gpui::AsyncWindowContext) {
    let Ok(rel) = util::rel_path::RelPath::new(Path::new(rel), util::paths::PathStyle::local())
    else {
        log::error!("HARNESS_PROBE_OPEN: {rel:?} is not a worktree-relative path");
        return;
    };
    let rel: Arc<util::rel_path::RelPath> = rel.as_ref().into();

    let task = hosted.update_in(cx, |ws, window, cx| {
        let worktree = ws.project().read(cx).visible_worktrees(cx).next()?;
        let path = project::ProjectPath {
            worktree_id: worktree.read(cx).id(),
            path: rel,
        };
        Some(ws.open_path(path, None, true, window, cx))
    });
    match task {
        Ok(Some(task)) => match task.await {
            Ok(_) => {
                let items = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());
                log::info!("HARNESS_PROBE_OPEN: opened; pane items = {items:?}");
                // Highlighting has three prerequisites — a resolved language,
                // a registered grammar, and a parsed syntax layer. Report all
                // three so "no colors" is attributable on sight.
                let lang = hosted.update(cx, |ws, cx| {
                    let editor = ws
                        .active_item(cx)
                        .and_then(|item| item.act_as::<editor::Editor>(cx))?;
                    let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
                    let buffer = buffer.read(cx);
                    let lang = buffer.language().map(|l| l.name().to_string());
                    let grammar = buffer
                        .language()
                        .and_then(|l| l.grammar())
                        .is_some();
                    let parsed = buffer.snapshot().syntax_layers().count();
                    Some((lang, grammar, parsed))
                });
                log::info!("HARNESS_PROBE_OPEN: language/grammar/layers = {lang:?}");
                // Language assignment is async (registry load → set_language),
                // so a read racing the open can see None that resolves a beat
                // later. Read again after a settle to tell "slow" from "never".
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1500))
                    .await;
                let lang = hosted.update(cx, |ws, cx| {
                    let editor = ws
                        .active_item(cx)
                        .and_then(|item| item.act_as::<editor::Editor>(cx))?;
                    let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
                    let buffer = buffer.read(cx);
                    Some((
                        buffer.language().map(|l| l.name().to_string()),
                        buffer.language().and_then(|l| l.grammar()).is_some(),
                        buffer.snapshot().syntax_layers().count(),
                    ))
                });
                log::info!("HARNESS_PROBE_OPEN: after settle = {lang:?}");
                // Layers prove parsing; this proves *coloring*. Chunks carry a
                // highlight id only if the capture resolved through the
                // language's HighlightMap — which is exactly what stays empty
                // when nobody pushes the theme into the registry.
                let styled = hosted.update(cx, |ws, cx| {
                    let editor = ws
                        .active_item(cx)
                        .and_then(|item| item.act_as::<editor::Editor>(cx))?;
                    let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
                    let snap = buffer.read(cx).snapshot();
                    let upto = snap.len().min(4000);
                    let styled = snap
                        .chunks(
                            0..upto,
                            language::LanguageAwareStyling {
                                tree_sitter: true,
                                diagnostics: false,
                            },
                        )
                        .filter(|c| c.syntax_highlight_id.is_some())
                        .count();
                    Some(styled)
                });
                log::info!("HARNESS_PROBE_OPEN: styled chunks in first 4k = {styled:?}");
                // Inline git blame: zed's defaults enable it, but the editor
                // only *starts* it in full mode with a repo behind the buffer.
                // has_blame_entries answers whether data actually arrived.
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1500))
                    .await;
                let blame = hosted.update(cx, |ws, cx| {
                    let editor = ws
                        .active_item(cx)
                        .and_then(|item| item.act_as::<editor::Editor>(cx))?;
                    let (inline, started) = {
                        let editor = editor.read(cx);
                        (editor.git_blame_inline_enabled(), editor.blame().is_some())
                    };
                    let author = editor
                        .update(cx, |editor, cx| editor.active_git_blame_entry(cx))
                        .and_then(|e| e.author.map(|a| a.to_string()));
                    Some((inline, started, author))
                });
                log::info!(
                    "HARNESS_PROBE_OPEN: blame (inline_enabled, started, entry_author) = {blame:?}"
                );
                if std::env::var_os("HARNESS_PROBE_EDITOR_KEYS").is_some() {
                    probe_editor_keys(hosted, cx).await;
                }
            }
            Err(e) => log::error!("HARNESS_PROBE_OPEN: open failed: {e:#}"),
        },
        _ => log::error!("HARNESS_PROBE_OPEN: no visible worktree to open against"),
    }
}

/// Drive the actions behind ⌘P and ⌘⇧F and report what they produced.
///
/// The bindings exist in `main.rs`, but "bound" and "working" are different
/// claims: the action must reach the hosted `Workspace`'s handler through the
/// focus path, and the result must actually appear (a modal, a search item).
/// This dispatches exactly what the keystrokes dispatch, with focus set the
/// same way clicking the editor tab sets it.
async fn probe_keys(hosted: &Entity<Workspace>, cx: &mut gpui::AsyncWindowContext) {
    let pause = std::time::Duration::from_millis(500);

    // Re-focus the workspace pane before *each* dispatch, waiting a beat so a
    // frame renders in between. Two reasons, both learned here: (1)
    // `dispatch_action` resolves the focused node in the last *rendered*
    // frame, so focusing and dispatching in one update sends the action to
    // whatever was focused before; (2) each action moves focus (search into
    // its query editor, the finder into its picker), so without a reset the
    // probes poison each other and fail in whichever order they run.
    async fn focused_dispatch(
        hosted: &Entity<Workspace>,
        action: Box<dyn gpui::Action>,
        pause: std::time::Duration,
        cx: &mut gpui::AsyncWindowContext,
    ) {
        let _ = hosted.update_in(cx, |ws, window, cx| {
            let fh = ws.focus_handle(cx);
            window.focus(&fh, cx);
        });
        cx.background_executor().timer(pause).await;
        let _ = hosted.update_in(cx, |_, window, cx| {
            window.dispatch_action(action, cx);
        });
        cx.background_executor().timer(pause).await;
    }

    // Control: an action registered the same way whose effect is directly
    // observable. Separates "registration is broken" from "this action is".
    let dock_before = hosted.update(cx, |ws, cx| ws.left_dock().read(cx).is_open());
    focused_dispatch(hosted, Box::new(workspace::ToggleLeftDock), pause, cx).await;
    let dock_after = hosted.update(cx, |ws, cx| ws.left_dock().read(cx).is_open());
    log::info!("HARNESS_PROBE_KEYS: ToggleLeftDock dock open {dock_before:?} -> {dock_after:?}");
    // Toggle it back so the probe leaves the workspace as it found it.
    focused_dispatch(hosted, Box::new(workspace::ToggleLeftDock), pause, cx).await;

    // ⌘⇧F — project search, which lands as a pane item.
    let before = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());
    focused_dispatch(
        hosted,
        Box::new(workspace::pane::DeploySearch::default()),
        pause,
        cx,
    )
    .await;
    let after = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());
    log::info!("HARNESS_PROBE_KEYS: cmd-shift-f search pane items {before:?} -> {after:?}");

    // ⌘⇧D — git_ui's project diff, which also lands as a pane item.
    //
    // Probed at BOTH layers, because "nothing happens" has four causes that
    // look identical from the outside and need four different fixes. The
    // chord goes through the real keymap (`press` → `dispatch_keystroke`:
    // context matching, precedence, the catalog binding); the action bypasses
    // all of that and speaks straight to the Workspace handler. Comparing them
    // is what separates a broken BINDING from a broken HANDLER — a distinction
    // `dispatch_action` alone cannot make, since it would report success for a
    // chord that never matches anything.
    //
    //   chord +1                      → works; nothing wrong at this layer
    //   chord +0, action +1           → the handler is fine, the BINDING never
    //                                   matches (key-context / focus)
    //   chord +0, action +0           → git_ui's observe_new never registered
    //                                   the handler on this hosted Workspace
    //   either +1 but repo None       → it opened against a Project with no git
    //                                   repository, so the diff is legitimately
    //                                   empty — the bug is in Project setup
    //
    // `active_repository` is read AFTER the dispatches: `find_or_create_worktree`
    // is fire-and-forget (`project_for`), so the git store is not populated the
    // instant the workspace exists, and reading it first would report None for
    // timing reasons rather than real ones.
    let before = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());
    let _ = hosted.update_in(cx, |ws, window, cx| {
        let fh = ws.focus_handle(cx);
        window.focus(&fh, cx);
    });
    cx.background_executor().timer(pause).await;
    press("cmd-shift-d", 500, cx).await;
    let after_chord = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());

    focused_dispatch(hosted, Box::new(git_ui::project_diff::Diff), pause, cx).await;
    let after_action = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());

    let repo = hosted.update(cx, |ws, cx| {
        ws.project()
            .read(cx)
            .active_repository(cx)
            .map(|repo| repo.read(cx).work_directory_abs_path.clone())
    });
    log::info!(
        "HARNESS_PROBE_KEYS: cmd-shift-d project diff pane items {before:?} \
         -> {after_chord:?} (chord) -> {after_action:?} (action), \
         active_repository = {repo:?}"
    );

    // ⌘⇧G — the git panel, which shares the LEFT dock with the file tree.
    //
    // Worth its own probe because the panel is deliberately never activated at
    // dock time (`dock_panel`): it would take the visible slot away from the
    // file tree. So the chord is the only thing that makes it appear, and
    // "docked but unreachable" is indistinguishable from "docked and working"
    // in every other reading of the dock. Reports the active index against the
    // git panel's own index, since with two panels in one dock the index alone
    // says nothing about WHICH panel is showing.
    let git_panel_slot = |ws: &mut Workspace, cx: &mut gpui::Context<Workspace>| {
        let dock = ws.left_dock().read(cx);
        (
            dock.active_panel_index(),
            dock.panel_index_for_type::<git_ui::git_panel::GitPanel>(),
        )
    };
    let (active_before, git_index) = hosted.update(cx, git_panel_slot);
    focused_dispatch(
        hosted,
        Box::new(zed_actions::git_panel::ToggleFocus),
        pause,
        cx,
    )
    .await;
    let (active_after, _) = hosted.update(cx, git_panel_slot);
    log::info!(
        "HARNESS_PROBE_KEYS: cmd-shift-g git panel left-dock active {active_before:?} \
         -> {active_after:?}, git panel is index {git_index:?} \
         (showing = {})",
        active_after.is_some() && active_after == git_index
    );

    // ⌘P — file finder modal.
    focused_dispatch(
        hosted,
        Box::new(workspace::ToggleFileFinder::default()),
        pause,
        cx,
    )
    .await;
    let finder = hosted.update(cx, |ws, cx| {
        ws.active_modal::<file_finder::FileFinder>(cx).is_some()
    });
    log::info!("HARNESS_PROBE_KEYS: cmd-p file finder modal = {finder:?}");
}

/// Press one chord through the real keymap.
async fn press(keys: &str, pause_ms: u64, cx: &mut gpui::AsyncWindowContext) {
    let ks = gpui::Keystroke::parse(keys).expect("probe chord parses");
    let handled = cx.update(|window, cx| window.dispatch_keystroke(ks, cx));
    log::debug!("press {keys}: handled={handled:?}");
    cx.background_executor()
        .timer(std::time::Duration::from_millis(pause_ms))
        .await;
}

/// Type a string one keystroke at a time.
async fn type_str(text: &str, cx: &mut gpui::AsyncWindowContext) {
    for ch in text.chars() {
        press(&ch.to_string(), 120, cx).await;
    }
}

/// ⌘P end to end: open the finder, type a filename, enter — the file must be
/// OPEN afterwards. "The modal appeared" is not the claim users care about.
async fn probe_finder(hosted: &Entity<Workspace>, cx: &mut gpui::AsyncWindowContext) {
    let _ = hosted.update_in(cx, |ws, window, cx| {
        let fh = ws.focus_handle(cx);
        window.focus(&fh, cx);
    });
    cx.background_executor().timer(std::time::Duration::from_millis(1200)).await;
    let focused = hosted.update_in(cx, |ws, window, cx| {
        let fh = ws.focus_handle(cx);
        fh.is_focused(window)
    });
    let stack = cx.update(|window, _| {
        window
            .context_stack()
            .iter()
            .map(|c| format!("{c:?}"))
            .collect::<Vec<_>>()
            .join(" > ")
    });
    log::info!("HARNESS_PROBE_FINDER: context stack = {stack:?}");
    press("cmd-p", 1000, cx).await;
    let modal_after_key = hosted.update(cx, |ws, cx| {
        ws.active_modal::<file_finder::FileFinder>(cx).is_some()
    });
    log::info!(
        "HARNESS_PROBE_FINDER: focused={focused:?} modal_after_keystroke={modal_after_key}"
    );
    if !modal_after_key {
        let _ = hosted.update_in(cx, |_, window, cx| {
            window.dispatch_action(Box::new(workspace::ToggleFileFinder::default()), cx);
        });
        cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
        let modal_after_action = hosted.update(cx, |ws, cx| {
            ws.active_modal::<file_finder::FileFinder>(cx).is_some()
        });
        log::info!("HARNESS_PROBE_FINDER: modal_after_action={modal_after_action}");
    }
    let pane_still_focused = hosted.update_in(cx, |ws, window, cx| {
        let fh = ws.focus_handle(cx);
        fh.is_focused(window)
    });
    log::info!(
        "HARNESS_PROBE_FINDER: pane still focused after modal = {pane_still_focused:?} (false = modal took focus)"
    );
    type_str("workspace_host", cx).await;
    cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
    press("enter", 900, cx).await;
    let state = hosted.update(cx, |ws, cx| {
        (
            ws.active_modal::<file_finder::FileFinder>(cx).is_some(),
            ws.active_pane().read(cx).items_len(),
        )
    });
    log::info!("HARNESS_PROBE_FINDER: after enter (modal_open, items) = {state:?}");
    if state.0 {
        // Enter didn't confirm — try the action directly to split "keystroke
        // never mapped" from "confirm no-ops on an empty match list".
        let _ = hosted.update_in(cx, |_, window, cx| {
            window.dispatch_action(Box::new(menu::Confirm), cx);
        });
        cx.background_executor().timer(std::time::Duration::from_millis(900)).await;
        let state2 = hosted.update(cx, |ws, cx| {
            (
                ws.active_modal::<file_finder::FileFinder>(cx).is_some(),
                ws.active_pane().read(cx).items_len(),
            )
        });
        log::info!("HARNESS_PROBE_FINDER: after Confirm action (modal_open, items) = {state2:?}");
    }
    let opened = hosted.update(cx, |ws, cx| {
        let editor = ws.active_item(cx)?.act_as::<editor::Editor>(cx)?;
        let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
        let file = buffer.read(cx).file()?;
        Some(file.path().display(util::paths::PathStyle::local()).into_owned())
    });
    log::info!("HARNESS_PROBE_FINDER: opened file = {opened:?}");
}

/// ⌘⇧F end to end: deploy search, type a query, wait for search-as-you-type,
/// then read the RESULTS buffer — non-empty means matches actually landed.
async fn probe_search(hosted: &Entity<Workspace>, cx: &mut gpui::AsyncWindowContext) {
    let _ = hosted.update_in(cx, |ws, window, cx| {
        let fh = ws.focus_handle(cx);
        window.focus(&fh, cx);
    });
    cx.background_executor().timer(std::time::Duration::from_millis(1200)).await;
    // Force a frame before pressing: dispatch_keystroke matches against the
    // last RENDERED frame's focus, and a press racing the first frame misses
    // scoped bindings (probe-only hazard — human keypresses trail frames).
    let _ = cx.update(|window, _| window.context_stack().len());
    press("cmd-shift-f", 600, cx).await;
    let deployed = hosted.update(cx, |ws, cx| ws.active_pane().read(cx).items_len());
    if deployed == 0 {
        // First-press frame race (probe-only): fall back to the action so the
        // part users actually report — typing and getting results — is what
        // this probe stands or falls on.
        let _ = hosted.update_in(cx, |_, window, cx| {
            window.dispatch_action(Box::new(workspace::pane::DeploySearch::default()), cx);
        });
        cx.background_executor().timer(std::time::Duration::from_millis(800)).await;
    }
    type_str("HarnessTheme", cx).await;
    press("enter", 400, cx).await;
    cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
    let results = hosted.update(cx, |ws, cx| {
        let items = ws.active_pane().read(cx).items_len();
        let active = ws.active_item(cx);
        let editor = active.as_ref().and_then(|i| i.act_as::<editor::Editor>(cx));
        let len = editor
            .as_ref()
            .map(|e| e.read(cx).buffer().read(cx).snapshot(cx).len());
        (items, active.is_some(), editor.is_some(), len)
    });
    log::info!(
        "HARNESS_PROBE_SEARCH: (pane_items, active, results_editor, len) = {results:?}"
    );
}
