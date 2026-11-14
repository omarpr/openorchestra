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

    let fs: Arc<dyn fs::Fs> =
        fs::RealFs::new(None, cx.background_executor().clone());
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
        fs: fs.clone(),
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
    // AppState holds an `fs` for Projects, but zed UI also reads
    // `<dyn Fs>::global` — e.g. DiffStyleControls writes
    // `editor.diff_view_style` through `update_settings_file` when you click
    // Unified/Split. Without this, that click panics inside AppKit's mouse-up
    // handler (`panic_cannot_unwind`) and aborts the process. Same class of
    // missing-global crash as LanguageModelRegistry in main.rs.
    <dyn fs::Fs>::set_global(fs, cx);
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
///
/// This is the *service* workspace behind the tools panel and per-file tabs:
/// its docks stay closed and its tab bar hidden, because the harness chrome
/// draws the file tree, Git panel and tabs itself.
pub fn workspace_for(
    worktree_root: &Path,
    window: &mut Window,
    cx: &mut App,
) -> Result<Entity<Workspace>> {
    let project = project_for(worktree_root, cx);
    build_workspace(project, HostedMode::Service, window, cx)
}

/// A full zed workspace — file tree and Git docks, tab bar, status bar — for
/// the Editor ("IDE") tab, over an EXISTING project.
///
/// Sharing the service workspace's project is the point: one project means
/// one worktree scan, one set of open buffers, and one rust-analyzer, rather
/// than a second copy of all three for the same folder. Two workspaces over a
/// single project is a configuration zed itself supports.
pub fn ide_workspace_for(
    project: Entity<Project>,
    window: &mut Window,
    cx: &mut App,
) -> Result<Entity<Workspace>> {
    build_workspace(project, HostedMode::Ide, window, cx)
}

/// Which of the two ways a hosted workspace is presented.
#[derive(PartialEq, Eq, Clone, Copy)]
enum HostedMode {
    /// Behind the harness tools panel and file tabs: docks closed.
    Service,
    /// The full editor tab: zed's own docks, file tree open.
    Ide,
}

fn build_workspace(
    project: Entity<Project>,
    mode: HostedMode,
    window: &mut Window,
    cx: &mut App,
) -> Result<Entity<Workspace>> {
    let state = app_state(cx);
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
            let breadcrumbs = cx.new(|_| breadcrumbs::Breadcrumbs::new());
            pane.update(cx, |pane, cx| {
                pane.toolbar().update(cx, |toolbar, cx| {
                    // FIRST, and PrimaryLeft. This is what pushes the Markdown
                    // preview eye — and anything else right-hand — to the right
                    // edge: zed's Toolbar right-aligns its PrimaryRight group
                    // with `justify_between`, which needs a left sibling to
                    // justify against. See the Cargo.toml note.
                    toolbar.add_item(breadcrumbs, window, cx);
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
                    // SoloDiff opens via add_item as a permanent tab. Mark it
                    // preview so successive git-panel file clicks replace each
                    // other (italic tab), matching project-panel single-click.
                    if let workspace::Event::ItemAdded { item } = event
                        && item
                            .act_as::<git_ui::solo_diff_view::SoloDiffView>(cx)
                            .is_some()
                    {
                        let item_id = item.item_id();
                        for pane in ws.panes() {
                            if pane.read(cx).index_for_item(item.as_ref()).is_some() {
                                pane.update(cx, |pane, cx| {
                                    pane.replace_preview_item_id(item_id, window, cx);
                                });
                                break;
                            }
                        }
                    }
                },
            )
            .detach();
        });
    }

    // Panels are registered on the hosted Workspace so their entities exist,
    // but docks stay CLOSED: the harness right-hand tools panel hosts the
    // ProjectPanel / GitPanel views (Files / Git tabs) so the editor stays
    // full-bleed. `dock_panel` still activates a slot when needed so a later
    // focus path is never blank.
    // The Editor tab is a full IDE: its file tree starts open, as zed's does.
    //
    // Its status bar keeps zed's dock buttons — that bar is how you switch
    // between the file tree and the Git panel. The service workspace's docks
    // never open, so its buttons would only toggle hidden docks: remove them
    // from THIS workspace's status bar. (The panels' `button` setting can't
    // do this — it is global, and hiding it there emptied the Editor tab's
    // bar too.)
    if mode == HostedMode::Service {
        workspace.update(cx, |ws, cx| {
            ws.status_bar().update(cx, |bar, cx| {
                while let Some(ix) = bar.position_of_item::<workspace::dock::PanelButtons>() {
                    bar.remove_item_at(ix, cx);
                }
            });
        });
    }
    dock_panel(
        &workspace,
        "project_panel",
        project_panel::ProjectPanel::load,
        if mode == HostedMode::Ide { StartOpen::Yes } else { StartOpen::No },
        window,
        cx,
    );
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
/// `git_panel.dock`) and a user may drag a panel to another edge, which
/// rewrites it. Activating "the last panel of the left dock" for a panel that
/// had docked elsewhere would miss its target. Harness keeps docks closed and
/// hosts the panels in the tools column, but registration still goes through
/// this path so the entities exist.
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


