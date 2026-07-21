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
        // Node is resolved per agent from $PATH (§4.5/R12); zed's managed
        // runtime would download its own, which we neither want nor need.
        node_runtime: node_runtime::NodeRuntime::unavailable(),
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

    // N1: dock the file explorer. `ProjectPanel::load` is async because it
    // reads its serialized width from the database, so the panel appears a
    // frame or two after the workspace does — which is why the Files tab
    // renders the workspace immediately rather than waiting on this.
    workspace.update(cx, |_, cx| {
        let handle = cx.entity().downgrade();
        cx.spawn_in(window, async move |_, cx| {
            match project_panel::ProjectPanel::load(handle.clone(), cx.clone()).await {
                Ok(panel) => {
                    let added = handle.update_in(cx, |workspace, window, cx| {
                        workspace.add_panel(panel, window, cx);
                        (
                            workspace.left_dock().read(cx).panels_len(),
                            workspace.bottom_dock().read(cx).panels_len(),
                            workspace.right_dock().read(cx).panels_len(),
                        )
                    });
                    log::debug!("project_panel added; docks (left,bottom,right) = {added:?}");
                }
                Err(e) => log::error!("project_panel failed to load: {e:#}"),
            }
        })
        .detach();
    });

    Ok(workspace)
}
