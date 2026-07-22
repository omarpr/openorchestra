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
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PathPromptOptions,
    Pixels, Render, Rgba, Styled, Window, div, prelude::*, px, rgb,
};

use agent_runtime::{
    runtime_for, AgentKind, AgentSignal, AgentState, Bus, StatusFileSource, WaitFidelity,
};
use forge::{BranchKey, BranchStatus, CheckStatus, ForgeUpdate, GhCli, Poller, PollerConfig, PrState};
use worktrees::{Change, CreateRequest, GitWorktrees, Manager, Registry, SystemRunner};

use crate::data::{compute_diff, WorkspaceData};
use crate::terminal_pane::TerminalPane;
use crate::theme::{theme, tint, HarnessTheme};

const SWATCHES: [u32; 8] = [
    0x5b9bff, 0x4ec26b, 0xa877f5, 0xe0a83b, 0xf0616d, 0x22b8cf, 0xf27bb0, 0x8b93a0,
];
/// Strip git's per-file metadata preamble from a unified diff.
///
/// `diff --git`, `index <sha>..<sha>`, mode/rename lines, and the `---` / `+++`
/// pair carry nothing a reader needs: the file being viewed is already named in
/// the changed-files list above, and in the all-files view we render our own
/// header. Left in, they cost five lines of noise at the top of every file —
/// which on a short change is most of what you can see.
///
/// `@@` hunk headers are kept: they carry the line numbers, which is real
/// navigational information.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum DiffLineKind {
    /// A synthesized per-file header, replacing git's `diff --git` preamble.
    FileHeader,
    /// A hunk boundary. Carries the enclosing section (the text git puts
    /// *after* the second `@@`) rather than the coordinates.
    HunkBreak,
    Added,
    Removed,
    Context,
}

/// One rendered row of the diff.
#[derive(Debug, PartialEq, Clone)]
pub struct DiffRow {
    pub kind: DiffLineKind,
    /// Line number for the gutter — the file's real line number, which is what
    /// the `@@ -a,b +c,d @@` coordinates encoded far less directly. Removed
    /// lines carry the *old* file's number, everything else the new file's.
    pub number: Option<u32>,
    pub text: String,
}

/// Parse a `@@ -old,n +new,n @@ section` header.
///
/// Returns the starting line numbers plus the trailing section text, which git
/// fills with the enclosing function or heading. The coordinates become the
/// gutter; the section is the only part worth showing as text.
fn parse_hunk(line: &str) -> Option<(u32, u32, String)> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, section) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;
    let start = |spec: &str| -> Option<u32> {
        spec.trim_start_matches(['-', '+'])
            .split(',')
            .next()?
            .parse()
            .ok()
    };
    Some((start(old)?, start(new)?, section.trim().to_string()))
}

pub fn readable_diff_lines(unified: &str) -> Vec<DiffRow> {
    const NOISE: [&str; 7] = [
        "index ",
        "new file mode ",
        "deleted file mode ",
        "old mode ",
        "new mode ",
        "similarity index ",
        "dissimilarity index ",
    ];
    let row = |kind, number, text: String| DiffRow { kind, number, text };

    let mut out: Vec<DiffRow> = Vec::new();
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut first_hunk_of_file = true;

    for line in unified.lines() {
        // The `diff --git a/X b/Y` line is where a file starts. We drop the
        // command itself but keep the boundary, as a header — without it the
        // all-files view would run every file together.
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `diff --git a/<old> b/<new>` — take the new path, which is the
            // one that matters for a rename.
            let path = rest.rsplit_once(" b/").map(|(_, new)| new).unwrap_or(rest);
            out.push(row(DiffLineKind::FileHeader, None, path.to_string()));
            first_hunk_of_file = true;
            continue;
        }
        if NOISE.iter().any(|p| line.starts_with(p)) {
            continue;
        }
        // `--- a/x` and `+++ b/x` are the file pair, not content. Real content
        // lines are a single `-`/`+` followed by the text, so requiring the
        // triple form avoids eating a legitimate `--foo` deletion.
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename from ") {
            out.push(row(DiffLineKind::Context, None, format!("renamed from {rest}")));
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename to ") {
            out.push(row(DiffLineKind::Context, None, format!("renamed to {rest}")));
            continue;
        }

        if let Some((old, new, section)) = parse_hunk(line) {
            old_no = old;
            new_no = new;
            // The first hunk right after a file header needs no separator —
            // the header already marks the boundary — unless git gave us a
            // section heading, which is worth showing on its own. Later hunks
            // always get one, because the line numbers jump.
            if !first_hunk_of_file || !section.is_empty() {
                out.push(row(DiffLineKind::HunkBreak, None, section));
            }
            first_hunk_of_file = false;
            continue;
        }

        // `\ No newline at end of file` is git commentary, not content.
        if line.starts_with('\\') {
            out.push(row(DiffLineKind::Context, None, line.trim_start_matches('\\').trim().to_string()));
            continue;
        }

        // Strip the leading +/-/space marker: the gutter and the row's tint
        // already say what happened, and keeping it shifts every line by one
        // column against the file's real indentation.
        let (kind, number, text) = if let Some(rest) = line.strip_prefix('+') {
            let n = new_no;
            new_no += 1;
            (DiffLineKind::Added, Some(n), rest)
        } else if let Some(rest) = line.strip_prefix('-') {
            let n = old_no;
            old_no += 1;
            (DiffLineKind::Removed, Some(n), rest)
        } else {
            let rest = line.strip_prefix(' ').unwrap_or(line);
            let n = new_no;
            new_no += 1;
            old_no += 1;
            (DiffLineKind::Context, Some(n), rest)
        };
        out.push(row(kind, number, text.to_string()));
    }
    out
}


/// Cap on rendered diff lines.
///
/// Each line is its own element, so an enormous diff would build tens of
/// thousands of them and stall the frame. Set well above any realistic
/// single-file hunk; when it bites, the UI says so rather than truncating
/// silently.
const MAX_DIFF_LINES: usize = 5000;

const DIFF_ADD_TX: u32 = 0x7fdd97;
const DIFF_DEL_TX: u32 = 0xf2868f;

/// What a tab can hold. Tab sets are per worktree and fully dynamic (§3.4):
/// a worktree can carry any number of terminals, editors, and chats.
enum TabContent {
    Terminal(Entity<TerminalPane>),
    /// A real zed `Workspace` — this is what carries the file explorer, ⌘P,
    /// ⌘⇧F, and LSP (§3.6).
    Editor(Entity<::workspace::Workspace>),
    /// Native agent chat over pi's RPC mode (M8, §4.2–4.3).
    Chat(Entity<crate::chat_pane::ChatPane>),
}

/// What to open, before the entity exists.
#[derive(Clone, PartialEq)]
enum TabKind {
    /// A plain `$SHELL`. What a worktree opens with.
    Shell,
    /// A specific agent CLI (§4.5) — any of the five, not only the
    /// worktree's recorded default.
    Agent(AgentKind),
    Editor,
    Chat,
}

struct Tab {
    /// When this tab hosts an agent CLI, its kind — the tab strip renders the
    /// agent's colored mark so five agent tabs stay tellable at a glance.
    agent: Option<AgentKind>,
    /// Stable per-pane-set id, so closing a tab cannot renumber the others out
    /// from under a click handler.
    id: u64,
    title: String,
    content: TabContent,
}

#[derive(Clone, PartialEq)]
#[derive(Debug)]
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
    /// Editing an agent's launch command in settings (kind string, text).
    AgentCmd(String, String),
}

/// The name an agent goes by in front of users — notification titles, rows.
fn agent_display(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::Pi => "pi",
        AgentKind::ClaudeCode => "Claude Code",
        AgentKind::Codex => "Codex",
        AgentKind::Kimi => "Kimi",
        AgentKind::OpenCode => "OpenCode",
        AgentKind::Other(_) => "Agent",
    }
}

/// An agent's monogram mark and brand-adjacent color, shared by the ＋ menu
/// and the settings screen.
fn agent_mark(kind: &AgentKind) -> (&'static str, Rgba) {
    match kind {
        AgentKind::Pi => ("π", rgb(0x8b7cf6)),
        AgentKind::ClaudeCode => ("✱", rgb(0xd97757)),
        AgentKind::Codex => ("⬡", rgb(0x10a37f)),
        AgentKind::Kimi => ("K", rgb(0x4f9cf9)),
        AgentKind::OpenCode => ("⟡", rgb(0x3fb950)),
        AgentKind::Other(_) => ("◇", rgb(0x9aa3af)),
    }
}

/// Per-agent user preferences (settings screen). Both fields default to the
/// permissive state so a missing entry behaves like a fresh install.
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, Default)]
struct AgentPref {
    #[serde(default)]
    disabled: bool,
    /// Launch-command override, run verbatim via `$SHELL -lc`. `None` = the
    /// adapter's own resolved command.
    #[serde(default)]
    command: Option<String>,
}

/// Which splitter is currently being dragged (§3.4: panels are resizable).
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    /// Left worktree sidebar width.
    Sidebar,
    /// Right diff panel width. Grows leftward, so the delta is negated.
    DiffPanel,
}

/// A splitter drag in progress.
///
/// Tracked as a start position plus a start value rather than reading absolute
/// coordinates, so a handle does not need to know where it sits in the layout
/// and the pointer can drift off the 6px handle mid-drag without losing it.
#[derive(Clone, Copy)]
struct DragState {
    target: Drag,
    start: Pixels,
    start_value: f32,
}

/// An in-progress inline comment, plus where to anchor its popover.
#[derive(Clone)]
struct CommentDraft {
    file: String,
    line: u32,
    body: String,
    /// Window coordinates of the gutter cell that opened it.
    at: gpui::Point<Pixels>,
}

struct CardMenu {
    id: String,
    x: Pixels,
    y: Pixels,
}

struct PaneSet {
    tabs: Vec<Tab>,
    /// Id of the active tab, not an index — indices shift when a tab closes.
    active: u64,
    next_id: u64,
    /// Monotonic counter, bumped on every touch, for LRU eviction (M5b).
    last_used: u64,
}