/// Open `rel` (worktree-relative to `root`) in a hosted workspace, putting the
/// caret on `line` when there is one.
///
/// The same recipe `probe_open` proved, promoted to a real entry point: a
/// `RelPath` into a `ProjectPath` into `Workspace::open_path`, then a selection
/// change so the file opens *where the user was looking* rather than at line 1.
/// Reaching this from the diff panel is the whole point — a diff row you want
/// to change is useless if the editor drops you at the top of the file.
///
/// `preview` matches Zed's project-panel single-click: the tab is italic and
/// replaced by the next preview open until the user edits or pins it.
///
/// `root` is taken rather than read off the project because a workspace created
/// moments ago HAS no worktree yet: `project_for` starts `find_or_create_worktree`
/// and detaches it. Asking `visible_worktrees` for the id synchronously is what
/// made "New editor tab" open an empty editor — the tab appeared, the file
/// silently went nowhere, and the only trace was a warn line. Re-requesting the
/// same root joins that in-flight creation (zed dedupes by path) and resolves
/// immediately once the worktree is registered, so both answers to the edit
/// prompt land on the file.
pub fn open_file_at(
    hosted: &Entity<Workspace>,
    root: &Path,
    rel: &str,
    line: Option<u32>,
    preview: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Ok(parsed) = util::rel_path::RelPath::new(Path::new(rel), util::paths::PathStyle::local())
    else {
        log::warn!("open_file_at: {rel:?} is not a worktree-relative path");
        return;
    };
    let parsed: Arc<util::rel_path::RelPath> = parsed.as_ref().into();

    let worktree = hosted.update(cx, |ws, cx| {
        ws.project()
            .update(cx, |p, cx| p.find_or_create_worktree(root, true, cx))
    });
    let hosted = hosted.clone();

    window
        .spawn(cx, async move |cx| {
            let (worktree, _) = worktree.await?;
            let task = hosted.update_in(cx, |ws, window, cx| {
                let path = project::ProjectPath {
                    worktree_id: worktree.read(cx).id(),
                    path: parsed,
                };
                ws.open_path_preview(path, None, true, preview, true, window, cx)
            })?;
            let item = task.await?;
            cx.update(|window, cx| {
                let Some(editor) = item.act_as::<editor::Editor>(cx) else {
                    return;
                };
                if let Some(line) = line {
                    editor.update(cx, |editor, cx| {
                        // 1-based in the diff gutter, 0-based in the buffer.
                        let row = line.saturating_sub(1);
                        editor.change_selections(
                            editor::SelectionEffects::default(),
                            window,
                            cx,
                            |s| {
                                s.select_ranges([
                                    language::Point::new(row, 0)..language::Point::new(row, 0),
                                ]);
                            },
                        );
                    });
                }
                window.focus(&editor.focus_handle(cx), cx);
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
}
