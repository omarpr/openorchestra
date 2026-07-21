//! Root view — three-column shell styled to `docs/harness-ui/Harness.dc.html`.
//!
//! The app keeps its state in `~/.openorchestra/` (registry + the list of repo
//! folders), independent of where it was launched — so a Finder-launched
//! `.app` (cwd `/`) still works. Folders are added via a native picker; when
//! none are open the app shows an empty state. Sidebar cards, the diff panel,
//! per-worktree tabs, create/rename/avatar, right-click menu, live PR/CI +
//! agent badges, and tool-status chips are all here.
//!
//! Selection is by id and resolves to `None` when no folder is open, so nothing
//! can index an empty repo list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, PathPromptOptions, Pixels, Render, Rgba, Styled,
    Window, div, prelude::*, px, rgb,
};

use agent_runtime::{
    runtime_for, AgentKind, AgentSignal, AgentState, Bus, StatusFileSource, WaitFidelity,
};
use forge::{BranchKey, BranchStatus, CheckStatus, ForgeUpdate, GhCli, Poller, PollerConfig, PrState};
use worktrees::{Change, CreateRequest, GitWorktrees, Manager, Registry, SystemRunner};

use crate::data::{compute_diff, WorkspaceData};
use crate::editor_pane::EditorPane;
use crate::terminal_pane::TerminalPane;
use crate::theme::{theme, tint, HarnessTheme};

const SWATCHES: [u32; 8] = [
    0x5b9bff, 0x4ec26b, 0xa877f5, 0xe0a83b, 0xf0616d, 0x22b8cf, 0xf27bb0, 0x8b93a0,
];
const DIFF_ADD_TX: u32 = 0x7fdd97;
const DIFF_DEL_TX: u32 = 0xf2868f;

#[derive(Clone, Copy, PartialEq)]
enum ActiveTab {
    Terminal,
    Editor,
    /// A real zed `Workspace` — file tree, ⌘P, ⌘⇧F (N1, §3.6).
    Files,
}

#[derive(Clone, PartialEq)]
enum Selection {
    Main(String),
    Worktree(String),
}

#[derive(Clone, PartialEq)]
enum Edit {
    None,
    NewName(String),
    Rename(String, String),
    Avatar(String, String),
}

struct CardMenu {
    id: String,
    x: Pixels,
    y: Pixels,
}

struct PaneSet {
    terminal: Entity<TerminalPane>,
    editor: Entity<EditorPane>,
    /// Built lazily: a `Project::local` scans the whole worktree, so paying for
    /// it on every pane creation would make switching worktrees slower for the
    /// majority of visits that never open the file tree.
    files: Option<Entity<::workspace::Workspace>>,
    active_tab: ActiveTab,
    /// Monotonic counter, bumped on every touch, for LRU eviction (M5b).
    last_used: u64,
}

/// How many worktree pane sets stay live at once.
///
/// Each holds a pty running a real agent process, so an uncapped cache means a
/// long day's work leaves a dozen agents resident, all burning tokens on
/// nothing. Evicting drops the pty and the process with it; reselecting the
/// worktree spawns a fresh one. Sized so ordinary back-and-forth between a few
/// tasks never evicts.
const MAX_LIVE_PANES: usize = 6;

struct Resolved {
    key: String,
    path: PathBuf,
    /// Which agent this entry runs, or `None` for the read-only `main`
    /// checkout, which gets a plain shell (D8).
    agent_kind: Option<AgentKind>,
    compare_ref: String,
    branch: String,
}

struct ToolStatus {
    git: bool,
    gh: bool,
    pi: bool,
}

pub struct HarnessWorkspace {
    manager: Manager<SystemRunner>,
    data: WorkspaceData,
    repo_roots: Vec<PathBuf>,
    repos_path: PathBuf,
    selection: Selection,
    collapsed: HashMap<String, bool>,
    panes: HashMap<String, PaneSet>,
    diff: worktrees::Diff,
    diff_file: Option<String>,
    /// Bumped per request so a slow background diff for a since-abandoned
    /// selection can be identified and discarded.
    diff_gen: u64,
    /// A diff is in flight — the panel shows this rather than stale numbers.
    diff_pending: bool,
    /// Monotonic tick for pane LRU ordering.
    pane_clock: u64,
    edit: Edit,
    edit_focus: FocusHandle,
    caret_on: bool,
    blink_gen: u64,
    new_target_repo: String,
    menu: Option<CardMenu>,
    pending_delete: Option<String>,
    note: String,
    forge_status: HashMap<String, BranchStatus>,
    agent_status: HashMap<PathBuf, AgentState>,
    /// Fidelity of the adapter behind each worktree's status, so the UI can be
    /// honest about how much the pill actually knows (R11).
    agent_fidelity: HashMap<PathBuf, WaitFidelity>,
    _poller: Option<Poller>,
    /// One watcher for every StatusFile-signalled worktree (pi, Claude Code).
    status_source: Option<StatusFileSource>,
    /// Where the adapters materialize their extensions and shims.
    state_dir: PathBuf,
    bus: Bus,
    tools: ToolStatus,
    focus_handle: FocusHandle,
}

impl HarnessWorkspace {
    pub fn new(launch_hint: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let git = GitWorktrees::new(SystemRunner);

        // App-level state dir, independent of any repo.
        let data_dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| launch_hint.clone())
            .join(".openorchestra");
        let repos_path = data_dir.join("repos.json");
        let reg_path = data_dir.join("registry.json");

        let mut repo_roots = load_repos(&repos_path);
        // Auto-add the launch dir if it's inside a git repo.
        if let Ok(root) = git.repo_root(&launch_hint)
            && !repo_roots.contains(&root) {
                repo_roots.push(root);
                save_repos(&repos_path, &repo_roots);
            }

        let registry = Registry::load(&reg_path).unwrap_or_else(|e| {
            log::warn!("registry load failed ({e}); starting empty");
            Registry::new_at(&reg_path)
        });
        let manager = Manager::new(registry, git, data_dir.join("worktrees"));
        let data = WorkspaceData::build(manager.git(), &manager.registry, &repo_roots);

        let branches: Vec<BranchKey> = manager
            .registry
            .active()
            .map(|e| BranchKey {
                repo_dir: e.repo_root.clone(),
                branch: e.branch.clone(),
            })
            .collect();
        let (poller, forge_rx) = Poller::spawn(Arc::new(GhCli::new()), branches, PollerConfig::default());