impl PaneSet {
    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active)
    }
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
    /// Files whose diff is expanded. A set, not a single selection: reviewing
    /// usually means comparing two files, and collapsing one to look at
    /// another loses your place in the first.
    expanded: std::collections::HashSet<String>,
    /// Selected diff rows: which file, and an inclusive row range within that
    /// file's body. Scoped per file because indices restart in each body.
    diff_selection: Option<(String, usize, usize)>,
    /// True while dragging a selection.
    diff_selecting: bool,
    /// Inline review comments, keyed by worktree (§3.4).
    review: crate::diff_review::ReviewNotes,
    review_path: PathBuf,
    /// The `(file, line)` currently being commented on, and the draft text.
    comment_draft: Option<CommentDraft>,
    /// First click of the two-click "clear all comments" confirm.
    clearing_review: bool,
    /// The settings screen (⌘, or the gear). Transient, never persisted.
    settings_open: bool,
    /// The conductor pet: message shown, expiry, and which beat of the
    /// two-pose arm cycle it is on.
    pet: Option<(String, std::time::Instant)>,
    pet_pose: bool,
    /// A shortcut row waiting for its new chord (`shortcuts::key_id`).
    rebinding: Option<String>,
    /// Focus target while capturing a chord.
    rebind_focus: FocusHandle,
    /// Rendered-markdown preview of the selected `.md` file, when the user has
    /// switched to it. Rebuilt on selection, since it snapshots file contents.
    md_preview: Option<(String, Entity<crate::markdown_preview::MarkdownPreview>)>,
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
    /// Open "new tab" menu, anchored under the `+` in the tab strip.
    tab_menu: Option<Pixels>,
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
    prefs: Prefs,
    prefs_path: PathBuf,
    drag: Option<DragState>,
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
        let prefs_path = data_dir.join("prefs.json");
        let prefs = load_prefs(&prefs_path);
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
            expanded: std::collections::HashSet::new(),
            diff_selection: None,
            diff_selecting: false,
            review: crate::diff_review::ReviewNotes::load(
                &crate::diff_review::ReviewNotes::path_in(&data_dir),
            ),
            review_path: crate::diff_review::ReviewNotes::path_in(&data_dir),
            comment_draft: None,
            clearing_review: false,
            settings_open: false,
            pet: None,
            pet_pose: false,
            rebinding: None,
            rebind_focus: cx.focus_handle(),
            md_preview: None,
            diff_gen: 0,
            diff_pending: false,
            pane_clock: 0,
            edit: Edit::None,
            edit_focus: cx.focus_handle(),
            caret_on: true,
            blink_gen: 0,
            new_target_repo,
            menu: None,
            tab_menu: None,
            pending_delete: None,
            note: String::new(),
            forge_status: HashMap::new(),
            agent_status: HashMap::new(),
            agent_fidelity: HashMap::new(),
            _poller: Some(poller),
            status_source,
            state_dir: data_dir.clone(),
            prefs,
            prefs_path,
            drag: None,
            bus,
            tools: detect_tools(),
            focus_handle: cx.focus_handle(),
        };
        this.ensure_pane(window, cx);
        // Restore persisted appearance (zed theme + fonts). main.rs already
        // installed the harness pair pre-window to avoid a theme flash; this
        // only does further work when prefs carry overrides.
        if this.prefs.zed_theme.is_some()
            || this.prefs.font_ui.is_some()
            || this.prefs.font_mono.is_some()
            || this.prefs.font_size.is_some()
        {
            this.apply_appearance(cx);
        }
        // Test hook: drive the whole notification chain with no agent
        // installed — write real status.json transitions into the first
        // registered worktree and let the watcher, bus, policy, and notifier
        // react. Expect a "NOTIFY posting" log for the Running→Idle edge.
        if std::env::var_os("HARNESS_PROBE_NOTIFY").is_some()
            && let Some(entry) = this.manager.registry.entries().first().cloned()
        {
            cx.spawn(async move |_, cx| {
                let write = |state: &str| {
                    let dir = entry.path.join(".harness");
                    let _ = std::fs::create_dir_all(&dir);
                    // Claims claude-code so the probe also proves the writer
                    // beats the registered kind (the "pi finished" mixup).
                    let body = format!(
                        "{{\n\t\"state\": \"{state}\",\n\t\"agent\": \"claude-code\",\n\t\"timestamp\": \"2026-01-01T00:00:00Z\",\n\t\"sessionFile\": null\n}}\n",
                    );
                    let tmp = dir.join(".probe.tmp");
                    let _ = std::fs::write(&tmp, body);
                    let _ = std::fs::rename(&tmp, dir.join("status.json"));
                };
                cx.background_executor().timer(Duration::from_secs(3)).await;
                write("running");
                cx.background_executor().timer(Duration::from_secs(2)).await;
                write("idle");
                log::info!("HARNESS_PROBE_NOTIFY: wrote running→idle transitions");
            })
            .detach();
        }
        // Test hook: apply a zed theme by name at startup, reporting what the
        // registry holds and what the chrome derived — the settings screen's
        // theme chips drive exactly this path.
        if let Some(name) = std::env::var_os("HARNESS_PROBE_THEME") {
            let name = name.to_string_lossy().into_owned();
            let count = ::theme::ThemeRegistry::global(cx).list_names().len();
            this.prefs.zed_theme = Some(name.clone());
            this.apply_appearance(cx);
            log::info!(
                "HARNESS_PROBE_THEME: registry={count} themes; applied {name:?}; bg0={:?} accent={:?}",
                theme(cx).bg0,
                theme(cx).accent,
            );
        }
        // Test hook: open an editor tab without going through the ＋ menu, so
        // "the menu is broken" and "the editor is broken" can be told apart.
        // A value naming a worktree id selects that worktree first, so the
        // editor can be probed against a real linked worktree — the thing a
        // user actually clicks — and not only against a repo root.
        if let Some(v) = std::env::var_os("HARNESS_OPEN_EDITOR") {
            let id = v.to_string_lossy();
            if this.manager.registry.get(&id).is_some() {
                this.selection = Selection::Worktree(id.into_owned());
            }
            this.open_tab(TabKind::Editor, window, cx);
            log::info!(
                "HARNESS_OPEN_EDITOR: tabs now = {:?}",
                this.panes.values().next().map(|p| p.tabs.len())
            );
        }
        this.recompute_diff(cx);

        // Notification "View" clicks: the platform delegate pushes worktree
        // ids; select them and front the app. Foreground actions activate the
        // app at the OS level already — cx.activate makes the dev-fallback
        // path behave the same.
        let nav_rx = crate::notifier::nav_rx();
        cx.spawn_in(window, async move |view, cx| loop {
            match nav_rx.try_recv() {
                Ok(id) => {
                    let ok = view.update_in(cx, |ws, window, cx| {
                        ws.select(Selection::Worktree(id.clone()), window, cx);
                        cx.activate(true);
                    });
                    if ok.is_err() {
                        break;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        })
        .detach();

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
        // The cache holds the fidelity of whatever adapter this session
        // spawned; signals for worktrees whose tab was never opened (app
        // restart, hooks left by an earlier session) fall back to the KIND's
        // declared fidelity — defaulting to TerminalOnly here silently
        // swallowed every notification after a restart.
        let fidelity = self
            .agent_fidelity
            .get(&signal.worktree_root)
            .copied()
            .unwrap_or_else(|| agent_runtime::fidelity_for(&signal.kind));
        if previous != Some(signal.state) && signal.should_notify(fidelity) {
            self.notify_needs_input(&signal, cx);
        }
        // "Finished — your turn" is a different claim with a different bar:
        // best-effort adapters may make it (it is exactly what turn-complete
        // means), and it only fires on the Running → Idle edge. This is the
        // notification the user reaches for when they leave a worktree tab
        // and ask an agent to work: silence here made the app feel deaf.
        if signal.should_notify_finished(previous, fidelity) {
            self.notify_finished(&signal, cx);
        }
        cx.notify();
    }

    /// Wake the conductor: show the pet card with the message, and keep its
    /// arms moving until it expires or is closed. A fresh notification while
    /// visible just replaces the message and extends the performance.
    fn show_pet(&mut self, message: String, cx: &mut Context<Self>) {
        if !self.prefs.pet_enabled {
            return;
        }
        let until = std::time::Instant::now() + Duration::from_secs(8);
        let animating = self.pet.is_some();
        self.pet = Some((message, until));
        cx.notify();
        if animating {
            return; // the running loop picks up the new expiry
        }
        cx.spawn(async move |view, cx| loop {
            cx.background_executor().timer(Duration::from_millis(350)).await;
            let done = view.update(cx, |ws, cx| {
                match &ws.pet {
                    Some((_, until)) if *until > std::time::Instant::now() => {
                        ws.pet_pose = !ws.pet_pose;
                        cx.notify();
                        false
                    }
                    _ => {
                        ws.pet = None;
                        cx.notify();
                        true
                    }
                }
            });
            if done.unwrap_or(true) {
                break;
            }
        })
        .detach();
    }

    /// Post a native "needs input" notification, unless the user is already
    /// looking at that worktree — telling someone about the prompt they are
    /// staring at is pure noise (§4.6).
    fn notify_needs_input(&mut self, signal: &AgentSignal, cx: &mut Context<Self>) {
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
        let id = self.worktree_id_for_path(&signal.worktree_root).unwrap_or_default();
        self.show_pet(
            format!("{} needs your attention!", agent_display(&signal.kind)),
            cx,
        );
        crate::notifier::notify_agent(
            agent_display(&signal.kind),
            &crate::notifier::label_for(&name, &signal.worktree_root),
            &repo,
            &id,
            crate::notifier::AgentAlert::NeedsAttention,
        );
    }

    /// Post a "finished" notification, with the same view-suppression rule
    /// as needs-input: never narrate the screen the user is looking at.
    fn notify_finished(&mut self, signal: &AgentSignal, cx: &mut Context<Self>) {
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
        let id = self.worktree_id_for_path(&signal.worktree_root).unwrap_or_default();
        self.show_pet(
            format!("{} finished — your turn", agent_display(&signal.kind)),
            cx,
        );
        crate::notifier::notify_agent(
            agent_display(&signal.kind),
            &crate::notifier::label_for(&name, &signal.worktree_root),
            &repo,
            &id,
            crate::notifier::AgentAlert::Finished,
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

        // A worktree opens with a single PLAIN terminal. The agent is a thing
        // you start, not a thing that starts itself: auto-spawning it burned a
        // pi session on every worktree you merely glanced at, and left no way
        // to just get a shell. Add an agent tab (or type `pi`) when you want one.
        // (`open_tab` creates the pane itself now.)
        self.open_tab(TabKind::Shell, window, cx);
        self.evict_stale_panes(&r.key);
    }

    /// Prepare the worktree's agent adapter and return the command to run.
    ///
    /// Separate from tab creation because it also wires up the status signal
    /// and the local git excludes, which are per-worktree, not per-tab.
    /// Prepare an adapter and command for a *specific* agent kind — the ＋
    /// menu offers all enabled agents, not only the worktree's default.
    ///
    /// A user command override replaces the launch command but NOT the
    /// adapter wiring: hooks, excludes, and the status watch still belong to
    /// the kind, or an overridden Claude Code would silently lose its status
    /// pill and notifications.
    fn agent_command_for(
        &mut self,
        kind: &AgentKind,
        r: &Resolved,
    ) -> Option<agent_runtime::AgentCommand> {
        let runtime = runtime_for(kind, &r.path, &self.state_dir);
        if let Err(e) = runtime.on_spawn(&r.path) {
            log::warn!("{kind} setup failed for {}: {e}", r.path.display());
        }
        // The harness runs inside the user's own repository, so anything it
        // drops there would otherwise show up as uncommitted changes in their
        // project. Exclude locally rather than touching their .gitignore.
        if let Err(e) = self
            .manager
            .git()
            .exclude_locally(&r.path, runtime.ignored_paths())
        {
            log::warn!("could not add local git excludes for {}: {e}", r.path.display());
        }
        self.agent_fidelity.insert(r.path.clone(), runtime.fidelity());
        // Adapters that stream their own signals (OpenCode's SSE) publish
        // straight onto the shared bus; file-based ones are served by
        // StatusFileSource.
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
        if let Some(cmd) = self
            .prefs
            .agents
            .get(kind.as_str())
            .and_then(|p| p.command.clone())
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            return Some(agent_runtime::AgentCommand {
                program: shell.into(),
                args: vec!["-lc".into(), cmd],
                env: Vec::new(),
                label: kind.as_str().to_string(),
            });
        }
        let command = runtime.command(&r.path);
        if command.is_none() {
            log::info!("{kind} not on PATH — falling back to a plain shell");
        }
        command
    }

    /// Open a new tab in the active worktree's set and focus it.
    fn open_tab(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        // `main` is a read-only checkout with no agent (D8).
        // D8 revised (July 22, 2026): agents may run on main too — it is the
        // user's repo and their call. Worktrees remain the *recommended*
        // isolation, and the chat pane stays worktree-only (it is pi's RPC
        // surface, wired to a worktree session).
        if matches!(kind, TabKind::Chat) && r.agent_kind.is_none() {
            self.note = "agent chat needs a worktree".to_string();
            cx.notify();
            return;
        }

        let (content, title) = match kind {
            TabKind::Shell => (
                TabContent::Terminal(
                    cx.new(|cx| TerminalPane::new(r.path.clone(), None, window, cx)),
                ),
                "❯_ shell".to_string(),
            ),
            TabKind::Agent(ref agent) => {
                let command = self.agent_command_for(agent, &r);
                let label = command
                    .as_ref()
                    .map(|c| c.label.clone())
                    .unwrap_or_else(|| "shell".to_string());
                (
                    TabContent::Terminal(
                        cx.new(|cx| TerminalPane::new(r.path.clone(), command, window, cx)),
                    ),
                    label,
                )
            }
            TabKind::Editor => match crate::workspace_host::workspace_for(&r.path, window, cx) {
                Ok(hosted) => {
                    crate::workspace_host::probe(&hosted, window, cx);
                    // Our render tree hosts the modal overlay, so our render
                    // must re-run when the workspace's modal state changes —
                    // a child view's notify does not re-render the parent.
                    cx.observe(&hosted, |_, _, cx| cx.notify()).detach();
                    (TabContent::Editor(hosted), "{ } editor".to_string())
                }
                Err(e) => {
                    // Never fatal: the other tabs still work.
                    log::warn!("could not open project for {}: {e}", r.path.display());
                    self.note = format!("could not open project: {e}");
                    cx.notify();
                    return;
                }
            },
            TabKind::Chat => {
                let path = r.path.clone();
                (
                    TabContent::Chat(cx.new(|cx| crate::chat_pane::ChatPane::new(path, cx))),
                    "◆ chat".to_string(),
                )
            }
        };

        let agent_of_kind = match &kind {
            TabKind::Agent(a) => Some(a.clone()),
            _ => None,
        };
        // Create the pane if it does not exist yet. This was `if let Some`,
        // and the else case silently dropped the tab — including a fully
        // constructed zed Workspace, whose own diagnostics all read healthy
        // because the entity *was* healthy right up until it was released.
        // Every state probe passed while the screen showed nothing; a tab
        // that cannot be attached must never be discarded without a trace.
        self.pane_clock = self.pane_clock.wrapping_add(1);
        let pane = self.panes.entry(r.key.clone()).or_insert_with(|| PaneSet {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            last_used: 0,
        });
        pane.last_used = self.pane_clock;
        let id = pane.next_id;
        pane.next_id += 1;
        pane.tabs.push(Tab { id, title, content, agent: agent_of_kind });
        pane.active = id;
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Close a tab, dropping whatever it held — a terminal's pty and its
    /// process, or a chat's `pi --mode rpc` sidecar.
    fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let Some(pane) = self.panes.get_mut(&r.key) else { return };
        let Some(ix) = pane.tabs.iter().position(|t| t.id == id) else { return };
        pane.tabs.remove(ix);
        if pane.active == id {
            // Prefer the tab to the left, which is where the eye already is.
            pane.active = pane
                .tabs
                .get(ix.saturating_sub(1))
                .or_else(|| pane.tabs.last())
                .map(|t| t.id)
                .unwrap_or(0);
        }
        // A worktree with no tabs is a dead end; give it a shell back.
        if pane.tabs.is_empty() {
            self.open_tab(TabKind::Shell, window, cx);
            return;
        }
        self.focus_active_pane(window, cx);
        cx.notify();
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
            let Some(tab) = p.active_tab() else { return };
            let handle = match &tab.content {
                TabContent::Terminal(t) => t.read(cx).focus_handle(cx),
                TabContent::Editor(ws) => ws.read(cx).focus_handle(cx),
                TabContent::Chat(c) => c.read(cx).focus_handle(cx),
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
    /// Copy the selected diff rows to the clipboard.
    ///
    /// Copies the *code*, not the diff decoration: no gutter numbers and no
    /// +/- markers, so a pasted selection compiles.
    fn copy_diff_selection(&mut self, cx: &mut Context<Self>) {
        let Some((file, from, to)) = self.diff_selection.clone() else { return };
        let rows = readable_diff_lines(&self.unified_for_file(&file));
        let text: Vec<String> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i >= from && *i <= to)
            .filter(|(_, r)| r.number.is_some())
            .map(|(_, r)| r.text.clone())
            .collect();
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.join("\n")));
        self.note = format!("copied {} line(s)", text.len());
        cx.notify();
    }

    /// Begin, extend, or clear a row selection within one file's diff.
    fn select_diff_row(&mut self, file: &str, row: usize, extend: bool, cx: &mut Context<Self>) {
        self.diff_selection = match (self.diff_selection.take(), extend) {
            // Shift-click extends, but only within the same file — a range
            // spanning two bodies would have no meaning.
            (Some((f, from, _)), true) if f == file => {
                Some((f, from.min(row), from.max(row)))
            }
            _ => Some((file.to_string(), row, row)),
        };
        cx.notify();
    }

    fn extend_diff_selection(&mut self, file: &str, row: usize, cx: &mut Context<Self>) {
        if let Some((f, from, _)) = self.diff_selection.clone()
            && f == file
        {
            self.diff_selection = Some((f, from.min(row), from.max(row)));
            cx.notify();
        }
    }

    /// Open the comment editor for a diff line, seeded with any existing note.
    fn begin_comment(
        &mut self,
        file: String,
        line: u32,
        at: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self
            .resolve()
            .and_then(|r| self.review.get(&r.key, &file, line))
            .map(|c| c.body.clone())
            .unwrap_or_default();
        self.comment_draft = Some(CommentDraft {
            file,
            line,
            body: existing,
            at,
        });
        window.focus(&self.edit_focus, cx);
        cx.notify();
    }

    fn comment_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                self.comment_draft = None;
                cx.notify();
            }
            "enter" => self.commit_comment(cx),
            "backspace" => {
                if let Some(draft) = self.comment_draft.as_mut() {
                    draft.body.pop();
                }
                cx.notify();
            }
            _ => {
                let m = &event.keystroke.modifiers;
                if let Some(ch) = event.keystroke.key_char.clone()
                    && !ch.is_empty()
                    && !m.control
                    && !m.platform
                    && let Some(draft) = self.comment_draft.as_mut()
                {
                    draft.body.push_str(&ch);
                    cx.notify();
                }
            }
        }
        cx.stop_propagation();
    }

    fn commit_comment(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.comment_draft.take() else { return };
        if let Some(r) = self.resolve() {
            self.review.set(&r.key, &draft.file, draft.line, &draft.body);
            self.review.save(&self.review_path);
        }
        cx.notify();
    }

    /// Hand the review to the agent: copy it formatted, addressed by file and
    /// line, ready to paste into the agent's terminal or chat.
    fn copy_review(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let review = self.review.as_review(&r.key);
        if review.is_empty() {
            self.note = "no review comments yet".to_string();
        } else {
            let n = self.review.count(&r.key);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(review));
            self.note = format!("copied {n} comment(s) — paste into the agent");
        }
        cx.notify();
    }

    /// Write the review to a markdown file next to the harness state.
    ///
    /// Not into the repo: a review is the user's working notes, and dropping an
    /// untracked file into their project is the thing the git-exclude work went
    /// out of its way to avoid.
    fn save_review_markdown(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let md = self.review.as_markdown(&r.key, &r.branch);
        if md.is_empty() {
            self.note = "no comments to save".to_string();
            cx.notify();
            return;
        }
        let file = crate::diff_review::ReviewNotes::markdown_path_in(&self.state_dir, &r.key);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&file, md) {
            Ok(()) => self.note = format!("saved {}", file.display()),
            Err(e) => self.note = format!("could not save review: {e}"),
        }
        cx.notify();
    }

    /// Discard every comment for this worktree, behind a two-click confirm.
    fn clear_review(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        if !self.clearing_review {
            self.clearing_review = true;
            self.note = "click again to discard all comments".to_string();
        } else {
            self.review.clear(&r.key);
            self.review.save(&self.review_path);
            self.clearing_review = false;
            self.note = "comments discarded".to_string();
        }
        cx.notify();
    }

    /// Toggle between the raw diff and a rendered preview of the selected
    /// markdown file.
    fn toggle_md_preview(&mut self, file: &str, cx: &mut Context<Self>) {
        if self.md_preview.as_ref().is_some_and(|(f, _)| f == file) {
            self.md_preview = None;
            cx.notify();
            return;
        }
        let Some(r) = self.resolve() else { return };
        let abs = r.path.join(file);
        self.md_preview = Some((
            file.to_string(),
            cx.new(|cx| crate::markdown_preview::MarkdownPreview::new(abs, cx)),
        ));
        cx.notify();
    }

    fn recompute_diff(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        self.md_preview = None;
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
            Edit::NewName(t) | Edit::Rename(_, t) | Edit::Avatar(_, t) | Edit::AgentCmd(_, t) => {
                Some(t)
            }
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
            Edit::AgentCmd(kind, text) => {
                let cmd = text.trim().to_string();
                let entry = self.prefs.agents.entry(kind).or_default();
                entry.command = if cmd.is_empty() { None } else { Some(cmd) };
                save_prefs_to(&self.prefs_path, &self.prefs);
                cx.notify();
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
            // OpenCode is the default (§4.5). It has the best signal of any
            // adapter we have: `permission.updated` over its local SSE stream
            // is a real, out-of-band "blocked on you" event, so its fidelity is
            // Exact and it may raise notifications — where pi's is BestEffort,
            // approximated from a status file an extension has to write. Less
            // machinery and a truer answer.
            //
            // pi stays a first-class peer rather than the default because what
            // it uniquely offers is the training loop (§4.4), not the signal.
            // Choosing per worktree is the natural next step in the create
            // dialog; until then this is the one that works with no setup.
            agent_kind: AgentKind::OpenCode,
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

    /// Swap the palette and regenerate the zed theme from it, so editor,
    /// terminal, and the hosted workspace follow the chrome (§3.5, D7).
    /// A draggable splitter. `target` says which dimension it drives.
    ///
    /// 6px wide with a 1px visible line: narrower than this and it is fiddly to
    /// grab, wider and it reads as a gap in the chrome.
    fn splitter(&self, hx: &HarnessTheme, target: Drag, cx: &mut Context<Self>) -> impl IntoElement {
        // Both remaining splitters are vertical dividers between columns.
        let vertical = true;
        let active = self.drag.map(|d| d.target) == Some(target);
        let start_value = match target {
            Drag::Sidebar => self.prefs.sidebar_width,
            Drag::DiffPanel => self.prefs.diff_width,
        };

        let base = div()
            .id(match target {
                Drag::Sidebar => "split-sidebar",
                Drag::DiffPanel => "split-diff",
            })
            .flex_none()
            .cursor(if vertical {
                gpui::CursorStyle::ResizeLeftRight
            } else {
                gpui::CursorStyle::ResizeUpDown
            })
            .when(active, |d| d.bg(hx.accent))
            .when(!active, |d| d.hover(|h| h.bg(hx.accent)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    ws.drag = Some(DragState {
                        target,
                        start: if vertical { ev.position.x } else { ev.position.y },
                        start_value,
                    });
                    cx.notify();
                }),
            );

        if vertical {
            base.w(px(6.0)).h_full()
        } else {
            base.h(px(6.0)).w_full()
        }
    }

    /// Apply an in-progress drag. Called from the root element so the pointer
    /// can leave the 6px handle without dropping the drag.
    fn drag_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        // The button can be released outside the window, in which case no
        // mouse-up ever reaches us; treat "moving with no button" as the end.
        if ev.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let (pos, sign) = match drag.target {
            Drag::Sidebar => (ev.position.x, 1.0),
            // The diff panel is right-anchored, so dragging left widens it.
            Drag::DiffPanel => (ev.position.x, -1.0),
        };
        let delta = f32::from(pos - drag.start) * sign;
        let next = drag.start_value + delta;
        match drag.target {
            Drag::Sidebar => self.prefs.sidebar_width = next,
            Drag::DiffPanel => self.prefs.diff_width = next,
        }
        // Clamp every frame, not on release: a panel must never be draggable
        // to zero width and become unrecoverable.
        self.prefs = self.prefs.clone().sanitized();
        cx.notify();
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            save_prefs_to(&self.prefs_path, &self.prefs);
            cx.notify();
        }
    }

    fn save_prefs(&mut self, cx: &App) {
        self.prefs.dark = theme(cx).is_dark();
        save_prefs_to(&self.prefs_path, &self.prefs);
    }

    /// Re-install the whole keymap and menus from the catalog + overrides.
    /// Clear-then-bind, not bind-on-top: gpui breaks precedence ties by
    /// recency, so stale chords would keep firing their old actions.
    fn apply_shortcuts(&self, cx: &mut Context<Self>) {
        cx.clear_key_bindings();
        crate::install_keymap(&self.prefs.shortcuts, cx);
        crate::install_menus(cx);
    }

    /// One captured chord while a shortcut row is armed.
    fn rebind_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.rebinding.clone() else { return };
        let ks = &event.keystroke;
        if ks.key == "escape" && !ks.modifiers.modified() {
            self.rebinding = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // A bare modifier press is the chord being formed, not the chord.
        if ["shift", "ctrl", "alt", "cmd", "platform", "function"].contains(&ks.key.as_str()) {
            cx.stop_propagation();
            return;
        }
        let mut chord = String::new();
        if ks.modifiers.control {
            chord.push_str("ctrl-");
        }
        if ks.modifiers.alt {
            chord.push_str("alt-");
        }
        if ks.modifiers.shift {
            chord.push_str("shift-");
        }
        if ks.modifiers.platform {
            chord.push_str("cmd-");
        }
        chord.push_str(&ks.key);
        self.prefs.shortcuts.insert(id, chord);
        save_prefs_to(&self.prefs_path, &self.prefs);
        self.rebinding = None;
        self.apply_shortcuts(cx);
        cx.notify();
        cx.stop_propagation();
    }

    /// Apply a zed theme by registry name (`None` = the harness pair).
    fn set_zed_theme(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.prefs.zed_theme = name;
        self.apply_appearance(cx);
        self.save_prefs(cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// Set a font override. `mono` covers the buffer, the terminal, and the
    /// chrome's monospace; `!mono` is the interface font.
    fn set_font(&mut self, mono: bool, name: String, cx: &mut Context<Self>) {
        if mono {
            self.prefs.font_mono = Some(name);
        } else {
            self.prefs.font_ui = Some(name);
        }
        self.apply_appearance(cx);
        self.save_prefs(cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// Re-derive every appearance global from prefs: theme (harness pair or a
    /// zed registry theme), chrome tokens, fonts, and the zed settings JSON.
    /// One function on purpose — called at startup and after every settings
    /// change, so "restored" and "just picked" cannot diverge.
    fn apply_appearance(&mut self, cx: &mut Context<Self>) {
        // Fonts first: leaked to &'static str, matching HarnessTheme's fields
        // and theme.rs's own resolve_font. Bounded by the number of font
        // changes in a session.
        let cur = theme(cx).clone();
        let font_ui: &'static str = match &self.prefs.font_ui {
            Some(name) => Box::leak(name.clone().into_boxed_str()),
            None => cur.font_ui,
        };
        let font_mono: &'static str = match &self.prefs.font_mono {
            Some(name) => Box::leak(name.clone().into_boxed_str()),
            None => cur.font_mono,
        };

        let zed_theme = self
            .prefs
            .zed_theme
            .as_ref()
            .and_then(|name| ::theme::ThemeRegistry::global(cx).get(name).ok());
        match zed_theme {
            Some(zed) => {
                ::theme::GlobalTheme::update_theme(cx, zed.clone());
                // Parsing is not coloring (see harness_zed_theme::install):
                // the registry's HighlightMaps rebuild from the new theme.
                ::workspace::AppState::global(cx).languages.set_theme(zed.clone());
                let hx = crate::harness_zed_theme::harness_from_zed(&zed, font_ui, font_mono);
                cx.set_global(hx);
            }
            None => {
                let mut hx = if self.prefs.dark {
                    HarnessTheme::dark(cx)
                } else {
                    HarnessTheme::light(cx)
                };
                hx.font_ui = font_ui;
                hx.font_mono = font_mono;
                crate::harness_zed_theme::install(&hx, cx);
                cx.set_global(hx);
            }
        }
        crate::apply_zed_settings(cx, font_ui, font_mono, self.prefs.font_size.unwrap_or(13.0));
    }

    fn render_top_bar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(
                div()
                    .id("toggle-sidebar")
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .text_color(if self.prefs.sidebar_open { hx.tx1 } else { hx.tx2 })
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .child("▤ worktrees")
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.prefs.sidebar_open = !ws.prefs.sidebar_open;
                        ws.save_prefs(cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("toggle-diff")
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .text_color(if self.prefs.diff_open { hx.tx1 } else { hx.tx2 })
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .child("diff ▤")
                    .on_click(cx.listener(|ws, _, _, cx| {
                        ws.prefs.diff_open = !ws.prefs.diff_open;
                        ws.save_prefs(cx);
                        cx.notify();
                    })),
            )
    }

    /// The conductor pet: a small maestro who appears with each notification
    /// and conducts until dismissed or done. Divs only — head, tapering robe,
    /// and two arms whose geometry flips between beats, plus alternating
    /// notes. Clippy walked so this little one could wave.
    fn render_pet(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (message, _) = self.pet.clone()?;
        let up = self.pet_pose;
        let arm = |left: bool| {
            let d = div().absolute().rounded_full().bg(hx.accent);
            match (left, up) {
                // Beat up: arms vertical, raised high.
                (true, true) => d.w(px(7.0)).h(px(26.0)).top(px(18.0)).left(px(14.0)),
                (false, true) => d.w(px(7.0)).h(px(26.0)).top(px(18.0)).left(px(67.0)),
                // Beat down: arms swing out sideways.
                (true, false) => d.w(px(24.0)).h(px(7.0)).top(px(40.0)).left(px(2.0)),
                (false, false) => d.w(px(24.0)).h(px(7.0)).top(px(40.0)).left(px(62.0)),
            }
        };
        Some(
            div()
                .absolute()
                .bottom(px(52.0))
                .right(px(16.0))
                .w(px(232.0))
                .rounded_lg()
                .bg(hx.surface2)
                .border_1()
                .border_color(hx.border2)
                .shadow_lg()
                .p_2()
                .flex()
                .gap_2()
                .items_center()
                .child(
                    // The stage: 88×72, everything positioned in it.
                    div()
                        .relative()
                        .flex_none()
                        .w(px(88.0))
                        .h(px(72.0))
                        .child(
                            div()
                                .absolute()
                                .top(px(4.0))
                                .left(px(33.0))
                                .w(px(22.0))
                                .h(px(22.0))
                                .rounded_full()
                                .bg(hx.tx0),
                        )
                        .child(
                            // The robe: wide-bottomed via border radius.
                            div()
                                .absolute()
                                .top(px(28.0))
                                .left(px(26.0))
                                .w(px(36.0))
                                .h(px(40.0))
                                .rounded_t_full()
                                .rounded_b_md()
                                .bg(hx.accent),
                        )
                        .child(arm(true))
                        .child(arm(false))
                        .child(
                            // The baton, only visible on the upbeat.
                            div()
                                .absolute()
                                .w(px(3.0))
                                .h(px(16.0))
                                .rounded_full()
                                .bg(hx.tx0)
                                .top(if up { px(2.0) } else { px(36.0) })
                                .left(if up { px(16.0) } else { px(-4.0) }),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(if up { px(0.0) } else { px(10.0) })
                                .right(px(2.0))
                                .text_sm()
                                .text_color(hx.amber)
                                .child(if up { "♪" } else { "♫" }),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(hx.tx0)
                        .child(message),
                )
                .child(
                    div()
                        .id("pet-close")
                        .flex_none()
                        .px_1()
                        .rounded_md()
                        .text_xs()
                        .text_color(hx.tx2)
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                        .child("✕")
                        .on_click(cx.listener(|ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.pet = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// zed's modal layer (file finder, pickers) rendered as OUR window-root
    /// child — the third window-root duty this embed adopts (after action
    /// wiring and key context). At the root rather than inside the editor
    /// tab's wrapper: an overlay hosted in the wrapper centers over the tab
    /// content area, which sits visibly right of the window's center the
    /// title bar establishes. zed centers modals over the window; so do we.
    fn render_zed_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let r = self.resolve()?;
        let pane = self.panes.get(&r.key)?;
        let ws = match &pane.active_tab()?.content {
            TabContent::Editor(ws) => ws.clone(),
            _ => return None,
        };
        let view: gpui::AnyView = ws
            .read(cx)
            .active_modal::<file_finder::FileFinder>(cx)?
            .into();
        let ws_for_dismiss = ws.clone();
        Some(
            div()
                .absolute()
                .inset_0()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |_, _, window, cx| {
                        ws_for_dismiss.update(cx, |ws, cx| {
                            ws.hide_modal(window, cx);
                        });
                    }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .h(px(0.0))
                        .mt_20()
                        .items_center()
                        .child(
                            div()
                                .occlude()
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .child(view),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The settings screen: an overlay, not a tab — settings are a glance,
    /// not a workspace. v1 is the shortcut catalog; the section structure is
    /// where future settings (theme, agent defaults) slot in.
    ///
    /// Rendered from `shortcuts::entries()` — the same table `main.rs` binds
    /// from — so this screen structurally cannot claim a shortcut that is not
    /// actually bound.
    fn render_settings(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = div()
            .id("settings-card")
            .flex()
            .flex_col()
            .w(px(520.0))
            .max_h(px(600.0))
            .rounded_lg()
            .bg(hx.bg1)
            .border_1()
            .border_color(hx.border)
            .shadow_lg()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(hx.tx0)
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .id("settings-close")
                            .px_2()
                            .rounded_md()
                            .text_sm()
                            .text_color(hx.tx2)
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                            .child("✕")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                ws.settings_open = false;
                                cx.notify();
                            })),
                    ),
            );

        let mut body = div().id("settings-body").flex_1().min_h_0().overflow_y_scroll().px_4().py_2();

        // ---- Appearance: mode first, then themes of that appearance -------
        let section_title = |label: &str| {
            div()
                .pt_3()
                .pb_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx2)
                .child(label.to_uppercase())
        };
        let chip = |label: String, selected: bool, hx: &HarnessTheme| {
            div()
                .px_2()
                .py_0p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .border_1()
                .border_color(if selected { hx.accent } else { hx.border })
                .bg(if selected { tint(hx.accent, 0.16) } else { hx.surface })
                .text_color(if selected { hx.accent } else { hx.tx1 })
                .hover(|d| d.bg(hx.surface_hover))
                .child(label)
        };

        // Mode decides which theme family the list below offers. Switching
        // modes returns to the harness pair — a dark zed theme in light mode
        // would contradict the choice just made.
        let mut modes = div().flex().gap_1p5().pb_1();
        for (label, dark) in [("Dark", true), ("Light", false)] {
            let selected = self.prefs.dark == dark;
            modes = modes.child(
                div()
                    .id(gpui::SharedString::from(format!("mode-{label}")))
                    .child(chip(label.to_string(), selected, hx))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.prefs.dark = dark;
                        ws.set_zed_theme(None, cx);
                    })),
            );
        }
        body = body.child(section_title("Mode")).child(modes);

        // Themes for the chosen mode: the harness palette plus every bundled
        // zed theme of the same appearance. The registry ALSO holds our own
        // generated theme under the harness name — excluded, or the list
        // shows a phantom duplicate that fights the real chip for clicks
        // (the "can't select Harness Dark" bug).
        let mut themes = div().flex().flex_wrap().gap_1p5().pb_1();
        let active_zed = self.prefs.zed_theme.clone();
        let want_dark = self.prefs.dark;
        let harness_label =
            if want_dark { "Harness Dark".to_string() } else { "Harness Light".to_string() };
        let mut options: Vec<(String, Option<String>)> = vec![(harness_label, None)];
        for meta in ::theme::ThemeRegistry::global(cx).list() {
            let name = meta.name.to_string();
            if name == crate::harness_zed_theme::HARNESS_THEME_NAME {
                continue;
            }
            let is_dark = matches!(meta.appearance, ::theme::Appearance::Dark);
            if is_dark == want_dark {
                options.push((name.clone(), Some(name)));
            }
        }
        for (label, value) in options {
            let selected = match (&active_zed, &value) {
                (Some(a), Some(v)) => a == v,
                (None, None) => true,
                _ => false,
            };
            let value2 = value.clone();
            themes = themes.child(
                div()
                    .id(gpui::SharedString::from(format!("theme-{label}")))
                    .child(chip(label.clone(), selected, hx))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.set_zed_theme(value2.clone(), cx);
                    })),
            );
        }
        body = body.child(section_title("Theme")).child(themes);

        // ---- Fonts: curated candidates that are actually installed --------
        let installed = cx.text_system().all_font_names();
        let has = |name: &str| installed.iter().any(|f| f == name);
        for (title, mono) in [("Code font", true), ("Interface font", false)] {
            let candidates: &[&str] = if mono {
                &["Menlo", "Monaco", "SF Mono", "Courier New", "JetBrains Mono",
                  "Fira Code", "Source Code Pro", "IBM Plex Mono", "Zed Plex Mono", "Hack"]
            } else {
                &["Zed Plex Sans", "Helvetica Neue", "Arial", "Inter",
                  "IBM Plex Sans", "Avenir Next", "Seravek", "Verdana"]
            };
            let current = if mono {
                self.prefs.font_mono.clone().unwrap_or_else(|| hx.font_mono.to_string())
            } else {
                self.prefs.font_ui.clone().unwrap_or_else(|| hx.font_ui.to_string())
            };
            let mut row = div().flex().flex_wrap().gap_1p5().pb_1();
            for name in candidates.iter().filter(|n| has(n)) {
                let name = name.to_string();
                let selected = name == current;
                let pick = name.clone();
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("font-{title}-{name}")))
                        .child(chip(name, selected, hx))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.set_font(mono, pick.clone(), cx);
                        })),
                );
            }
            body = body.child(section_title(title)).child(row);
        }
        {
            let size = self.prefs.font_size.unwrap_or(13.0);
            let step = |delta: f32| {
                move |ws: &mut Self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                    let cur = ws.prefs.font_size.unwrap_or(13.0);
                    let next = (cur + delta).clamp(9.0, 24.0);
                    ws.prefs.font_size = if next == 13.0 { None } else { Some(next) };
                    ws.apply_appearance(cx);
                    ws.save_prefs(cx);
                    cx.refresh_windows();
                    cx.notify();
                }
            };
            let size_chip = |label: &str| {
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(hx.border)
                    .bg(hx.surface)
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx0)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .child(label.to_string())
            };
            body = body.child(section_title("Font size")).child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .pb_1()
                    .child(div().id("fs-minus").child(size_chip("−")).on_click(cx.listener(step(-1.0))))
                    .child(
                        div()
                            .w(px(44.0))
                            .text_center()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx0)
                            .child(format!("{size:.0} px")),
                    )
                    .child(div().id("fs-plus").child(size_chip("+")).on_click(cx.listener(step(1.0)))),
            );
        }

        // ---- Extras --------------------------------------------------------
        {
            let enabled = self.prefs.pet_enabled;
            body = body.child(section_title("Extras")).child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_1()
                    .child(
                        div()
                            .id("pet-toggle")
                            .child(chip(
                                if enabled { "on".into() } else { "off".into() },
                                enabled,
                                hx,
                            ))
                            .on_click(cx.listener(|ws, _, _, cx| {
                                cx.stop_propagation();
                                ws.prefs.pet_enabled = !ws.prefs.pet_enabled;
                                if !ws.prefs.pet_enabled {
                                    ws.pet = None;
                                }
                                save_prefs_to(&ws.prefs_path, &ws.prefs);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(hx.tx1)
                            .child("Conductor pet on notifications"),
                    ),
            );
        }

        // ---- Agents: enable/disable + launch-command override -------------
        let mut agents = div().flex().flex_col().gap_1().pb_1();
        for kind in AgentKind::known() {
            let key = kind.as_str().to_string();
            let pref = self.prefs.agents.get(&key).cloned().unwrap_or_default();
            let (glyph, color) = agent_mark(&kind);
            // The binary each adapter actually launches; claude-code's is
            // `claude`, everyone else matches their kind string.
            let default_cmd = match kind {
                AgentKind::ClaudeCode => "claude",
                _ => kind.as_str(),
            };
            let editing = matches!(&self.edit, Edit::AgentCmd(k, _) if *k == key);

            let toggle_key = key.clone();
            let edit_key_ = key.clone();
            // Prefill what the user SEES — the override if set, else the
            // default — so editing refines the current command instead of
            // starting from a blank line.
            let prefill = pref.command.clone().unwrap_or_else(|| default_cmd.to_string());
            let cmd_display = pref.command.clone().unwrap_or_else(|| default_cmd.to_string());
            let edit_text = match &self.edit {
                Edit::AgentCmd(k, t) if *k == key => t.clone(),
                _ => String::new(),
            };

            let mut row = div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(
                    div()
                        .w(px(18.0))
                        .text_center()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(color)
                        .child(glyph),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(110.0))
                        .text_sm()
                        .text_color(if pref.disabled { hx.tx2 } else { hx.tx0 })
                        .child(kind.to_string()),
                )
                .child(
                    div()
                        .id(gpui::SharedString::from(format!("agent-en-{key}")))
                        .child(chip(
                            if pref.disabled { "off".into() } else { "on".into() },
                            !pref.disabled,
                            hx,
                        ))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            let e = ws.prefs.agents.entry(toggle_key.clone()).or_default();
                            e.disabled = !e.disabled;
                            save_prefs_to(&ws.prefs_path, &ws.prefs);
                            cx.notify();
                        })),
                );
            if editing {
                row = row.child(div().flex_1().child(self.render_input(hx, &edit_text, cx)));
            } else {
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("agent-cmd-{key}")))
                        .flex_1()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(hx.surface)
                        .border_1()
                        .border_color(hx.border)
                        .text_xs()
                        .font_family(hx.font_mono)
                        .text_color(if pref.command.is_some() { hx.tx0 } else { hx.tx2 })
                        .cursor_pointer()
                        .hover(|d| d.bg(hx.surface_hover))
                        .child(cmd_display)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.begin_edit(
                                Edit::AgentCmd(edit_key_.clone(), prefill.clone()),
                                window,
                                cx,
                            );
                        })),
                );
            }
            agents = agents.child(row);
        }
        body = body
            .child(section_title("Agents"))
            .child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .pb_1()
                    .child("Click a command to edit it (runs via $SHELL -lc; empty restores the default). Enter saves, Esc cancels."),
            )
            .child(agents);

        // The shortcuts get their own heading now that the screen holds
        // appearance, fonts, and agents above them.
        body = body.child(
            div()
                .mt_2()
                .pt_2()
                .border_t_1()
                .border_color(hx.border)
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx0)
                .child("Keyboard Shortcuts"),
        );
        let entries = crate::shortcuts::entries_with(&self.prefs.shortcuts);
        let scopes = [
            crate::shortcuts::Scope::Global,
            crate::shortcuts::Scope::Workspace,
            crate::shortcuts::Scope::Editor,
            crate::shortcuts::Scope::Picker,
        ];
        for scope in scopes {
            let mut section = div().flex().flex_col().pb_2().child(
                div()
                    .pt_3()
                    .pb_1()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx2)
                    .child(scope.label().to_uppercase()),
            );
            for e in entries.iter().filter(|e| {
                e.scope == scope
                    || (scope == crate::shortcuts::Scope::Editor
                        && e.scope == crate::shortcuts::Scope::EditorFull)
            }) {
                let id = crate::shortcuts::key_id(e.scope, e.what);
                let arming = self.rebinding.as_deref() == Some(id.as_str());
                let overridden = self.prefs.shortcuts.contains_key(&id);
                let arm_id = id.clone();
                let chip_label = if arming {
                    "press keys…".to_string()
                } else {
                    crate::shortcuts::display_keys(&e.keys)
                };
                let mut chipd = div()
                    .id(gpui::SharedString::from(format!("sc-{id}")))
                    .flex_none()
                    .w(px(120.0))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(if arming { tint(hx.accent, 0.16) } else { hx.surface })
                    .border_1()
                    .border_color(if arming {
                        hx.accent
                    } else if overridden {
                        hx.amber
                    } else {
                        hx.border
                    })
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(if arming { hx.accent } else { hx.tx0 })
                    .text_center()
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .child(chip_label)
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.rebinding = Some(arm_id.clone());
                        window.focus(&ws.rebind_focus, cx);
                        cx.notify();
                    }));
                if arming {
                    // The armed chip owns the keyboard until a chord lands
                    // or escape cancels.
                    chipd = chipd
                        .track_focus(&self.rebind_focus)
                        .on_key_down(cx.listener(Self::rebind_key));
                }
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_1()
                        .child(chipd)
                        .child(div().text_sm().text_color(hx.tx1).child(e.what)),
                );
            }
            body = body.child(section);
        }
        // Reset appears only once something is rebound — a button that
        // undoes nothing is clutter.
        if !self.prefs.shortcuts.is_empty() {
            body = body.child(
                div()
                    .id("sc-reset")
                    .mt_2()
                    .mb_3()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(hx.border)
                    .bg(hx.surface)
                    .text_xs()
                    .text_color(hx.tx1)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                    .child("Reset shortcuts to defaults")
                    .on_click(cx.listener(|ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.prefs.shortcuts.clear();
                        save_prefs_to(&ws.prefs_path, &ws.prefs);
                        ws.apply_shortcuts(cx);
                        cx.notify();
                    })),
            );
        }
        card = card.child(body);

        // Full-window backdrop. Swallows mouse-down (the ＋-menu lesson: a
        // backdrop that closes on mouse-down eats the click before any child
        // on_click can fire on mouse-up) and closes on click outside the card.
        div()
            .id("settings-backdrop")
            .when(std::env::var_os("HARNESS_PROBE_PAINT").is_some(), |d| {
                d.child(
                    gpui::canvas(
                        |b, _, _| log::info!("paint: settings overlay bounds = {b:?}"),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // A modal owns the wheel. Scroll events bubble past the card's
            // own scroll region, and anything the backdrop lets through
            // reaches the editor underneath — both were scrolling at once.
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|ws, _, _, cx| {
                ws.settings_open = false;
                cx.notify();
            }))
            .child(card.on_click(|_, _, cx| cx.stop_propagation()))
            .into_any_element()
    }

    /// The sidebar footer: the only way in when nothing is open, so it gets
    /// its own region rather than floating as the last item in a scrolling
    /// list.
    ///
    /// It sits in a padded, top-bordered strip because the sidebar column is
    /// `overflow_hidden` and full height — a button flush against that edge had
    /// its own border clipped off by the container.
    fn add_folder_button(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .p_2()
            .border_t_1()
            .border_color(hx.border)
            .child(
                div()
                    .id("add-folder")
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_1p5()
                    .h(px(30.0))
                    .rounded_md()
                    .bg(hx.surface)
                    .border_1()
                    .border_color(hx.border2)
                    .text_sm()
                    .text_color(hx.tx1)
                    .cursor_pointer()
                    .hover(|d| {
                        d.bg(hx.surface_hover)
                            .border_color(hx.accent)
                            .text_color(hx.tx0)
                    })
                    .on_click(cx.listener(|ws, _, window, cx| ws.add_folder(window, cx)))
                    // Icon and label as separate children so the gap is real
                    // spacing rather than a space character in the glyph run.
                    .child(div().text_color(hx.accent).child("＋"))
                    .child("Add folder"),
            )
    }

    fn render_sidebar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.prefs.sidebar_width))
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
            // Spacer so the footer sits at the bottom here too. Without it the
            // top-bordered strip would float mid-column, reading as a divider
            // across nothing.
            return column
                .child(div().flex_1())
                .child(self.add_folder_button(hx, cx));
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
            // Swallow mouse-DOWN inside the menu. The dismiss backdrop listens
            // on mouse-down, and a click is down-then-up: without this the
            // backdrop closed the menu on the way down and the item's on_click
            // never fired on the way up. That is why ＋ → Editor did nothing.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
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

    fn render_center(&self, hx: &HarnessTheme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if std::env::var_os("HARNESS_PROBE_PAINT").is_some() {
            log::info!(
                "paint: center selection={:?} pane={:?}",
                self.selection,
                self.resolve().map(|r| {
                    let p = self.panes.get(&r.key);
                    (r.key.clone(), p.map(|p| p.tabs.len()), p.and_then(|p| p.active_tab()).map(|t| t.title.clone()))
                }),
            );
        }
        let Some(r) = self.resolve() else {
            return self.render_empty_center(hx, cx).into_any_element();
        };
        let pane = self.panes.get(&r.key);

        let content: AnyElement = match pane.and_then(|p| p.active_tab()) {
            Some(tab) => match &tab.content {
                TabContent::Terminal(t) => t.clone().into_any_element(),
                TabContent::Editor(ws) => {
                    if std::env::var_os("HARNESS_PROBE_PAINT").is_some() {
                        log::info!("paint: center renders Editor tab");
                    }
                    // `Workspace::render` draws panes and docks but attaches
                    // NONE of its action handlers — in zed, the *window root*
                    // (MultiWorkspace) calls `workspace.actions(...)` on its
                    // own wrapper and sets the "Workspace" key context there.
                    // We are that window root here, so we carry the same
                    // duties: without this wrapper every workspace action
                    // (⌘P, ⌘⇧F, dock toggles, ...) dispatches into a tree
                    // with no matching handler and no matching key context,
                    // and dies silently.
                    ws.update(cx, |workspace, cx| workspace.actions(div(), window, cx))
                        .key_context("Workspace")
                        // `.flex()`, because gpui divs default to `display:
                        // Block`, and taffy's block layout does not resolve
                        // child percentage *heights*. zed's own window root
                        // wraps the workspace in `h_flex()` for this reason.
                        // Without it the whole hosted workspace is silently
                        // content-sized: panes look fine (they have intrinsic
                        // content), but the dock caches its panel as a
                        // childless leaf whose content height is zero — an
                        // invisible file tree with healthy state.
                        .flex()
                        .size_full()
                        .child(ws.clone())
                        .when(std::env::var_os("HARNESS_PROBE_PAINT").is_some(), |d| {
                            d.child(
                                gpui::canvas(
                                    |bounds, _, _| {
                                        log::info!("paint: editor slot bounds = {bounds:?}");
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                        })
                        .into_any_element()
                }
                TabContent::Chat(c) => c.clone().into_any_element(),
            },
            None => div().into_any_element(),
        };

        // The tab strip scrolls: a worktree can carry many tabs now, and
        // without this the `+` buttons would be pushed out of reach.
        let mut strip = div()
            .id("tab-strip")
            .flex()
            .flex_none()
            .items_center()
            .h(px(34.0))
            .px_2()
            .gap_1()
            .bg(hx.bg1)
            .border_b_1()
            .border_color(hx.border)
            .overflow_x_scroll();
        if let Some(p) = pane {
            for tab in &p.tabs {
                strip = strip.child(self.render_tab(hx, tab, tab.id == p.active, cx));
            }
        }
        strip = strip.child(self.render_new_tab_button(hx, cx));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .bg(hx.bg0)
            .child(strip)
            .child(div().flex_1().h_full().min_h_0().child(content))
            .child(self.render_status_bar(hx, Some(&r), cx))
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

    /// One tab button, with a close affordance.
    fn render_tab(
        &self,
        hx: &HarnessTheme,
        tab: &Tab,
        active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = tab.id;
        let close_id = tab.id;
        div()
            .id(("tab", tab.id as usize))
            .flex()
            .items_center()
            .gap_1()
            .h(px(26.0))
            .pl_3()
            .pr_1p5()
            .rounded_md()
            .when(active, |d| d.bg(hx.bg0).border_1().border_color(hx.border))
            .text_xs()
            .font_family(hx.font_mono)
            .text_color(if active { hx.tx0 } else { hx.tx2 })
            .when_some(tab.agent.clone(), |d, kind| {
                let (glyph, color) = agent_mark(&kind);
                d.child(
                    div()
                        .mr_1()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(color)
                        .child(glyph),
                )
            })
            .cursor_pointer()
            .hover(|d| d.text_color(hx.tx1))
            .on_click(cx.listener(move |ws, _, window, cx| {
                let Some(r) = ws.resolve() else { return };
                if let Some(pane) = ws.panes.get_mut(&r.key) {
                    pane.active = id;
                }
                ws.focus_active_pane(window, cx);
                cx.notify();
            }))
            .child(tab.title.clone())
            .child(
                div()
                    .id(("tab-close", tab.id as usize))
                    .px_1()
                    .rounded_sm()
                    .text_color(hx.tx2)
                    .hover(|d| d.bg(hx.surface).text_color(hx.red))
                    .child("×")
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.close_tab(close_id, window, cx);
                        // Otherwise the click also selects the tab being closed.
                        cx.stop_propagation();
                    })),
            )
    }

    /// The single `+` that opens the new-tab menu (§3.4).
    ///
    /// One affordance rather than one button per kind: the strip already
    /// carries the worktree's tabs, and four permanent `＋x` buttons competed
    /// with them for both space and attention.
    fn render_new_tab_button(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("new-tab")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(22.0))
            .w(px(24.0))
            .ml_1()
            .rounded_md()
            .text_xs()
            .text_color(hx.tx2)
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .child("＋")
            .on_click(cx.listener(|ws, ev: &gpui::ClickEvent, _, cx| {
                // Anchor under the button that was clicked, so the menu lands
                // wherever the strip has scrolled to.
                ws.tab_menu = Some(ev.position().x);
                cx.notify();
            }))
    }

    /// The inline-comment popover, anchored beside the line it belongs to.
    ///
    /// A popover rather than a panel at the bottom: a comment is about one
    /// line, and reading it fifty rows away makes you hunt for what it means.
    fn render_comment_popover(
        &self,
        hx: &HarnessTheme,
        draft: &CommentDraft,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        const W: f32 = 320.0;
        // Open to the LEFT: the diff lives in the right-hand panel, so an
        // anchor rightwards would push the popover off-window.
        let left = (f32::from(draft.at.x) - W - 14.0).max(8.0);
        let top = (f32::from(draft.at.y) - 12.0).max(48.0);
        let existing = self
            .resolve()
            .and_then(|r| self.review.get(&r.key, &draft.file, draft.line))
            .is_some();
        let file_label = draft
            .file
            .rsplit('/')
            .next()
            .unwrap_or(&draft.file)
            .to_string();

        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(W))
            .rounded_lg()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .flex()
            .flex_col()
            .overflow_hidden()
            // Typing in the popover must not count as "clicked outside".
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            // Header: what this comment is attached to.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .bg(hx.bg1)
                    .border_b_1()
                    .border_color(hx.border)
                    .child(div().text_xs().text_color(hx.amber).child("▸"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx0)
                            .overflow_hidden()
                            .child(file_label),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx2)
                            .child(format!("line {}", draft.line)),
                    ),
            )
            // Body: the text field, sized like a note rather than a one-liner.
            .child(
                div()
                    .id("comment-input")
                    .track_focus(&self.edit_focus)
                    .on_key_down(cx.listener(Self::comment_key))
                    .min_h(px(56.0))
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(if draft.body.is_empty() { hx.tx2 } else { hx.tx0 })
                    .child(if draft.body.is_empty() {
                        "What should the agent change here?".to_string()
                    } else {
                        draft.body.clone()
                    }),
            )
            // Footer: the two things you can do, and how.
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .bg(hx.bg1)
                    .border_t_1()
                    .border_color(hx.border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(hx.tx2)
                            .child(if existing {
                                "clear the text to delete"
                            } else {
                                "goes to the agent with the review"
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .child(div().text_color(hx.tx2).child("esc"))
                            .child(div().text_color(hx.accent).child("⏎ save")),
                    ),
            )
    }

    /// The popup listing what can be opened in this worktree.
    fn render_tab_menu(
        &self,
        hx: &HarnessTheme,
        x: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let agent = self.resolve().and_then(|r| r.agent_kind);
        let mut menu = div()
            .absolute()
            .left(x)
            // Just below the top bar + tab strip.
            .top(px(72.0))
            .w(px(190.0))
            .p(px(4.0))
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .flex()
            .flex_col()
            .gap_1()
            // Swallow mouse-DOWN inside the menu. The dismiss backdrop listens
            // on mouse-down, and a click is down-then-up: without this the
            // backdrop closed the menu on the way down and the item's on_click
            // never fired on the way up. That is why ＋ → Editor did nothing.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(self.tab_menu_item(hx, "❯_  Terminal", "⌘T", TabKind::Shell, cx))
            .child(self.tab_menu_item(hx, "{ }  Editor", "⌘E", TabKind::Editor, cx));
        // Every enabled agent, on any selection (D8 revised: main included) —
        // a worktree's recorded default merely decides which signal the
        // status pill trusts first.
        {
            for kind in AgentKind::known() {
                let disabled = self
                    .prefs
                    .agents
                    .get(kind.as_str())
                    .is_some_and(|p| p.disabled);
                if disabled {
                    continue;
                }
                let (glyph, color) = agent_mark(&kind);
                let label = format!("{glyph}  {kind}");
                menu = menu.child(self.tab_menu_item_colored(
                    hx,
                    &label,
                    "",
                    TabKind::Agent(kind.clone()),
                    color,
                    cx,
                ));
            }
        }
        // The chat tab is pi's RPC surface specifically — approvals as UI,
        // session capture for training (§4.4) — so it appears only for
        // worktrees and only while pi is enabled.
        let pi_enabled = !self
            .prefs
            .agents
            .get(AgentKind::Pi.as_str())
            .is_some_and(|p| p.disabled);
        if agent.is_some() && pi_enabled {
            menu = menu.child(self.tab_menu_item(hx, "π  pi chat", "⌘⇧C", TabKind::Chat, cx));
        }
        menu
    }

    fn tab_menu_item(
        &self,
        hx: &HarnessTheme,
        label: &str,
        shortcut: &str,
        kind: TabKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(gpui::SharedString::from(format!("tabmenu-{label}")))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .text_color(hx.tx1)
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
            .child(div().font_family(hx.font_mono).child(label.to_string()))
            .child(div().text_color(hx.tx2).child(shortcut.to_string()))
            .on_click(cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.tab_menu = None;
                ws.open_tab(kind.clone(), window, cx);
            }))
    }

    /// A tab-menu item whose leading glyph carries an agent's brand color.
    /// Monogram marks, not logos: hand-drawn approximations of trademarks
    /// would be worse than none, and real logo files can slot in later.
    fn tab_menu_item_colored(
        &self,
        hx: &HarnessTheme,
        label: &str,
        shortcut: &str,
        kind: TabKind,
        color: Rgba,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (glyph, rest) = label.split_once("  ").unwrap_or(("", label));
        div()
            .id(gpui::SharedString::from(format!("tabmenu-{label}")))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .text_color(hx.tx1)
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(16.0))
                            .text_center()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(color)
                            .child(glyph.to_string()),
                    )
                    .child(div().font_family(hx.font_mono).child(rest.to_string())),
            )
            .child(div().text_color(hx.tx2).child(shortcut.to_string()))
            .on_click(cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.tab_menu = None;
                ws.open_tab(kind.clone(), window, cx);
            }))
    }

    fn render_status_bar(
        &self,
        hx: &HarnessTheme,
        r: Option<&Resolved>,
        cx: &App,
    ) -> impl IntoElement {
        let branch = r.map(|r| r.branch.clone()).unwrap_or_default();
        // N2: real LSP state, replacing the placeholder. Reports the language
        // servers actually running on this worktree's Project, so "rust-analyzer"
        // appearing means it started — not that we hope it did.
        let lsp = r
            .and_then(|r| self.panes.get(&r.key))
            .and_then(|p| {
                p.tabs.iter().find_map(|t| match &t.content {
                    TabContent::Editor(ws) => Some(ws),
                    _ => None,
                })
            })
            .map(|ws| {
                let project = ws.read(cx).project().read(cx);
                let names: Vec<String> = project
                    .language_server_statuses(cx)
                    .map(|(_, status)| status.name.to_string())
                    .collect();
                if names.is_empty() {
                    "no LSP".to_string()
                } else {
                    names.join(", ")
                }
            })
            .unwrap_or_else(|| "LSP: open an editor tab".to_string());
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
            .child(div().font_family(hx.font_mono).text_color(hx.tx2).child(lsp))
            .child(tool_chip(hx, "git", self.tools.git, true))
            .child(tool_chip(hx, "gh", self.tools.gh, false))
            .child(tool_chip(hx, "pi", self.tools.pi, false))
    }

    /// The unified-diff block for one file.
    fn unified_for_file(&self, path: &str) -> String {
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
            .unwrap_or_default()
    }

    /// The review toolbar above the file list: how many comments there are,
    /// and what can be done with them.
    ///
    /// Shown only when there is something to act on. Adding a comment is a
    /// click on a line's gutter number in the diff itself, so this bar is
    /// purely about what to do with the comments you already have.
    fn render_review_bar(
        &self,
        hx: &HarnessTheme,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chip = |id: &'static str, label: String, color: Rgba| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(20.0))
                .px_1p5()
                .rounded_md()
                .text_xs()
                .text_color(color)
                .cursor_pointer()
                .hover(|d| d.bg(hx.surface))
                .child(label)
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_3()
            .pb_1()
            .font_family(hx.font_mono)
            .child(div().flex_1())
            .child(div().flex_1())
            .child(
                    div()
                        .text_xs()
                        .text_color(hx.amber)
                        .child(format!("{count} comment{}", if count == 1 { "" } else { "s" })),
                )
            .child(chip("copy-review", "copy".to_string(), hx.tx2).on_click(
                cx.listener(|ws, _, _, cx| ws.copy_review(cx)),
            ))
            .child(chip("save-review", "save .md".to_string(), hx.tx2).on_click(
                cx.listener(|ws, _, _, cx| ws.save_review_markdown(cx)),
            ))
            .child(
                chip(
                        "clear-review",
                        if self.clearing_review { "discard?".to_string() } else { "clear".to_string() },
                        if self.clearing_review { hx.red } else { hx.tx2 },
                    )
                    .on_click(cx.listener(|ws, _, _, cx| ws.clear_review(cx))),
            )
    }

    /// The unified diff body, rendered inline beneath the file it belongs
    /// to (or beneath the whole list when no single file is selected).
    fn render_diff_body(&self, hx: &HarnessTheme, file: &str, cx: &mut Context<Self>) -> AnyElement {
        let mut preview = div()
            .id("diff-preview")
            .flex()
            .flex_col()
            .flex_none()
            .my_1()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.border)
            .items_start()
            .text_xs()
            .font_family(hx.font_mono);
        let unified = self.unified_for_file(file);
        if unified.is_empty() {
            preview = preview.child(div().p_2().text_color(hx.tx2).child("—"));
        } else {
            let shown = readable_diff_lines(&unified);
            let total = shown.len();
            let transparent = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
            // Comments hang off (file, line); with no file selected the rows
            // span several files, so fall back to each row's own header.
            let worktree_key = self.resolve().map(|r| r.key);
            let file_for_comments = file.to_string();
            for (ix, r) in shown.iter().enumerate().take(MAX_DIFF_LINES) {
                // A hunk boundary is a thin rule plus git's section text (the
                // enclosing function or heading). The old `@@ -a,b +c,d @@`
                // coordinates are gone: the gutter shows the real line numbers,
                // which is the same information without the decoding step.
                if r.kind == DiffLineKind::HunkBreak {
                    preview = preview.child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .h(px(18.0))
                            .px_2()
                            .border_t_1()
                            .border_color(hx.border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(hx.tx2)
                                    .whitespace_nowrap()
                                    .child(r.text.clone()),
                            ),
                    );
                    continue;
                }

                let (fg, bg, marker) = match r.kind {
                    DiffLineKind::FileHeader => (hx.tx0, hx.surface, ""),
                    DiffLineKind::Added => (rgb(DIFF_ADD_TX), tint(hx.green, 0.13), "+"),
                    DiffLineKind::Removed => (rgb(DIFF_DEL_TX), tint(hx.red, 0.13), "−"),
                    DiffLineKind::Context => (hx.tx2, transparent, " "),
                    DiffLineKind::HunkBreak => unreachable!("handled above"),
                };

                let selected = self
                    .diff_selection
                    .as_ref()
                    .is_some_and(|(f, from, to)| f == file && ix >= *from && ix <= *to);
                let row_bg = if selected { tint(hx.accent, 0.22) } else { bg };

                if r.kind == DiffLineKind::FileHeader {
                    preview = preview.child(
                        div()
                            .flex()
                            .flex_none()
                            .h(px(15.0))
                            .bg(row_bg)
                            .px_2()
                            .text_color(fg)
                            .whitespace_nowrap()
                            .child(r.text.clone()),
                    );
                    continue;
                }

                let line_no = r.number;
                let comment_key = (file_for_comments.clone(), line_no);
                let row_file = file_for_comments.clone();
                let move_file = file_for_comments.clone();
                let has_comment = line_no.is_some_and(|n| {
                    worktree_key
                        .as_deref()
                        .is_some_and(|k| self.review.get(k, &file_for_comments, n).is_some())
                });

                preview = preview.child(
                    div()
                        .id(("diffrow", ix))
                        .flex()
                        .flex_none()
                        .h(px(15.0))
                        .bg(row_bg)
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                                ws.diff_selecting = true;
                                ws.select_diff_row(&row_file, ix, ev.modifiers.shift, cx);
                            }),
                        )
                        // Dragging over rows extends the selection, which is
                        // what "highlight some lines" means with a mouse.
                        .on_mouse_move(cx.listener(move |ws, _: &MouseMoveEvent, _, cx| {
                            if ws.diff_selecting {
                                ws.extend_diff_selection(&move_file, ix, cx);
                            }
                        }))
                        .child(
                            // Fixed-width gutter so the code column stays
                            // aligned as numbers grow past 999. Doubles as the
                            // comment affordance.
                            div()
                                .id(("diffgutter", ix))
                                .flex_none()
                                .w(px(46.0))
                                .pr_1()
                                .flex()
                                .items_center()
                                .gap_1()
                                .when(has_comment, |d| d.bg(tint(hx.amber, 0.18)))
                                .text_color(if has_comment { hx.amber } else { hx.tx2 })
                                .hover(|d| d.bg(hx.surface).text_color(hx.accent))
                                // The marker sits in a fixed slot rather than
                                // being prefixed to the number, so commented
                                // lines do not shift their digits sideways.
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(8.0))
                                        .child(if has_comment { "▸" } else { "" }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .child(line_no.map(|n| n.to_string()).unwrap_or_default()),
                                )
                                .on_click(cx.listener(
                                    move |ws, ev: &gpui::ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        if let Some(line) = comment_key.1 {
                                            ws.begin_comment(
                                                comment_key.0.clone(),
                                                line,
                                                ev.position(),
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )),
                        )
                        .child(div().flex_none().w(px(10.0)).text_color(fg).child(marker))
                        .child(
                            div()
                                .flex_none()
                                .pr_2()
                                .text_color(fg)
                                .whitespace_nowrap()
                                .child(r.text.clone()),
                        ),
                );

                // An existing note renders under its line, where the thing it
                // is about is still on screen.
                if has_comment
                    && let (Some(k), Some(n)) = (worktree_key.as_deref(), line_no)
                    && let Some(c) = self.review.get(k, &file_for_comments, n)
                {
                    preview = preview.child(
                        div()
                            .flex()
                            .flex_none()
                            .py_0p5()
                            .pl(px(56.0))
                            .pr_2()
                            .bg(tint(hx.amber, 0.10))
                            .border_l_2()
                            .border_color(hx.amber)
                            .text_color(hx.tx1)
                            .child(c.body.clone()),
                    );
                }
            }
            if total > MAX_DIFF_LINES {
                // Never truncate silently — a diff that just stops looks like
                // the change is smaller than it is.
                preview = preview.child(
                    div()
                        .flex_none()
                        .h(px(15.0))
                        .px_2()
                        .text_color(hx.amber)
                        .child(format!(
                            "… {} more lines — select a single file, or view the full diff in git",
                            total - MAX_DIFF_LINES
                        )),
                );
            }
        }
        preview.into_any_element()
    }

    fn render_diff_panel(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let Some(r) = self.resolve() else {
            return div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(self.prefs.diff_width))
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
        let review_count = self.resolve().map_or(0, |r| self.review.count(&r.key));

        let mut panel = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.prefs.diff_width))
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
                    .child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono).text_color(hx.tx1).overflow_hidden().child(r.branch.clone()))
                    ,
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
                            let total = self.diff.files.len();
                            let done = self.resolve().map_or(0, |r| {
                                self.review.reviewed_count(
                                    &r.key,
                                    self.diff.files.iter().map(|f| f.path.as_str()),
                                )
                            });
                            if done > 0 {
                                format!("Changed files  {done}/{total} reviewed")
                            } else {
                                format!("Changed files  {total}")
                            }
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .child(div().text_color(hx.green).child(format!("+{adds}")))
                            .child(div().text_color(hx.red).child(format!("−{dels}"))),
                    ),
            );

        // One scroller holding the file list, with the selected file's diff
        // expanded directly beneath its row — so the diff is next to the name
        // it belongs to instead of in a separate pane you have to look away to.
        let worktree_key = self.resolve().map(|r| r.key);
        let mut files = div()
            .id("diff-files")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_start()
            .overflow_scroll()
            .px_2()
            .pb_1();
        if self.diff.files.is_empty() {
            files = files.child(div().px_2().py_1().text_xs().text_color(hx.tx2).child("no changes vs merge-base"));
        }
        for (i, file) in self.diff.files.iter().enumerate() {
            let path = file.path.clone();
            let expanded = self.expanded.contains(&file.path);
            let reviewed = worktree_key
                .as_deref()
                .is_some_and(|k| self.review.is_reviewed(k, &file.path));
            let badge_color = match file.change {
                Change::Added => hx.green,
                Change::Deleted => hx.red,
                Change::Renamed => hx.accent,
                _ => hx.amber,
            };
            let check_path = path.clone();
            let toggle_path = path.clone();
            let md_path = path.clone();

            files = files.child(
                div()
                    .id(("difffile", i))
                    .flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .when(expanded, |d| d.bg(hx.surface))
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        if !ws.expanded.remove(&toggle_path) {
                            ws.expanded.insert(toggle_path.clone());
                        }
                        cx.notify();
                    }))
                    // Reviewed checkbox. Ticking it collapses the file: the
                    // point of marking it is to get it out of the way, and
                    // leaving it open would defeat that.
                    .child(
                        div()
                            .id(("reviewed", i))
                            .flex_none()
                            .w(px(14.0))
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(if reviewed { hx.green } else { hx.tx2 })
                            .hover(|d| d.text_color(hx.green))
                            .child(if reviewed { "☑" } else { "☐" })
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                cx.stop_propagation();
                                let Some(r) = ws.resolve() else { return };
                                let now = !ws.review.is_reviewed(&r.key, &check_path);
                                ws.review.set_reviewed(&r.key, &check_path, now);
                                ws.review.save(&ws.review_path);
                                if now {
                                    ws.expanded.remove(&check_path);
                                }
                                cx.notify();
                            })),
                    )
                    // Disclosure marker, so expanded state reads without
                    // relying on the row background alone.
                    .child(
                        div()
                            .flex_none()
                            .w(px(8.0))
                            .text_xs()
                            .text_color(hx.tx2)
                            .child(if expanded { "▾" } else { "▸" }),
                    )
                    .child(div().flex_none().w(px(12.0)).text_xs().font_family(hx.font_mono).text_color(badge_color).child(file.change.badge().to_string()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(hx.font_mono)
                            // Reviewed files recede rather than vanish — still
                            // findable, visibly done.
                            .text_color(if reviewed { hx.tx2 } else { hx.tx1 })
                            .overflow_hidden()
                            .child(file.path.clone()),
                    )
                    // The markdown preview belongs to a file, so its toggle
                    // lives on that file's row, not the panel header.
                    .when(expanded && crate::markdown_preview::is_markdown(&file.path), |d| {
                        let previewing =
                            self.md_preview.as_ref().is_some_and(|(f, _)| f == &md_path);
                        let p = md_path.clone();
                        d.child(
                            div()
                                .id(("md-toggle", i))
                                .flex_none()
                                .px_1()
                                .rounded_md()
                                .text_xs()
                                .text_color(if previewing { hx.accent } else { hx.tx2 })
                                .hover(|h| h.bg(hx.surface_hover))
                                .child(if previewing { "◨" } else { "◧" })
                                .on_click(cx.listener(move |ws, _, _, cx| {
                                    cx.stop_propagation();
                                    ws.toggle_md_preview(&p, cx);
                                })),
                        )
                    })
                    .child(div().flex_none().text_xs().font_family(hx.font_mono).text_color(hx.green).child(format!("+{}", file.adds)))
                    .child(div().flex_none().text_xs().font_family(hx.font_mono).text_color(hx.red).child(format!("−{}", file.dels))),
            );

            if expanded {
                match self.md_preview.as_ref().filter(|(f, _)| f == &path) {
                    Some((_, md)) => {
                        files = files.child(
                            div()
                                .w_full()
                                .h(px(360.0))
                                .my_1()
                                .rounded_md()
                                .bg(hx.bg0)
                                .border_1()
                                .border_color(hx.border)
                                .child(md.clone()),
                        );
                    }
                    None => files = files.child(self.render_diff_body(hx, &path, cx)),
                }
            }
        }

        if review_count > 0 {
            panel = panel.child(self.render_review_bar(hx, review_count, cx));
        }
        panel = panel.child(files);

        panel.into_any_element()

    }
}

