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

use editor::Editor;

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
pub(crate) enum TabKind {
    /// A plain `$SHELL`. What a worktree opens with.
    Shell,
    /// A specific agent CLI (§4.5) — any of the five, not only the
    /// worktree's recorded default.
    Agent(AgentKind),
    Editor,
    Chat,
}

pub(crate) struct Tab {
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
pub(crate) enum Selection {
    Main(String),
    Worktree(String),
}

/// What an active inline edit targets. The text itself lives in the backing
/// `Editor` (full caret/selection/clipboard editing), not here — this only
/// carries which thing is being edited so the commit knows where to write.
#[derive(Clone)]
pub(crate) enum EditKind {
    /// New worktree name, created under `new_target_repo`.
    NewName,
    /// Rename worktree `id`.
    Rename(String),
    /// Edit worktree `id`'s avatar initials.
    Avatar(String),
    /// Edit an agent's launch command in settings (kind string).
    AgentCmd(String),
}

/// An inline edit in progress: what it targets, and the single-line `Editor`
/// backing it. Replacing the old hand-rolled append-only field, this brings
/// real text editing — caret motion, selection, ⌘A/C/V/X, IME.
struct ActiveEdit {
    kind: EditKind,
    editor: Entity<Editor>,
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
pub(crate) enum Drag {
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
pub(crate) struct CommentDraft {
    file: String,
    line: u32,
    /// The single-line editor holding the draft comment text.
    editor: Entity<Editor>,
    /// Window coordinates of the gutter cell that opened it.
    at: gpui::Point<Pixels>,
}

pub(crate) struct CardMenu {
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

pub(crate) struct Resolved {
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
    /// The active inline edit, if any. `Editor`-backed, so it carries its own
    /// focus, caret, and selection — no more hand-rolled caret blink.
    edit: Option<ActiveEdit>,
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
    /// Watches the selected root's files so the diff panel keeps up with an
    /// agent's edits in near real time (retargeted on every selection change).
    diff_watch: crate::diff_watch::DiffWatcher,
    /// Where the adapters materialize their extensions and shims.
    state_dir: PathBuf,
    prefs: Prefs,
    prefs_path: PathBuf,
    drag: Option<DragState>,
    bus: Bus,
    tools: ToolStatus,
    focus_handle: FocusHandle,
    /// Focus target for the hosted-editor content wrapper. That wrapper carries
    /// the `"Workspace"` key context and the workspace action handlers, so
    /// focusing it makes ⌘P / ⌘⇧F resolve even when no file is open — otherwise
    /// focus drifts to the `"Harness"` root, which has neither, and the
    /// keystrokes silently no-op.
    editor_host_focus: FocusHandle,
}


// The chrome view's methods are split across the `workspace/` module by
// feature area; each file is an `impl HarnessWorkspace` block. Only the
// constructor, shared types, the Render/Focusable roots, and free
// helpers live here.
mod state;
mod actions;
mod ui_chrome;
mod ui_settings;
mod ui_sidebar;
mod ui_center;
mod ui_diff;

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

        let mut branches: Vec<BranchKey> = manager
            .registry
            .active()
            .map(|e| BranchKey {
                repo_dir: e.repo_root.clone(),
                branch: e.branch.clone(),
            })
            .collect();
        // Main-checkout CI: poll each repo's default branch too (main is
        // first-class). PRs don't apply to a default branch, but its CI does.
        for repo in &data.repos {
            branches.push(BranchKey {
                repo_dir: repo.repo_root.clone(),
                branch: repo.default_branch.clone(),
            });
        }
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
                // Main checkouts are first-class (D8 revised): watch each repo
                // root too, so an agent run on `main` drives the same status
                // pill and notifications a worktree does. The kind is a fallback
                // — status files self-identify their writer.
                for root in &repo_roots {
                    if let Err(err) = source.watch(root, AgentKind::default()) {
                        log::warn!("cannot watch main {}: {err}", root.display());
                    }
                }
                Some(source)
            }
            Err(e) => {
                log::warn!("status watcher unavailable: {e}");
                None
            }
        };

        let (diff_watch, diff_dirty_rx) = crate::diff_watch::DiffWatcher::new();

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
            edit: None,
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
            diff_watch,
            state_dir: data_dir.clone(),
            prefs,
            prefs_path,
            drag: None,
            bus,
            tools: detect_tools(),
            focus_handle: cx.focus_handle(),
            editor_host_focus: cx.focus_handle(),
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

        // Live diff refresh: a file changed under the selected root. The 250 ms
        // poll cadence is the debounce — a burst of edits in one agent turn
        // collapses to one recompute per tick, and the `diff_gen` guard drops
        // any overlap. State-preserving so an in-progress review is not reset.
        cx.spawn(async move |view, cx| loop {
            match diff_dirty_rx.try_recv() {
                Ok(_) => {
                    // Coalesce everything already queued this instant.
                    while diff_dirty_rx.try_recv().is_ok() {}
                    if view.update(cx, |ws, cx| ws.refresh_diff_live(cx)).is_err() {
                        break;
                    }
                }
                Err(e) if e.is_empty() => {
                    cx.background_executor().timer(Duration::from_millis(250)).await
                }
                Err(_) => break,
            }
        })
        .detach();

        this
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

        // Keep the agent-command field focused while the Settings overlay edits
        // it. That field floats over the hosted zed editor, which holds window
        // focus when Settings is opened from the menu (⌘,) — so `begin_edit`
        // lands the caret but the editor behind keeps the keystrokes and the
        // field looks dead. The sidebar's rename uses this same input and never
        // sees this, because interacting with the sidebar already moved focus
        // off the editor. Re-assert only for this exact case, and only once
        // focus has actually escaped, so it converges instead of thrashing.
        if self.settings_open
            && matches!(self.edit_kind(), Some(EditKind::AgentCmd(_)))
            && let Some(editor) = self.edit_editor()
        {
            let handle = editor.focus_handle(cx);
            if !handle.is_focused(window) {
                window.focus(&handle, cx);
            }
        }

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
        pi: on_path(agent_runtime::default_invocation(&AgentKind::Pi).0),
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

/// A minimal text tooltip — a styled popover label shown on hover, so
/// icon-only controls (the tab `×`, the reviewed checkbox, the font-size
/// steppers) carry a readable name. Native apps have no ARIA; a hover label
/// is the accessible-name affordance available here.
struct TextTooltip {
    text: gpui::SharedString,
}

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hx = theme(cx);
        div()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .rounded_md()
            .px_1p5()
            .py_0p5()
            .text_xs()
            .text_color(hx.tx0)
            .child(self.text.clone())
    }
}

/// Build the closure `.tooltip()` wants: it hands back a fresh tooltip view
/// each time the pointer settles on the element it is attached to.
fn tip(
    text: impl Into<gpui::SharedString>,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let text = text.into();
    move |_window, cx| cx.new(|_| TextTooltip { text: text.clone() }).into()
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