        // One bus for every adapter's signal (§4.6). The sidebar pill, the
        // status bar, and the notifier each take their own subscription, so it
        // must fan out rather than work-steal.
        let bus = Bus::new();
        let status_rx = Some(bus.subscribe());
        let status_source = match StatusFileSource::new(bus.clone()) {
            Ok(mut source) => {
                for e in manager.registry.active() {
                    if let Err(err) = source.watch(&e.path, e.agent_kind.clone()) {
                        log::warn!("cannot watch {}: {err}", e.path.display());
                    }
                }
                Some(source)
            }
            Err(e) => {
                log::warn!("status watcher unavailable: {e}");
                None
            }
        };

        let selection = data
            .repos
            .first()
            .map(|r| Selection::Main(r.name.clone()))
            .unwrap_or_else(|| Selection::Main(String::new()));
        let new_target_repo = data.repos.first().map(|r| r.name.clone()).unwrap_or_default();

        let mut this = Self {
            manager,
            data,
            repo_roots,
            repos_path,
            selection,
            collapsed: HashMap::new(),
            panes: HashMap::new(),
            diff: worktrees::Diff::default(),
            diff_file: None,
            diff_gen: 0,
            diff_pending: false,
            pane_clock: 0,
            edit: Edit::None,
            edit_focus: cx.focus_handle(),
            caret_on: true,
            blink_gen: 0,
            new_target_repo,
            menu: None,
            pending_delete: None,
            note: String::new(),
            forge_status: HashMap::new(),
            agent_status: HashMap::new(),
            agent_fidelity: HashMap::new(),
            _poller: Some(poller),
            status_source,
            state_dir: data_dir.clone(),
            bus,
            tools: detect_tools(),
            focus_handle: cx.focus_handle(),
        };
        this.ensure_pane(window, cx);
        this.recompute_diff(cx);

        cx.spawn(async move |view, cx| loop {
            match forge_rx.try_recv() {
                Ok(update) => {
                    if view.update(cx, |ws, cx| ws.apply_forge(update, cx)).is_err() {
                        break;
                    }
                }
                Err(e) if e.is_empty() => {
                    cx.background_executor().timer(Duration::from_millis(400)).await
                }
                Err(_) => break,
            }
        })
        .detach();

        if let Some(status_rx) = status_rx {
            cx.spawn(async move |view, cx| loop {
                match status_rx.try_recv() {
                    Ok(ev) => {
                        if view.update(cx, |ws, cx| ws.apply_agent(ev, cx)).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.is_empty() => {
                        cx.background_executor().timer(Duration::from_millis(400)).await
                    }
                    Err(_) => break,
                }
            })
            .detach();
        }

        this
    }

    fn apply_forge(&mut self, update: ForgeUpdate, cx: &mut Context<Self>) {
        match update.result {
            Ok(status) => {
                self.forge_status.insert(update.key.branch, status);
            }
            Err(_) => {
                self.forge_status.remove(&update.key.branch);
            }
        }
        cx.notify();
    }

    fn apply_agent(&mut self, signal: AgentSignal, cx: &mut Context<Self>) {
        let previous = self.agent_status.insert(signal.worktree_root.clone(), signal.state);

        // Record the session file the moment an adapter reports one: it is the
        // harvesting hook M7 reads, and pi only learns it after startup.
        if let (Some(session), Some(id)) = (
            &signal.session_file,
            self.worktree_id_for_path(&signal.worktree_root),
        ) {
            let changed = self
                .manager
                .registry
                .get(&id)
                .is_none_or(|e| e.session_file.as_deref() != Some(session.as_path()));
            if changed {
                let _ = self
                    .manager
                    .registry
                    .set_session_file(&id, Some(session.clone()));
                let _ = self.manager.registry.save();
            }
        }

        // Notify on any *transition* into Waiting, and only when the adapter's
        // signal is exact (R11) — a best-effort turn-complete dressed up as
        // "input needed" is the one thing that would make this noise.
        let fidelity = self
            .agent_fidelity
            .get(&signal.worktree_root)
            .copied()
            .unwrap_or(WaitFidelity::TerminalOnly);
        if previous != Some(signal.state) && signal.should_notify(fidelity) {
            self.notify_needs_input(&signal);
        }
        cx.notify();
    }

    /// Post a native "needs input" notification, unless the user is already
    /// looking at that worktree — telling someone about the prompt they are
    /// staring at is pure noise (§4.6).
    fn notify_needs_input(&self, signal: &AgentSignal) {
        if self
            .resolve()
            .is_some_and(|active| active.path == signal.worktree_root)
        {
            return;
        }
        let entry = self
            .manager
            .registry
            .entries()
            .iter()
            .find(|e| e.path == signal.worktree_root);
        let (name, repo) = match entry {
            Some(e) => (e.name.clone(), e.repo.clone()),
            None => (String::new(), String::new()),
        };
        crate::notifier::notify_needs_input(
            &crate::notifier::label_for(&name, &signal.worktree_root),
            &repo,
            match signal.reason {
                agent_runtime::WaitReason::PermissionPrompt => "waiting for approval",
                agent_runtime::WaitReason::IdlePrompt => "waiting for your next instruction",
                _ => "needs input",
            },
        );
    }