impl Focusable for HarnessWorkspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HarnessWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The font-size pref scales the whole app, not only the code
        // surfaces: chrome text is rem-based (text_xs/text_sm), so scaling
        // the window's rem size rescales it all — the same mechanism zed's
        // setup_ui_font uses inside the hosted workspace. 13px ↦ the 16px
        // default rem. Fixed px dimensions (bar heights, panel widths) stay
        // fixed on purpose; text is what legibility needs.
        let scale = self.prefs.font_size.unwrap_or(13.0) / 13.0;
        window.set_rem_size(px(16.0 * scale));
        let hx = theme(cx).clone();
        let mut root = div()
            .relative()
            .key_context("Harness")
            .flex()
            .flex_col()
            .size_full()
            .bg(hx.bg0)
            .font_family(hx.font_ui)
            .text_color(hx.tx0)
            .child(self.render_top_bar(&hx, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    // `h_full` alongside `flex_1`, the way zed's own containers
                    // pair them. flex_1 alone leaves the height *indefinite* in
                    // taffy's eyes: the div gets stretched to the right size,
                    // but percentage heights below it fall back to content
                    // size. zed's dock caches its panel as a childless leaf
                    // node, whose content height is zero — so the file tree
                    // laid out into a 239×0 box while every state probe read
                    // healthy. min_h_0 still allows shrinking.
                    .h_full()
                    .min_h_0()
                    // Collapsing hides the splitter too — a resize handle for
                    // a panel that isn't there is just a dead strip of chrome.
                    .when(self.prefs.sidebar_open, |d| {
                        d.child(self.render_sidebar(&hx, cx))
                            .child(self.splitter(&hx, Drag::Sidebar, cx))
                    })
                    .child(self.render_center(&hx, window, cx))
                    .when(self.prefs.diff_open, |d| {
                        d.child(self.splitter(&hx, Drag::DiffPanel, cx))
                            .child(self.render_diff_panel(&hx, cx))
                    }),
            )
            .children(self.render_zed_modal(cx))
            .children(self.render_pet(&hx, cx))
            // The settings overlay is the LAST child on purpose: gpui paints
            // siblings in order, so an overlay added before the content row
            // renders underneath it — "settings don't open" while the state
            // toggles perfectly. Absolute positioning takes it out of flow;
            // being last takes it to the top.
            .when(self.settings_open, |d| d.child(self.render_settings(&hx, cx)))
            // Drag tracking lives on the root so the pointer can leave the
            // 6px handle mid-drag without the resize stopping.
            .on_action(cx.listener(|ws, _: &crate::CopyDiffSelection, _, cx| {
                ws.copy_diff_selection(cx)
            }))
            .on_action(cx.listener(|ws, _: &crate::NewTerminal, window, cx| {
                ws.open_tab(TabKind::Shell, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &crate::NewEditor, window, cx| {
                ws.open_tab(TabKind::Editor, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &crate::CloseTab, window, cx| {
                log::debug!("CloseTab (root ⌘W)");
                let Some(r) = ws.resolve() else { return };
                let Some(active) = ws.panes.get(&r.key).map(|p| p.active) else {
                    return;
                };
                ws.close_tab(active, window, cx);
            }))
            .on_action(cx.listener(|ws, _: &crate::OpenSettings, _, cx| {
                ws.settings_open = !ws.settings_open;
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &crate::NewChat, window, cx| {
                ws.open_tab(TabKind::Chat, window, cx)
            }))
            .on_mouse_move(cx.listener(|ws, ev: &MouseMoveEvent, _, cx| ws.drag_move(ev, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseUpEvent, _, cx| {
                    ws.diff_selecting = false;
                    ws.end_drag(cx);
                }),
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

        if let Some(draft) = self.comment_draft.clone() {
            root = root
                .child(
                    // Click outside saves and closes, so a comment is never
                    // lost by clicking away — the common accident.
                    div()
                        .id("comment-backdrop")
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| ws.commit_comment(cx)),
                        ),
                )
                .child(self.render_comment_popover(&hx, &draft, cx));
        }

        if let Some(x) = self.tab_menu {
            root = root
                .child(
                    // Click-anywhere-else dismisses, same as the card menu.
                    div()
                        .id("tab-menu-backdrop")
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| {
                                ws.tab_menu = None;
                                cx.notify();
                            }),
                        ),
                )
                .child(self.render_tab_menu(&hx, x, cx));
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

/// User-interface preferences persisted across launches (§12 cross-cutting).
///
/// Deliberately separate from `registry.json`: that file is *project state* the
/// training loop reads (§6.4), and mixing window chrome into it would make a
/// theme toggle look like a change to harvestable data.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Prefs {
    dark: bool,
    /// A zed theme name overriding the harness dark/light pair. `None` means
    /// the built-in pair selected by `dark`.
    #[serde(default)]
    zed_theme: Option<String>,
    /// Font overrides; `None` means the resolved defaults.
    #[serde(default)]
    font_ui: Option<String>,
    #[serde(default)]
    font_mono: Option<String>,
    /// Per-agent settings, keyed by `AgentKind::as_str()`. Missing entry =
    /// enabled, default command.
    #[serde(default)]
    agents: std::collections::BTreeMap<String, AgentPref>,
    /// Editor + terminal font size in px. `None` = 13.
    #[serde(default)]
    font_size: Option<f32>,
    /// The conductor pet (Clippy's grandchild): a small animated maestro in
    /// the corner whenever a notification posts.
    #[serde(default = "default_true")]
    pet_enabled: bool,
    /// Shortcut rebindings, keyed by `shortcuts::key_id`. Value is the gpui
    /// chord string; missing = the catalog default.
    #[serde(default)]
    shortcuts: std::collections::BTreeMap<String, String>,
    sidebar_width: f32,
    diff_width: f32,
    sidebar_open: bool,
    diff_open: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            dark: true,
            zed_theme: None,
            font_ui: None,
            font_mono: None,
            agents: Default::default(),
            font_size: None,
            pet_enabled: true,
            shortcuts: Default::default(),
            sidebar_width: 280.0,
            diff_width: 340.0,
            sidebar_open: true,
            diff_open: true,
        }
    }
}

impl Prefs {
    /// Clamp on load, not just on write: a hand-edited or truncated prefs file
    /// must not be able to render a panel unusable or invisible.
    fn sanitized(mut self) -> Self {
        self.sidebar_width = self.sidebar_width.clamp(180.0, 640.0);
        self.diff_width = self.diff_width.clamp(220.0, 800.0);
        self
    }
}

/// Whether the last session used the dark palette.
///
/// Read before the window exists, so `main` can install the right zed theme up
/// front rather than flashing dark and correcting.
/// The persisted shortcut rebindings, for `main` to bind before any window
/// exists. Same file the settings screen writes.
pub fn persisted_shortcut_overrides() -> std::collections::BTreeMap<String, String> {
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".openorchestra")
        .join("prefs.json");
    load_prefs(&path).shortcuts
}

pub fn persisted_dark_theme() -> bool {
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".openorchestra")
        .join("prefs.json");
    load_prefs(&path).dark
}