    /// Resolve a worktree path back to its registry id.
    fn worktree_id_for_path(&self, path: &Path) -> Option<String> {
        self.manager
            .registry
            .entries()
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.id.clone())
    }

    // ---- selection resolution (None when no folder is open) --------------

    fn resolve(&self) -> Option<Resolved> {
        let primary = self.data.repos.first()?;
        let main_of = |r: &crate::data::RepoView| Resolved {
            key: format!("main:{}", r.name),
            path: r.repo_root.clone(),
            agent_kind: None,
            compare_ref: r.compare_ref.clone(),
            branch: r.default_branch.clone(),
        };
        match &self.selection {
            Selection::Main(repo) => {
                let r = self.data.repos.iter().find(|x| &x.name == repo).unwrap_or(primary);
                Some(main_of(r))
            }
            Selection::Worktree(id) => {
                for r in &self.data.repos {
                    if let Some(w) = r.worktrees.iter().find(|w| &w.id == id) {
                        return Some(Resolved {
                            key: id.clone(),
                            path: w.path.clone(),
                            agent_kind: Some(
                                self.manager
                                    .registry
                                    .get(id)
                                    .map(|e| e.agent_kind.clone())
                                    .unwrap_or_default(),
                            ),
                            compare_ref: r.compare_ref.clone(),
                            branch: w.branch.clone(),
                        });
                    }
                }
                Some(main_of(primary))
            }
        }
    }

    fn ensure_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        self.pane_clock = self.pane_clock.wrapping_add(1);
        if let Some(existing) = self.panes.get_mut(&r.key) {
            existing.last_used = self.pane_clock;
            return;
        }
        // Build the adapter for whatever agent this worktree records (§4.5),
        // let it drop in its config, then run the command it names. A failing
        // on_spawn costs the status signal, never the terminal.
        let agent_command = r.agent_kind.as_ref().and_then(|kind| {
            let runtime = runtime_for(kind, &r.path, &self.state_dir);
            if let Err(e) = runtime.on_spawn(&r.path) {
                log::warn!("{kind} setup failed for {}: {e}", r.path.display());
            }
            self.agent_fidelity.insert(r.path.clone(), runtime.fidelity());
            // Adapters that stream their own signals (OpenCode's SSE) publish
            // straight onto the shared bus; file-based ones are already served
            // by StatusFileSource.
            if let Some(rx) = runtime.subscribe() {
                let bus = self.bus.clone();
                std::thread::Builder::new()
                    .name("agent-signal-forward".into())
                    .spawn(move || {
                        for signal in rx {
                            bus.publish(signal);
                        }
                    })
                    .ok();
            }
            let command = runtime.command(&r.path);
            if command.is_none() {
                log::info!("{kind} not on PATH — falling back to a plain shell");
            }
            command
        });
        let spawns_agent = agent_command.is_some();

        let terminal = cx.new(|cx| TerminalPane::new(r.path.clone(), agent_command, window, cx));
        let editor = cx.new(|cx| EditorPane::new(&r.path, window, cx));
        let active_tab = if spawns_agent {
            ActiveTab::Terminal
        } else {
            ActiveTab::Editor
        };
        self.panes.insert(
            r.key.clone(),
            PaneSet {
                terminal,
                editor,
                files: None,
                active_tab,
                last_used: self.pane_clock,
            },
        );
        self.evict_stale_panes(&r.key);
    }

    /// Build the hosted zed `Workspace` for this entry, once (N1, §3.6).
    ///
    /// Deferred to the first visit to the Files tab because `Project::local`
    /// starts a full worktree scan; doing that for every pane creation would
    /// tax every worktree switch to benefit only the visits that use the tree.
    fn ensure_files_workspace(&mut self, r: &Resolved, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.get(&r.key).is_some_and(|p| p.files.is_some()) {
            return;
        }
        match crate::workspace_host::workspace_for(&r.path, window, cx) {
            Ok(hosted) => {
                if let Some(pane) = self.panes.get_mut(&r.key) {
                    pane.files = Some(hosted);
                }
            }
            Err(e) => {
                // Never fatal: the terminal and editor tabs still work.
                log::warn!("could not open project for {}: {e}", r.path.display());
                self.note = format!("could not open project: {e}");
            }
        }
    }

    /// Drop the least-recently-used pane sets beyond [`MAX_LIVE_PANES`],
    /// never the one just created (M5b).
    fn evict_stale_panes(&mut self, keep: &str) {
        while self.panes.len() > MAX_LIVE_PANES {
            let Some(victim) = self
                .panes
                .iter()
                .filter(|(key, _)| key.as_str() != keep)
                .min_by_key(|(_, pane)| pane.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            log::info!("evicting pane {victim} (over the {MAX_LIVE_PANES}-pane cap)");
            // Dropping the PaneSet drops the terminal entity, which closes the
            // pty and terminates the agent process with it.
            self.panes.remove(&victim);
        }
    }

    fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        if let Some(p) = self.panes.get(&r.key) {
            let handle = match p.active_tab {
                ActiveTab::Terminal => p.terminal.read(cx).focus_handle(cx),
                ActiveTab::Editor => p.editor.read(cx).focus_handle(cx),
                ActiveTab::Files => match &p.files {
                    Some(ws) => ws.read(cx).focus_handle(cx),
                    None => return,
                },
            };
            window.focus(&handle, cx);
        }
    }

    /// Recompute the merge-base diff **off the UI thread** (M4).
    ///
    /// `compute_diff` shells out to `git diff`, which on a large repo takes
    /// long enough to visibly stall the window if run inline on selection.
    /// Results are tagged with the worktree they were computed for and the
    /// generation that requested them, so a slow diff for a worktree the user
    /// has already navigated away from is dropped rather than painted over the
    /// current one.
    fn recompute_diff(&mut self, cx: &mut Context<Self>) {
        self.diff_file = None;
        let Some(r) = self.resolve() else {
            self.diff = worktrees::Diff::default();
            self.diff_pending = false;
            cx.notify();
            return;
        };

        self.diff_gen = self.diff_gen.wrapping_add(1);
        let generation = self.diff_gen;
        let (path, compare_ref) = (r.path.clone(), r.compare_ref.clone());
        self.diff_pending = true;

        cx.spawn(async move |view, cx| {
            let computed = cx
                .background_spawn(async move {
                    compute_diff(&GitWorktrees::new(SystemRunner), &path, &compare_ref)
                })
                .await;
            let _ = view.update(cx, |ws, cx| {
                // Stale result: the user moved on while git was working.
                if ws.diff_gen != generation {
                    return;
                }
                ws.diff = computed;
                ws.diff_pending = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn select(&mut self, sel: Selection, window: &mut Window, cx: &mut Context<Self>) {
        self.selection = sel;
        self.pending_delete = None;
        self.menu = None;
        self.edit = Edit::None;
        self.ensure_pane(window, cx);
        self.recompute_diff(cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.data = WorkspaceData::build(self.manager.git(), &self.manager.registry, &self.repo_roots);

        let items: Vec<(PathBuf, String, PathBuf)> = self
            .data
            .repos
            .iter()
            .flat_map(|repo| {
                repo.worktrees
                    .iter()
                    .map(move |w| (repo.repo_root.clone(), w.branch.clone(), w.path.clone()))
            })
            .collect();
        if let Some(p) = &self._poller {
            for (root, branch, _) in &items {
                p.add_branch(BranchKey {
                    repo_dir: root.clone(),
                    branch: branch.clone(),
                });
            }
        }
        if let Some(source) = &mut self.status_source {
            for (_, _, path) in &items {
                let kind = self
                    .manager
                    .registry
                    .entries()
                    .iter()
                    .find(|e| &e.path == path)
                    .map(|e| e.agent_kind.clone())
                    .unwrap_or_default();
                let _ = source.watch(path, kind);
            }
        }

        self.ensure_pane(window, cx);
        self.recompute_diff(cx);
        cx.notify();
    }

    // ---- folders ---------------------------------------------------------

    fn add_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next() {
                    let _ = view.update_in(cx, |ws, window, cx| ws.add_repo(path, window, cx));
                }
        })
        .detach();
    }

    fn add_repo(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match self.manager.git().repo_root(&path) {
            Ok(root) => {
                let name = root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !self.repo_roots.contains(&root) {
                    self.repo_roots.push(root);
                    save_repos(&self.repos_path, &self.repo_roots);
                    self.note = format!("added {name}");
                } else {
                    self.note = "folder already added".to_string();
                }
                self.refresh(window, cx);
                self.select(Selection::Main(name), window, cx);
            }
            Err(_) => {
                self.note = "not a git repository".to_string();
                cx.notify();
            }
        }
    }

    // ---- text edit -------------------------------------------------------

    fn begin_edit(&mut self, edit: Edit, window: &mut Window, cx: &mut Context<Self>) {
        self.edit = edit;
        self.menu = None;
        self.caret_on = true;
        self.blink_gen = self.blink_gen.wrapping_add(1);
        let blink_id = self.blink_gen;
        window.focus(&self.edit_focus, cx);
        cx.spawn(async move |view, cx| loop {
            cx.background_executor().timer(Duration::from_millis(530)).await;
            let cont = view
                .update(cx, |ws, cx| {
                    if ws.blink_gen != blink_id || matches!(ws.edit, Edit::None) {
                        false
                    } else {
                        ws.caret_on = !ws.caret_on;
                        cx.notify();
                        true
                    }
                })
                .unwrap_or(false);
            if !cont {
                break;
            }
        })
        .detach();
        cx.notify();
    }

    fn edit_text_mut(&mut self) -> Option<&mut String> {
        match &mut self.edit {
            Edit::NewName(t) | Edit::Rename(_, t) | Edit::Avatar(_, t) => Some(t),
            Edit::None => None,
        }
    }

    fn edit_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                self.edit = Edit::None;
                cx.notify();
            }
            "enter" => self.commit_edit(window, cx),
            "backspace" => {
                if let Some(t) = self.edit_text_mut() {
                    t.pop();
                }
                self.caret_on = true;
                cx.notify();
            }
            _ => {
                let m = &event.keystroke.modifiers;
                if let Some(ch) = event.keystroke.key_char.clone()
                    && !ch.is_empty() && !m.control && !m.platform {
                        if let Some(t) = self.edit_text_mut() {
                            t.push_str(&ch);
                        }
                        self.caret_on = true;
                        cx.notify();
                    }
            }
        }
        cx.stop_propagation();
    }

    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match std::mem::replace(&mut self.edit, Edit::None) {
            Edit::NewName(text) => {
                let name = text.trim().to_string();
                if !name.is_empty() {
                    self.create_named(&name, window, cx);
                } else {
                    cx.notify();
                }
            }
            Edit::Rename(id, text) => {
                let name = text.trim();
                if !name.is_empty() {
                    let _ = self.manager.registry.rename(&id, name);
                    let _ = self.manager.registry.save();
                }
                self.refresh(window, cx);
            }
            Edit::Avatar(id, text) => {
                let initials = text.trim().to_uppercase();
                if !initials.is_empty()
                    && let Some(e) = self.manager.registry.get(&id) {
                        let color = e.avatar.color;
                        let _ = self
                            .manager
                            .registry
                            .set_avatar(&id, worktrees::Avatar { initials, color });
                        let _ = self.manager.registry.save();
                    }
                self.refresh(window, cx);
            }
            Edit::None => {}
        }
    }

    /// Also fix the note so it names the branch and base actually used.
    fn create_named(&mut self, input: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.new_target_repo.clone();
        let Some(repo) = self.data.repos.iter().find(|r| r.name == target).or_else(|| self.data.repos.first()) else {
            self.note = "add a folder first".to_string();
            cx.notify();
            return;
        };
        let spec = NewWorktree::parse(input, &repo.compare_ref);
        let name = spec.name.as_str();
        let slug = slugify(name);
        let req = CreateRequest {
            repo: repo.name.clone(),
            repo_root: repo.repo_root.clone(),
            task_slug: slug.clone(),
            branch: spec.branch.clone().unwrap_or_else(|| format!("feat/{slug}")),
            base_ref: Some(spec.base_ref.clone()),
            created_at: now_stamp(),
            // pi is the reference runtime and the default (§4.4). Choosing a
            // peer per worktree is the natural next step in the create dialog.
            agent_kind: AgentKind::Pi,
        };
        match self.manager.create(req) {
            Ok(id) => {
                let _ = self.manager.registry.rename(&id, name);
                let _ = self.manager.registry.save();
                self.note = format!("created {id}");
                self.refresh(window, cx);
                self.select(Selection::Worktree(id), window, cx);
            }
            Err(e) => {
                self.note = format!("create failed: {e}");
                cx.notify();
            }
        }
    }

    fn set_avatar_color(&mut self, id: &str, color: u8, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = self.manager.registry.get(id) {
            let initials = e.avatar.initials.clone();
            let _ = self
                .manager
                .registry
                .set_avatar(id, worktrees::Avatar { initials, color });
            let _ = self.manager.registry.save();
        }
        self.refresh(window, cx);
    }

    fn archive(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match self.manager.archive(&id) {
            Ok(()) => self.note = format!("archived {id}"),
            Err(e) => self.note = format!("archive failed: {e}"),
        }
        self.refresh(window, cx);
        if let Some(name) = self.data.repos.first().map(|r| r.name.clone()) {
            self.select(Selection::Main(name), window, cx);
        }
    }

    fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match self.manager.delete(&id) {
            Ok(()) => self.note = format!("deleted {id}"),
            Err(e) => self.note = format!("delete failed: {e}"),
        }
        self.panes.remove(&id);
        self.pending_delete = None;
        self.refresh(window, cx);
        if let Some(name) = self.data.repos.first().map(|r| r.name.clone()) {
            self.select(Selection::Main(name), window, cx);
        }
    }

    // ---- rendering -------------------------------------------------------

    fn render_top_bar(&self, hx: &HarnessTheme) -> impl IntoElement {
        let branch = self.resolve().map(|r| r.branch).unwrap_or_else(|| "no folder".to_string());
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(38.0))
            .px_3()
            .bg(hx.bg0)
            .border_b_1()
            .border_color(hx.border)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child("openOrchestra"))
                    .child(div().text_xs().text_color(hx.tx2).child("·"))
                    .child(div().text_xs().font_family(hx.font_mono).text_color(hx.tx1).child(branch)),
            )
    }

    fn add_folder_button(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("add-folder")
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .mx_2()
            .mt_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(hx.border2)
            .text_sm()
            .text_color(hx.tx1)
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .on_click(cx.listener(|ws, _, window, cx| ws.add_folder(window, cx)))
            .child("＋ Add folder")
    }

    fn render_sidebar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(280.0))
            .h_full()
            .bg(hx.bg1)
            .border_r_1()
            .border_color(hx.border)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(34.0))
                    .px_3()
                    .border_b_1()
                    .border_color(hx.border)
                    .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx2).child("WORKTREES")),
            );

        if self.data.repos.is_empty() {
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .mt(px(40.0))
                    .px_3()
                    .child(div().text_sm().text_color(hx.tx1).child("No folders yet"))
                    .child(div().text_xs().text_color(hx.tx2).child("Open a git repository to begin.")),
            );
            return column.child(self.add_folder_button(hx, cx));
        }

        let mut groups = div().flex().flex_col().flex_1().overflow_hidden().pt_1();
        for repo in &self.data.repos {
            groups = groups.child(self.render_repo_group(hx, repo, cx));
        }
        column.child(groups).child(self.add_folder_button(hx, cx))
    }

    fn render_repo_group(
        &self,
        hx: &HarnessTheme,
        repo: &crate::data::RepoView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let collapsed = *self.collapsed.get(&repo.name).unwrap_or(&false);
        let repo_name = repo.name.clone();
        let plus_repo = repo.name.clone();
        let count = repo.worktrees.len();

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .child(
                div()
                    .id(("repo-toggle", repo.name.len()))
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        let e = ws.collapsed.entry(repo_name.clone()).or_insert(false);
                        *e = !*e;
                        cx.notify();
                    }))
                    .child(div().text_xs().text_color(hx.tx2).child(if collapsed { "▸" } else { "▾" }))
                    .child(div().flex_1().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child(repo.name.clone()))
                    .child(div().text_xs().text_color(hx.tx2).child(format!("{count} {}", if count == 1 { "tree" } else { "trees" }))),
            )
            .child(
                div()
                    .id(("repo-plus", repo.name.len()))
                    .size(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_color(hx.tx1)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.new_target_repo = plus_repo.clone();
                        ws.begin_edit(Edit::NewName(String::new()), window, cx);
                    }))
                    .child("+"),
            );

        let mut group = div().flex().flex_col().child(header);

        if !collapsed {
            let mut body = div()
                .flex()
                .flex_col()
                .ml(px(15.0))
                .pl_1()
                .border_l_1()
                .border_color(hx.border);
            body = body.child(self.render_main_entry(hx, repo, cx));
            if let Edit::NewName(text) = &self.edit
                && self.new_target_repo == repo.name {
                    body = body
                        .child(div().mx_1().my_0p5().child(self.render_input(hx, text, cx)))
                        // The optional syntax is invisible otherwise (M5a).
                        .child(
                            div()
                                .mx_2()
                                .mb_1()
                                .text_color(hx.tx2)
                                .text_size(px(10.0))
                                .child("name  ·  #branch  ·  @base-ref"),
                        );
                }
            for card in &repo.worktrees {
                body = body.child(self.render_card(hx, card, cx));
            }
            group = group.child(body);
        }
        group
    }

    fn render_main_entry(
        &self,
        hx: &HarnessTheme,
        repo: &crate::data::RepoView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selection == Selection::Main(repo.name.clone());
        let repo_name = repo.name.clone();
        div()
            .id(("main-entry", repo.name.len() + 1))
            .flex()
            .items_center()
            .gap_2()
            .mx_1()
            .my_0p5()
            .px_2()
            .py_1()
            .rounded_md()
            .when(selected, |d| d.bg(hx.surface))
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface))
            .on_click(cx.listener(move |ws, _, window, cx| {
                ws.select(Selection::Main(repo_name.clone()), window, cx);
            }))
            .child(
                div()
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(tint(hx.accent, 0.15))
                    .text_xs()
                    .text_color(hx.accent)
                    .child("⌂"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().text_color(hx.tx0).child("main"))
                    .child(div().text_xs().text_color(hx.tx2).child(format!("default · {}", repo.default_branch))),
            )
            .child(div().text_xs().text_color(hx.green).child("✓"))
    }

    fn render_card(
        &self,
        hx: &HarnessTheme,
        card: &crate::data::WorktreeView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selection == Selection::Worktree(card.id.clone());
        let id = card.id.clone();
        let renaming = matches!(&self.edit, Edit::Rename(rid, _) if rid == &id);
        let avatar_open = matches!(&self.edit, Edit::Avatar(aid, _) if aid == &id);

        let avatar_id = id.clone();
        let avatar = div()
            .id(("avatar", card.id.len()))
            .size(px(26.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(avatar_color(card.color))
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(0xffffff))
            .cursor_pointer()
            .on_click(cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                let cur = ws
                    .manager
                    .registry
                    .get(&avatar_id)
                    .map(|e| e.avatar.initials.clone())
                    .unwrap_or_default();
                ws.begin_edit(Edit::Avatar(avatar_id.clone(), cur), window, cx);
            }))
            .child(card.initials.clone());

        let name_el: AnyElement = if renaming {
            let text = if let Edit::Rename(_, t) = &self.edit { t.clone() } else { String::new() };
            self.render_input(hx, &text, cx).into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx0)
                .overflow_hidden()
                .child(card.name.clone())
                .into_any_element()
        };

        let agent = self.agent_status.get(&card.path).copied().unwrap_or(AgentState::Unknown);
        let header = div().flex().items_center().gap_2().child(avatar).child(name_el).child(agent_pill(hx, agent));

        let fs = self.forge_status.get(&card.branch);
        let mut badges = div().flex().items_center().gap_2().mt_1();
        if let Some(pr) = fs.and_then(|s| s.pr.as_ref()) {
            let c = match pr.state {
                PrState::Open => hx.green,
                PrState::Draft => hx.tx1,
                PrState::Merged => rgb(0xa877f5),
                PrState::Closed => hx.red,
            };
            badges = badges.child(div().text_xs().font_family(hx.font_mono).text_color(c).child(format!("#{}", pr.number)));
        }
        if card.ahead > 0 {
            badges = badges.child(div().text_xs().text_color(hx.green).child(format!("↑{}", card.ahead)));
        }
        if card.behind > 0 {
            badges = badges.child(div().text_xs().text_color(hx.amber).child(format!("↓{}", card.behind)));
        }
        if card.dirty > 0 {
            badges = badges.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(hx.amber)
                    .child(div().size(px(5.0)).rounded_full().bg(hx.amber))
                    .child(format!("~{}", card.dirty)),
            );
        }
        if let Some((c, label)) = fs.and_then(|s| ci_summary(hx, &s.checks)) {
            badges = badges.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(c)
                    .child(div().size(px(5.0)).rounded_full().bg(c))
                    .child(label),
            );
        }

        let mut card_div = div()
            .id(("card", card.id.len() + 7))
            .relative()
            .flex()
            .flex_col()
            .mx_1()
            .my_0p5()
            .p_2()
            .rounded_md()
            .when(selected, |d| d.bg(hx.surface).border_l_1().border_color(hx.accent))
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface))
            .on_click(cx.listener({
                let id = id.clone();
                move |ws, _, window, cx| ws.select(Selection::Worktree(id.clone()), window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let id = id.clone();
                    move |ws, ev: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        ws.pending_delete = None;
                        ws.menu = Some(CardMenu {
                            id: id.clone(),
                            x: ev.position.x,
                            y: ev.position.y,
                        });
                        cx.notify();
                    }
                }),
            )
            .child(header)
            .child(div().mt_1().text_xs().font_family(hx.font_mono).text_color(hx.tx1).overflow_hidden().child(card.branch.clone()))
            .child(badges);

        if avatar_open {
            card_div = card_div.child(self.render_avatar_popover(hx, &id, cx));
        }
        card_div
    }

    fn render_avatar_popover(
        &self,
        hx: &HarnessTheme,
        id: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let initials = if let Edit::Avatar(_, t) = &self.edit { t.clone() } else { String::new() };
        let mut swatches = div().flex().flex_wrap().gap_1();
        for ix in 0..8u8 {
            let sid = id.to_string();
            swatches = swatches.child(
                div()
                    .id(("swatch", ix as usize))
                    .size(px(22.0))
                    .rounded_md()
                    .bg(avatar_color(ix))
                    .cursor_pointer()
                    .hover(|d| d.border_1().border_color(hx.tx0))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.set_avatar_color(&sid, ix, window, cx);
                    })),
            );
        }
        div()
            .absolute()
            .top(px(40.0))
            .left(px(8.0))
            .w(px(212.0))
            .p_2()
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .child(div().text_xs().text_color(hx.tx2).mb_1().child("INITIALS"))
            .child(self.render_input(hx, &initials, cx))
            .child(div().text_xs().text_color(hx.tx2).mt_2().mb_1().child("COLOR"))
            .child(swatches)
    }

    fn render_input(&self, hx: &HarnessTheme, text: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("edit-input")
            .track_focus(&self.edit_focus)
            .on_key_down(cx.listener(Self::edit_key))
            .flex()
            .items_center()
            .flex_1()
            .min_w_0()
            .h(px(22.0))
            .px_1p5()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.accent)
            .text_sm()
            .text_color(hx.tx0)
            .child(div().min_w_0().overflow_hidden().child(text.to_string()))
            .when(self.caret_on, |d| d.child(div().w(px(2.0)).h(px(14.0)).ml(px(1.0)).bg(hx.accent)))
    }

    fn render_context_menu(&self, hx: &HarnessTheme, menu: &CardMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let confirming = self.pending_delete.as_deref() == Some(menu.id.as_str());
        let rename_id = menu.id.clone();
        let archive_id = menu.id.clone();
        let del_id = menu.id.clone();

        div()
            .absolute()
            .left(menu.x)
            .top(menu.y)
            .w(px(170.0))
            .p(px(4.0))
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .flex()
            .flex_col()
            .gap_1()
            .child(self.menu_item(hx, "Rename", false, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                let name = ws.manager.registry.get(&rename_id).map(|e| e.name.clone()).unwrap_or_default();
                ws.select(Selection::Worktree(rename_id.clone()), window, cx);
                ws.begin_edit(Edit::Rename(rename_id.clone(), name), window, cx);
            })))
            .child(self.menu_item(hx, "Archive", false, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.archive(archive_id.clone(), window, cx);
            })))
            .child(self.menu_item(hx, if confirming { "Confirm delete?" } else { "Delete" }, true, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                if ws.pending_delete.as_deref() == Some(del_id.as_str()) {
                    ws.delete(del_id.clone(), window, cx);
                } else {
                    ws.pending_delete = Some(del_id.clone());
                    cx.notify();
                }
            })))
    }

    fn menu_item(
        &self,
        hx: &HarnessTheme,
        label: &str,
        danger: bool,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(("menu-item", label.len()))
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .text_color(if danger { hx.red } else { hx.tx1 })
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface_hover).text_color(if danger { hx.red } else { hx.tx0 }))
            .on_click(handler)
            .child(label.to_string())
    }

    fn render_center(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let Some(r) = self.resolve() else {
            return self.render_empty_center(hx, cx).into_any_element();
        };
        let pane = self.panes.get(&r.key);
        let active_tab = pane.map(|p| p.active_tab).unwrap_or(ActiveTab::Editor);
        let content: AnyElement = match (pane, active_tab) {
            (Some(p), ActiveTab::Terminal) => p.terminal.clone().into_any_element(),
            (Some(p), ActiveTab::Editor) => p.editor.clone().into_any_element(),
            (Some(p), ActiveTab::Files) => match &p.files {
                Some(ws) => ws.clone().into_any_element(),
                // Still being constructed on the first switch to this tab.
                None => div()
                    .p_3()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("opening project …")
                    .into_any_element(),
            },
            (None, _) => div().into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .bg(hx.bg0)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(34.0))
                    .px_2()
                    .gap_1()
                    .bg(hx.bg1)
                    .border_b_1()
                    .border_color(hx.border)
                    .child(self.render_tab(hx, format!("❯_ {}", r.branch), ActiveTab::Terminal, active_tab == ActiveTab::Terminal, cx))
                    .child(self.render_tab(hx, "{ } editor".to_string(), ActiveTab::Editor, active_tab == ActiveTab::Editor, cx))
                    .child(self.render_tab(hx, "▤ files".to_string(), ActiveTab::Files, active_tab == ActiveTab::Files, cx)),
            )
            .child(div().flex_1().min_h_0().child(content))
            .child(self.render_status_bar(hx, Some(&r)))
            .into_any_element()
    }

    fn render_empty_center(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(hx.bg0)
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx1).child("No folder open"))
            .child(div().text_sm().text_color(hx.tx2).child("Open a git repository to start orchestrating worktrees."))
            .child(
                div()
                    .id("empty-add")
                    .mt_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(tint(hx.accent, 0.16))
                    .text_sm()
                    .text_color(hx.accent)
                    .cursor_pointer()
                    .hover(|d| d.bg(tint(hx.accent, 0.24)))
                    .on_click(cx.listener(|ws, _, window, cx| ws.add_folder(window, cx)))
                    .child("＋ Add a folder"),
            )
    }

    fn render_tab(
        &self,
        hx: &HarnessTheme,
        label: String,
        tab: ActiveTab,
        active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(match tab {
                ActiveTab::Terminal => "tab-terminal",
                ActiveTab::Editor => "tab-editor",
                ActiveTab::Files => "tab-files",
            })
            .flex()
            .items_center()
            .h(px(26.0))
            .px_3()
            .rounded_md()
            .when(active, |d| d.bg(hx.bg0).border_1().border_color(hx.border))
            .text_xs()
            .font_family(hx.font_mono)
            .text_color(if active { hx.tx0 } else { hx.tx2 })
            .cursor_pointer()
            .hover(|d| d.text_color(hx.tx1))
            .on_click(cx.listener(move |ws, _, window, cx| {
                let Some(r) = ws.resolve() else { return };
                if let Some(pane) = ws.panes.get_mut(&r.key) {
                    pane.active_tab = tab;
                }
                if tab == ActiveTab::Files {
                    ws.ensure_files_workspace(&r, window, cx);
                }
                ws.focus_active_pane(window, cx);
                cx.notify();
            }))
            .child(label)
    }

    fn render_status_bar(&self, hx: &HarnessTheme, r: Option<&Resolved>) -> impl IntoElement {
        let branch = r.map(|r| r.branch.clone()).unwrap_or_default();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .h(px(24.0))
            .px_3()
            .bg(hx.bg1)
            .border_t_1()
            .border_color(hx.border)
            .text_xs()
            .child(div().font_family(hx.font_mono).text_color(hx.tx1).child(branch))
            .child(div().flex_1())
            .child(div().text_color(hx.tx2).child(self.note.clone()))
            .child(tool_chip(hx, "git", self.tools.git, true))
            .child(tool_chip(hx, "gh", self.tools.gh, false))
            .child(tool_chip(hx, "pi", self.tools.pi, false))
    }

    fn unified_for_selected(&self) -> String {
        let Some(path) = &self.diff_file else {
            return self.diff.unified.clone();
        };
        let mut blocks: Vec<String> = Vec::new();
        let mut cur = String::new();
        for line in self.diff.unified.lines() {
            if line.starts_with("diff --git") && !cur.is_empty() {
                blocks.push(std::mem::take(&mut cur));
            }
            cur.push_str(line);
            cur.push('\n');
        }
        if !cur.is_empty() {
            blocks.push(cur);
        }
        blocks
            .into_iter()
            .find(|b| b.lines().next().is_some_and(|h| h.contains(&format!("b/{path}")) || h.ends_with(path)))
            .unwrap_or_else(|| self.diff.unified.clone())
    }

    fn render_diff_panel(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let Some(r) = self.resolve() else {
            return div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(340.0))
                .h_full()
                .items_center()
                .justify_center()
                .gap_2()
                .px_3()
                .bg(hx.bg1)
                .border_l_1()
                .border_color(hx.border)
                .child(div().text_sm().text_color(hx.tx1).child("No project or folder"))
                .child(
                    div()
                        .text_xs()
                        .text_color(hx.tx2)
                        .child("There is no project / folder selected or created. Create your first one!"),
                )
                .into_any_element();
        };
        let (adds, dels) = self.diff.totals();

        let mut panel = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(340.0))
            .h_full()
            .bg(hx.bg1)
            .border_l_1()
            .border_color(hx.border)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(34.0))
                    .px_3()
                    .border_b_1()
                    .border_color(hx.border)
                    .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx2).child("DIFF"))
                    .child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono).text_color(hx.tx1).overflow_hidden().child(r.branch.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child(
                        if self.diff_pending {
                            "Changed files  …".to_string()
                        } else {
                            format!("Changed files  {}", self.diff.files.len())
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .child(div().text_color(hx.green).child(format!("+{adds}")))
                            .child(div().text_color(hx.red).child(format!("−{dels}"))),
                    ),
            );

        let mut files = div().flex().flex_col().flex_none().px_2().pb_1();
        if self.diff.files.is_empty() {
            files = files.child(div().px_2().py_1().text_xs().text_color(hx.tx2).child("no changes vs merge-base"));
        }
        for (i, file) in self.diff.files.iter().enumerate() {
            let path = file.path.clone();
            let is_sel = self.diff_file.as_deref() == Some(file.path.as_str());
            let badge_color = match file.change {
                Change::Added => hx.green,
                Change::Deleted => hx.red,
                Change::Renamed => hx.accent,
                _ => hx.amber,
            };
            files = files.child(
                div()
                    .id(("difffile", i))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .when(is_sel, |d| d.bg(hx.surface))
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        ws.diff_file = if ws.diff_file.as_deref() == Some(path.as_str()) {
                            None
                        } else {
                            Some(path.clone())
                        };
                        cx.notify();
                    }))
                    .child(div().flex_none().w(px(12.0)).text_xs().font_family(hx.font_mono).text_color(badge_color).child(file.change.badge().to_string()))
                    .child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono).text_color(hx.tx1).overflow_hidden().child(file.path.clone()))
                    .child(div().flex_none().text_xs().font_family(hx.font_mono).text_color(hx.green).child(format!("+{}", file.adds)))
                    .child(div().flex_none().text_xs().font_family(hx.font_mono).text_color(hx.red).child(format!("−{}", file.dels))),
            );
        }
        panel = panel.child(files);

        let mut preview = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .m_2()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.border)
            .overflow_hidden()
            .text_xs()
            .font_family(hx.font_mono);
        let unified = self.unified_for_selected();
        if unified.is_empty() {
            preview = preview.child(div().p_2().text_color(hx.tx2).child("—"));
        } else {
            for line in unified.lines().take(600) {
                let (fg, bg) = if line.starts_with("@@") {
                    (hx.accent, tint(hx.accent, 0.10))
                } else if line.starts_with('+') {
                    (rgb(DIFF_ADD_TX), tint(hx.green, 0.13))
                } else if line.starts_with('-') {
                    (rgb(DIFF_DEL_TX), tint(hx.red, 0.13))
                } else {
                    (hx.tx2, Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 })
                };
                preview = preview.child(div().flex_none().h(px(15.0)).px_2().bg(bg).text_color(fg).overflow_hidden().child(line.to_string()));
            }
        }
        panel.child(preview).into_any_element()
    }
}