fn load_prefs(path: &Path) -> Prefs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Prefs>(&s).ok())
        .unwrap_or_default()
        .sanitized()
}

fn save_prefs_to(path: &Path, prefs: &Prefs) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(path, json);
    }
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

#[cfg(test)]
mod diff_render_tests {
    use super::{readable_diff_lines, DiffLineKind, NewWorktree, Prefs};

    /// Verbatim shape of the noise a new file produces — the case that
    /// prompted this: five metadata lines before any content.
    const NEW_FILE: &str = "\
diff --git a/agent-schema/README.md b/agent-schema/README.md
new file mode 100644
index 0000000..2cf3f44
--- /dev/null
+++ b/agent-schema/README.md
@@ -0,0 +1,67 @@
+# agent-schema — the tool contract
+
";

    #[test]
    fn git_metadata_preamble_is_replaced_by_a_single_file_header() {
        let rows = readable_diff_lines(NEW_FILE);
        assert_eq!(rows[0].kind, DiffLineKind::FileHeader);
        assert_eq!(rows[0].text, "agent-schema/README.md", "header names the file");

        for noise in [
            "diff --git", "new file mode", "index 0000000", "--- /dev/null", "+++ b/", "@@",
        ] {
            assert!(
                !rows.iter().any(|r| r.text.contains(noise)),
                "{noise:?} should not be rendered, got {:?}",
                rows.iter().map(|r| &r.text).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn every_content_line_carries_its_real_file_line_number() {
        // This is what replaces the `@@ -0,0 +1,67 @@` header: the coordinates
        // were only ever a way to work out these numbers by hand.
        let rows = readable_diff_lines(NEW_FILE);
        let added: Vec<(Option<u32>, &str)> = rows
            .iter()
            .filter(|r| r.kind == DiffLineKind::Added)
            .map(|r| (r.number, r.text.as_str()))
            .collect();
        assert_eq!(added[0].0, Some(1), "new file starts at line 1");
        assert_eq!(added[0].1, "# agent-schema — the tool contract");
        assert_eq!(added[1].0, Some(2), "and increments");
    }

    #[test]
    fn line_numbers_track_old_and_new_files_independently() {
        // Removed lines number against the OLD file, added and context against
        // the new one — otherwise a deletion shifts every later number.
        let diff = "@@ -10,4 +20,4 @@\n ctx\n-gone\n+added\n ctx2\n";
        let rows = readable_diff_lines(diff);
        let n = |k: DiffLineKind| -> Vec<Option<u32>> {
            rows.iter().filter(|r| r.kind == k).map(|r| r.number).collect()
        };
        assert_eq!(n(DiffLineKind::Removed), vec![Some(11)], "old-file numbering");
        assert_eq!(n(DiffLineKind::Added), vec![Some(21)], "new-file numbering");
        assert_eq!(n(DiffLineKind::Context), vec![Some(20), Some(22)]);
    }

    #[test]
    fn the_hunk_section_heading_survives_but_the_coordinates_do_not() {
        // git puts the enclosing function/heading after the second `@@`. That
        // part is genuinely useful; the numbers are what the gutter now shows.
        let diff = "@@ -1,2 +1,2 @@ fn interesting()\n a\n@@ -50,2 +50,2 @@ fn other()\n b\n";
        let rows = readable_diff_lines(diff);
        let breaks: Vec<&str> = rows
            .iter()
            .filter(|r| r.kind == DiffLineKind::HunkBreak)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(breaks, vec!["fn interesting()", "fn other()"]);
    }

    #[test]
    fn the_diff_marker_column_is_stripped_from_the_text() {
        // The gutter and the row tint already say what happened; keeping the
        // leading +/- shifts every line against the file's real indentation.
        let diff = "@@ -1,1 +1,1 @@\n-    old_indented()\n+    new_indented()\n";
        let rows = readable_diff_lines(diff);
        let texts: Vec<&str> = rows
            .iter()
            .filter(|r| r.number.is_some())
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(texts, vec!["    old_indented()", "    new_indented()"]);
    }

    #[test]
    fn a_deletion_of_a_dashed_line_is_not_mistaken_for_a_file_marker() {
        // `--- ` is the file-pair marker, but `--foo` is a legitimate removed
        // line. Requiring the space-terminated triple form keeps them apart.
        let diff = "@@ -1,3 +1,1 @@\n---foo\n-  --bar\n+baz\n";
        let removed: Vec<String> = readable_diff_lines(diff)
            .iter()
            .filter(|r| r.kind == DiffLineKind::Removed)
            .map(|r| r.text.clone())
            .collect();
        // The leading `-` marker is stripped; the rest of the line is intact.
        assert_eq!(removed, vec!["--foo", "  --bar"]);
    }

    #[test]
    fn renames_are_stated_in_words_rather_than_dropped() {
        let diff = "diff --git a/old.rs b/new.rs\nsimilarity index 98%\nrename from old.rs\nrename to new.rs\n";
        let rows = readable_diff_lines(diff);
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text[0], "new.rs", "header uses the post-rename path");
        assert!(text.contains(&"renamed from old.rs"));
        assert!(!text.iter().any(|l| l.contains("similarity index")));
    }

    #[test]
    fn multiple_files_each_get_a_header() {
        let diff = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n+x\n\
diff --git a/b.rs b/b.rs\nindex 3..4 100644\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n+y\n";
        let headers: Vec<String> = readable_diff_lines(diff)
            .into_iter()
            .filter(|r| r.kind == DiffLineKind::FileHeader)
            .map(|r| r.text)
            .collect();
        assert_eq!(headers, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn panel_sizes_are_clamped_so_a_drag_cannot_make_one_unrecoverable() {
        let squashed = Prefs { sidebar_width: 0.0, diff_width: -50.0, ..Prefs::default() }
            .sanitized();
        assert!(squashed.sidebar_width >= 180.0);
        assert!(squashed.diff_width >= 220.0);

        let huge = Prefs { sidebar_width: 9999.0, diff_width: 9999.0, ..Prefs::default() }
            .sanitized();
        assert!(huge.sidebar_width <= 640.0);
        assert!(huge.diff_width <= 800.0);
    }

    #[test]
    fn worktree_spec_still_parses() {
        // Guards the M5a syntax against regressions from this refactor.
        let spec = NewWorktree::parse("fix bug #hot/fix @v2", "origin/main");
        assert_eq!(spec.name, "fix bug");
        assert_eq!(spec.branch.as_deref(), Some("hot/fix"));
        assert_eq!(spec.base_ref, "v2");
    }
}