impl Focusable for HarnessWorkspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HarnessWorkspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hx = theme(cx).clone();
        let mut root = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(hx.bg0)
            .font_family(hx.font_ui)
            .text_color(hx.tx0)
            .child(self.render_top_bar(&hx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(&hx, cx))
                    .child(self.render_center(&hx, cx))
                    .child(self.render_diff_panel(&hx, cx)),
            );

        if let Some(m) = &self.menu {
            let menu = CardMenu { id: m.id.clone(), x: m.x, y: m.y };
            root = root.child(
                div()
                    .id("menu-backdrop")
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .size_full()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _, _, cx| {
                            ws.menu = None;
                            ws.pending_delete = None;
                            cx.notify();
                        }),
                    ),
            );
            root = root.child(self.render_context_menu(&hx, &menu, cx));
        }
        root
    }
}

fn agent_pill(hx: &HarnessTheme, state: AgentState) -> impl IntoElement {
    let (color, label) = match state {
        AgentState::Running => (hx.accent, "RUNNING"),
        AgentState::Waiting => (hx.amber, "WAITING"),
        AgentState::Idle | AgentState::Unknown => (hx.tx2, "IDLE"),
    };
    let bg = if matches!(state, AgentState::Idle | AgentState::Unknown) {
        hx.surface2
    } else {
        tint(color, 0.16)
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1p5()
        .py_0p5()
        .rounded_full()
        .bg(bg)
        .text_xs()
        .text_color(color)
        .child(div().size(px(6.0)).rounded_full().bg(color))
        .child(label)
}

fn ci_summary(hx: &HarnessTheme, checks: &[forge::CheckRun]) -> Option<(Rgba, &'static str)> {
    if checks.is_empty() {
        return None;
    }
    if checks.iter().any(|c| c.status == CheckStatus::Fail) {
        Some((hx.red, "ci"))
    } else if checks.iter().any(|c| c.status == CheckStatus::Running) {
        Some((hx.amber, "ci"))
    } else if checks.iter().any(|c| c.status == CheckStatus::Pass) {
        Some((hx.green, "ci"))
    } else {
        None
    }
}

fn avatar_color(ix: u8) -> Rgba {
    rgb(SWATCHES[(ix as usize) % SWATCHES.len()])
}

fn detect_tools() -> ToolStatus {
    ToolStatus {
        git: on_path("git"),
        gh: on_path("gh"),
        pi: on_path("pi"),
    }
}

fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(bin);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            candidate
                .metadata()
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            candidate.is_file()
        }
    })
}

fn tool_chip(hx: &HarnessTheme, label: &str, present: bool, required: bool) -> impl IntoElement {
    let color = if present {
        hx.green
    } else if required {
        hx.red
    } else {
        hx.tx2
    };
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(div().size(px(5.0)).rounded_full().bg(color))
        .child(div().text_color(color).child(label.to_string()))
}

fn load_repos(path: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_repos(path: &Path, repos: &[PathBuf]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(repos) {
        let _ = std::fs::write(path, json);
    }
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

/// What the new-worktree input asked for (M5a).
///
/// The plain case stays plain — typing a name is all most tasks need, and it
/// behaves exactly as before. Two optional suffixes cover the cases the
/// auto-derived branch and base could not express:
///
/// ```text
///   fix login bug                     -> branch feat/fix-login-bug, base = compare ref
///   fix login bug #hotfix/login       -> explicit branch
///   fix login bug @v2.1               -> explicit base ref
///   fix login bug #hotfix/login @v2.1 -> both
/// ```
///
/// Chosen over a bespoke modal deliberately: it delivers the same capability
/// against far less unstable GPUI surface, and the sidebar input already
/// exists and is already focused when you press `+`.
#[derive(Debug, PartialEq)]
struct NewWorktree {
    name: String,
    branch: Option<String>,
    base_ref: String,
}

impl NewWorktree {
    fn parse(input: &str, default_base: &str) -> NewWorktree {
        let mut name_parts: Vec<&str> = Vec::new();
        let mut branch = None;
        let mut base_ref = None;

        for token in input.split_whitespace() {
            match token.split_at_checked(1) {
                // A bare "#" or "@" carries no value; treat it as part of the
                // name rather than silently swallowing it.
                Some(("#", rest)) if !rest.is_empty() => branch = Some(rest.to_string()),
                Some(("@", rest)) if !rest.is_empty() => base_ref = Some(rest.to_string()),
                _ => name_parts.push(token),
            }
        }

        NewWorktree {
            name: name_parts.join(" "),
            branch,
            base_ref: base_ref.unwrap_or_else(|| default_base.to_string()),
        }
    }
}

#[cfg(test)]
mod new_worktree_tests {
    use super::NewWorktree;

    #[test]
    fn a_plain_name_keeps_the_previous_behavior() {
        let spec = NewWorktree::parse("fix login bug", "origin/main");
        assert_eq!(spec.name, "fix login bug");
        assert_eq!(spec.branch, None, "branch stays auto-derived");
        assert_eq!(spec.base_ref, "origin/main");
    }

    #[test]
    fn an_explicit_branch_and_base_are_picked_up_in_any_order() {
        let spec = NewWorktree::parse("fix login @v2.1 #hotfix/login bug", "origin/main");
        assert_eq!(spec.name, "fix login bug");
        assert_eq!(spec.branch.as_deref(), Some("hotfix/login"));
        assert_eq!(spec.base_ref, "v2.1");
    }

    #[test]
    fn bare_markers_stay_part_of_the_name() {
        // Otherwise a name containing a stray "#" would silently vanish.
        let spec = NewWorktree::parse("issue # 42 @", "origin/main");
        assert_eq!(spec.name, "issue # 42 @");
        assert_eq!(spec.branch, None);
        assert_eq!(spec.base_ref, "origin/main");
    }

    #[test]
    fn the_last_marker_of_each_kind_wins() {
        let spec = NewWorktree::parse("x #a #b @c @d", "origin/main");
        assert_eq!(spec.branch.as_deref(), Some("b"));
        assert_eq!(spec.base_ref, "d");
    }
}
