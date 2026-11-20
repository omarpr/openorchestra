//! Root view — three-column shell styled to `docs/harness-ui/Harness.dc.html`.
//!
//! The app keeps its state in `~/.openorchestra/` (registry + the list of
//! project roots), independent of where it was launched — so a Finder-launched
//! `.app` (cwd `/`) still works. Projects are added via a native directory
//! picker; when none are open the app shows an empty state. Sidebar cards, the diff panel,
//! per-worktree tabs, create/rename/avatar, right-click menu, live PR/CI +
//! agent badges, and tool-status chips are all here.
//!
//! Selection is by id and resolves to `None` when no project is open, so nothing
//! can index an empty repo list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EntityId, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    PathPromptOptions, Pixels, Render, Rgba, Styled, Window, div, prelude::*, px, rgb, rgba, svg,
};

use editor::Editor;

use agent_runtime::{AgentKind, AgentSignal, AgentState, Bus, StatusFileSource, WaitFidelity};
use forge::{BranchStatus, CheckStatus, ForgeUpdate, GhCli, Poller, PollerConfig};
use worktrees::{
    Change, CreateTaskMember, CreateTaskRequest, GitWorktrees, HookEnv, HookLayer, HookScriptPhase,
    HooksConfig, Manager, Registry, SystemRunner, TaskHooks, TaskStore, clear_local_hooks,
    create_task, ensure_hook_script, local_hooks_base, migrate_worktrees_to_tasks, task_name_slug,
    visible_setup_shell_line, write_task_agent_context, write_task_brief,
    write_visible_setup_runner,
};

use crate::data::{UnifiedDiff, WorkspaceData, compute_diff};
use crate::review_overview::models::ModelCatalog;
use crate::review_overview::{
    ReadingStep, ReviewCli, ReviewFollowup, ReviewPreferences, ReviewRun, StoredReviewOverview,
};
use crate::terminal_pane::TerminalPane;
use crate::theme::{HarnessTheme, theme, tint};

const SWATCHES: [u32; 8] = [
    0x5b9bff, 0x4ec26b, 0xa877f5, 0xe0a83b, 0xf0616d, 0x22b8cf, 0xf27bb0, 0x8b93a0,
];
/// Which pane of Settings is showing.
///
/// One page had grown to five topics and 900 lines of builder; a rail turns
/// "scroll until you recognise it" into "pick the thing you came for", and gives
/// Notifications somewhere to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    General,
    Appearance,
    Editor,
    Diff,
    Agents,
    Notifications,
    Remote,
    Shortcuts,
}

impl SettingsSection {
    pub(crate) const ALL: [SettingsSection; 8] = [
        SettingsSection::General,
        SettingsSection::Appearance,
        SettingsSection::Editor,
        SettingsSection::Diff,
        SettingsSection::Agents,
        SettingsSection::Notifications,
        SettingsSection::Remote,
        SettingsSection::Shortcuts,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Editor => "Editor",
            // The pane holds Review location, diff layout and Review
            // Overview: named for the panel it configures.
            SettingsSection::Diff => "Review",
            SettingsSection::Agents => "Agents",
            SettingsSection::Notifications => "Notifications",
            SettingsSection::Remote => "Remote",
            SettingsSection::Shortcuts => "Shortcuts",
        }
    }

    fn focus_id(self) -> gpui::ElementId {
        format!("settings-nav-{}", self.label()).into()
    }
}

/// One line of the rendered diff body.
#[derive(Clone, Copy)]
pub(crate) enum BodyItem {
    /// An index into the file's parsed rows.
    Row(usize),
    /// One side-by-side line, in split view.
    Split(SplitRow),
    /// A review note, pinned under the line it annotates.
    Note(u32),
    /// The tail marker: how many lines the cap dropped.
    Truncated(usize),
}

const DIFF_ADD_TX: u32 = 0x7fdd97;
const DIFF_DEL_TX: u32 = 0xf2868f;

/// A file's diff body in the row cache.
///
/// `Fetching` is inserted synchronously, the instant a fetch is spawned, and is
/// what makes the request idempotent: rendering is re-entered many times while
/// a `git diff` is in flight, and each of those frames would otherwise start
/// its own.
pub(crate) enum FileRows {
    Fetching,
    Ready(std::sync::Arc<Vec<DiffRow>>),
}

/// What a tab can hold. Tab sets are per worktree and fully dynamic (§3.4):
/// a worktree can carry any number of terminals, file editors, and chats.
///
/// File buffers live in a per-worktree **service** Zed `Workspace` (not itself a
/// tab). Opening a file or git diff promotes that pane item into a harness tab.
enum TabContent {
    Terminal(Entity<TerminalPane>),
    /// A buffer / solo-diff / search item from the service Workspace.
    File {
        item_id: EntityId,
        /// Italic preview tab — replaced by the next single-click open.
        preview: bool,
    },
    /// Native agent chat over pi's RPC mode (M8, §4.2–4.3).
    Chat(Entity<crate::chat_pane::ChatPane>),
    /// The full editor ("IDE") tab: a complete zed workspace with its own file
    /// tree and Git docks, tab bar and status bar, over the same project as the
    /// tools panel. The other way to edit, alongside per-file tabs.
    Editor(Entity<::workspace::Workspace>),
    /// The diff review surface, when `DiffLocation::Tab` puts it in the center.
    ///
    /// A UNIT variant, unlike the other three: the diff, the review notes, and
    /// the expand/selection state all live on `HarnessWorkspace` already and are
    /// resolved per worktree. Giving the tab its own entity would create a
    /// second copy that could disagree with the dock's.
    Diff,
    ReviewOverview(OverviewTab),
}

#[derive(Default)]
struct OverviewTab {
    preferences: ReviewPreferences,
    status: OverviewStatus,
    last_report: Option<Arc<StoredReviewOverview>>,
    reading_steps: Vec<ReadingStep>,
    /// The step whose file was opened most recently from the reading-order path.
    last_reading_step: Option<usize>,
}

#[derive(Default)]
enum OverviewStatus {
    #[default]
    Waiting,
    Cancelled,
    Running(ReviewRun),
    Ready {
        markdown: Entity<markdown::Markdown>,
        saved: Arc<StoredReviewOverview>,
    },
    Failed(String),
}

/// What to open, before the entity exists.
#[derive(Clone, PartialEq)]
pub(crate) enum TabKind {
    /// A plain `$SHELL`. What a worktree opens with.
    Shell,
    /// A specific agent CLI (§4.5) — any of the five, not only the
    /// worktree's recorded default.
    Agent(AgentKind),
    Chat,
    /// The diff review surface as a center tab (`DiffLocation::Tab`).
    Diff,
    ReviewOverview,
    /// The full editor (IDE) tab — one per worktree.
    Editor,
}

/// The tab being dragged, and the pane set it came from.
///
/// Carries the worktree key so a drop can refuse a tab from somewhere else: tab
/// sets are per worktree, and a terminal tab holds a pty spawned in that
/// worktree's directory. Moving one across would leave a running agent pointed
/// at a tree it was never started in.
pub(crate) struct DraggedTab {
    key: String,
    id: u64,
}

/// `items` with `dragged` moved to `onto`'s position. Unknown or identical
/// items leave the order untouched. Used for tabs (ids) and projects (roots).
fn reordered<T: PartialEq + Clone>(items: &[T], dragged: T, onto: T) -> Vec<T> {
    let mut out = items.to_vec();
    if dragged == onto {
        return out;
    }
    let (Some(from), Some(to)) = (
        out.iter().position(|i| *i == dragged),
        out.iter().position(|i| *i == onto),
    ) else {
        return out;
    };
    let item = out.remove(from);
    out.insert(to, item);
    out
}

/// The list a reorder applies to: the copy on disk now, since the other
/// build (Dev and the installed release share ~/.openorchestra) may have
/// added or removed entries since this one loaded. An empty disk copy
/// (missing or unreadable file) while this build has entries falls back to
/// this build's list, so a reorder never empties it.
fn reorder_base<T: Clone>(disk: Vec<T>, current: &[T]) -> Vec<T> {
    if disk.is_empty() {
        current.to_vec()
    } else {
        disk
    }
}

/// Reorder panel geometry at the default 16 px rem: its width, a row's
/// height (text + padding + gap; at least the 22 px grip box), and the
/// title, Done button and padding around the list.
const REORDER_PANEL_W: f32 = 240.0;
const REORDER_ROW_H: f32 = 32.0;
const REORDER_PANEL_CHROME_H: f32 = 80.0;
const REORDER_PANEL_MARGIN: f32 = 8.0;

/// Where the reorder panel goes for a click-derived `origin` (x, y): moved
/// so the whole panel — title, the list (capped at `list_max_h`) and Done —
/// stays inside the window, at least a margin from each edge. Heights grow
/// with the UI font size (`rem`, px); they never shrink below the 16 px
/// sizes, since parts of a row are fixed px. A window too small for the
/// panel pins it to the top-left margin.
fn reorder_panel_origin(
    origin: (f32, f32),
    window: (f32, f32),
    rows: usize,
    list_max_h: f32,
    rem: f32,
) -> (f32, f32) {
    let m = REORDER_PANEL_MARGIN;
    let scale = (rem / 16.0).max(1.0);
    let panel_h =
        (rows as f32 * REORDER_ROW_H * scale).min(list_max_h) + REORDER_PANEL_CHROME_H * scale;
    let x = origin.0.min(window.0 - REORDER_PANEL_W - m).max(m);
    let y = origin.1.min(window.1 - panel_h - m).max(m);
    (x, y)
}

/// Whether `a` and `b` hold the same items, ignoring order.
fn same_members<T: PartialEq>(a: &[T], b: &[T]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.contains(x))
}

/// Move task `dragged` to `onto`'s position in the task list on disk (see
/// [`reorder_base`]), save it, and return the store as saved. `Ok(None)` when
/// nothing changed (same or unknown ids). An unreadable file is an error and
/// is left untouched rather than overwritten.
fn move_task_on_disk(
    current: &TaskStore,
    dragged: &str,
    onto: &str,
) -> std::io::Result<Option<TaskStore>> {
    let mut fresh = TaskStore::load(current.path())?;
    if fresh.tasks().is_empty() {
        fresh = current.clone();
    }
    if !fresh.move_task(dragged, onto) {
        return Ok(None);
    }
    fresh.save()?;
    Ok(Some(fresh))
}

/// Which sidebar list a reorder panel (and a row dragged in it) belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReorderList {
    Projects,
    Tasks,
}

/// An agent row being dragged in Settings › Agents: its catalog key.
pub(crate) struct DraggedAgentRow {
    key: String,
}

/// A row being dragged in a reorder panel: its list, and the project name or
/// task id that identifies it.
pub(crate) struct DraggedReorderRow {
    list: ReorderList,
    key: String,
}

/// `items` with `item` swapped with its neighbour above (`up`) or below.
/// At the edge, or for an unknown item, the order is unchanged.
fn shifted<T: PartialEq + Clone>(items: &[T], item: &T, up: bool) -> Vec<T> {
    let mut out = items.to_vec();
    let Some(i) = out.iter().position(|x| x == item) else {
        return out;
    };
    let j = if up {
        i.checked_sub(1)
    } else {
        Some(i + 1).filter(|j| *j < out.len())
    };
    if let Some(j) = j {
        out.swap(i, j);
    }
    out
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

impl Tab {
    /// Classify this tab for persistence (`panes_state`). `TabContent` holds
    /// live entities that can't be matched by kind ergonomically at the call
    /// site, so `save_panes` goes through this instead.
    fn kind_tag(&self) -> Option<crate::panes_state::TabRecordKind> {
        use crate::panes_state::TabRecordKind as K;
        Some(match &self.content {
            TabContent::File { .. } => return None,
            TabContent::ReviewOverview(overview) => K::ReviewOverview(overview.last_report.clone()),
            TabContent::Chat(_) => K::Chat,
            TabContent::Diff => K::Diff,
            TabContent::Editor(_) => K::Editor,
            TabContent::Terminal(_) => match &self.agent {
                Some(a) => K::Agent(a.clone()),
                None => K::Shell,
            },
        })
    }
}

/// Prefix on a notification's navigation id that names a project's main
/// checkout rather than a registry worktree (see `alert_target`).
pub(crate) const MAIN_NAV_PREFIX: &str = "main:";

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Selection {
    /// Projects foot — main checkout.
    Main(String),
    /// Legacy single worktree id (still used when resolving members).
    Worktree(String),
    /// Task-centric selection. `member_id` is the focused member worktree id —
    /// agent, shell, and editor all use that member's worktree (home when `None`).
    Task {
        task_id: String,
        member_id: Option<String>,
    },
}

/// What an active inline edit targets. The text itself lives in the backing
/// `Editor` (full caret/selection/clipboard editing), not here — this only
/// carries which thing is being edited so the commit knows where to write.
#[derive(Clone)]
pub(crate) enum EditKind {
    /// Rename worktree or task `id`.
    Rename(String),
    /// Edit an agent's launch command in settings (kind string).
    AgentCmd(String),
    /// Name a new custom agent in settings. Commit inserts a custom
    /// `AgentPref` keyed by the entered slug.
    NewAgent,
    /// Settings → Remote's HTTPS port.
    RemotePort,
}

/// An inline edit in progress: what it targets, and the single-line `Editor`
/// backing it. Replacing the old hand-rolled append-only field, this brings
/// real text editing — caret motion, selection, ⌘A/C/V/X, IME.
struct ActiveEdit {
    kind: EditKind,
    editor: Entity<Editor>,
}

/// Centered "New task" dialog: name, brief, optional branch, project members.
struct NewTaskDialog {
    focus: ui::ModalFocus,
    name: Entity<Editor>,
    name_required: bool,
    /// Multi-line initial prompt injected into agent context on launch.
    brief: Entity<Editor>,
    branch: Entity<Editor>,
    /// Last auto-branch string pushed into the branch field's placeholder, so
    /// we only call `set_placeholder_text` when the name's slug changes.
    auto_branch_shown: String,
    /// Refreshes the form's accessible values and auto-branch placeholder.
    _field_subs: [gpui::Subscription; 3],
    /// `Some(repo)` when opened from a PROJECTS row's `+`: the dialog is a
    /// "New worktree" form for that one project — no member picker, no Home,
    /// worktree wording. Under the hood it is still a one-member task, so the
    /// agent, branch and setup hooks behave exactly as for any task.
    worktree_for: Option<String>,
}

/// Steps in the post-create "start agents" guide for multi-member tasks.
enum StartSpecialistsStep {
    /// Single yes/no: open an agent in every member project?
    AskOpenAll,
    /// Which enabled agent to launch everywhere.
    PickAgent { selected: AgentKind },
}

/// After creating a multi-member task, guide opening agents across projects.
struct StartSpecialistsDialog {
    task_id: String,
    step: StartSpecialistsStep,
}

/// Live A2A broker for one openOrchestra task (multi-member only).
struct TaskBrokerInfo {
    base_url: String,
    api_key: String,
    handle: a2a_broker::BrokerHandle,
}

/// Which splitter is currently being dragged (§3.4: panels are resizable).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Drag {
    /// Left worktree sidebar width.
    Sidebar,
    /// Right diff panel width. Grows leftward, so the delta is negated.
    DiffPanel,
    /// File-list column width inside the two-pane diff surface.
    DiffList,
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

/// What the diff panel measures against.
///
/// Both options end at the working tree; they differ in where the range starts.
/// The panel used to hard-code the second one, which meant a task's own commits
/// were mixed in with its uncommitted edits and nothing on screen said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiffBase {
    /// `HEAD` → working tree: what you have changed and not committed.
    #[default]
    Uncommitted,
    /// Merge-base with the repo's default branch → working tree: everything
    /// this branch changes, which is what a PR would show.
    DefaultBranch,
}

impl DiffBase {
    /// Menu label. `compare_ref` names the real ref rather than a fixed
    /// "main", so a repo with no remote reads `vs master`, not `vs
    /// origin/master`.
    pub(crate) fn label(self, compare_ref: &str) -> String {
        match self {
            Self::Uncommitted => "Uncommitted".to_string(),
            Self::DefaultBranch => format!("vs {compare_ref}"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct CardMenu {
    id: Option<String>,
    pr: Option<(u64, String)>,
    x: Pixels,
    y: Pixels,
}

/// A pending "you tried to edit a diff line — where should it open?" question.
pub(crate) struct EditPrompt {
    file: String,
    /// The working-file line to land on, or `None` when the row was a deletion
    /// with nothing after it to land on.
    line: Option<u32>,
}

/// State of the "keep or close your terminals?" prompt shown on quit.
pub(crate) struct ClosePrompt {
    /// The "remember my choice" checkbox — when set, the chosen action is
    /// persisted to `Prefs::close_terminals_on_exit` so the prompt is skipped
    /// next time.
    remember: bool,
    /// How many live openOrchestra tmux sessions the choice affects, shown so
    /// "close" isn't a blind action.
    session_count: usize,
}

struct PaneSet {
    tabs: Vec<Tab>,
    /// Id of the active tab, not an index — indices shift when a tab closes.
    active: u64,
    /// Monotonic counter, bumped on every touch, for LRU eviction (M5b).
    last_used: u64,
}

impl PaneSet {
    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active)
    }

    fn remove_tab_at(&mut self, index: usize) {
        if self.tabs.remove(index).id == self.active {
            self.active = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map_or(0, |tab| tab.id);
        }
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
    /// Agent / shell cwd — the focused task member (or main / single worktree).
    path: PathBuf,
    /// Editor project root — same as `path` for task members (per-context tabs).
    editor_path: PathBuf,
    /// Which agent this entry runs, or `None` for the read-only `main`
    /// checkout, which gets a plain shell (D8).
    agent_kind: Option<AgentKind>,
    compare_ref: String,
    branch: String,
    /// When set, DIFF is the unified multi-member changeset for this task.
    task_id: Option<String>,
}

pub struct HarnessWorkspace {
    manager: Manager<SystemRunner>,
    task_store: TaskStore,
    /// `~/.openorchestra` — hooks, tasks scripts, etc.
    data_dir: PathBuf,
    data: WorkspaceData,
    repo_roots: Vec<PathBuf>,
    repos_path: PathBuf,
    selection: Selection,
    panes: HashMap<String, PaneSet>,
    /// Per-worktree hosted Zed Workspace used by Files/Git tools and as the
    /// engine behind file harness tabs. Not itself a user-facing tab.
    service_workspaces: HashMap<String, Entity<::workspace::Workspace>>,
    /// Per-worktree full-editor workspaces (the Editor tab), keyed like
    /// `service_workspaces` and sharing that workspace's project.
    ide_workspaces: HashMap<String, Entity<::workspace::Workspace>>,
    /// Watches the open ⌘P finder for its dismissal so focus can be handed
    /// back (see `watch_finder_dismiss`). Replaced on every open: zed keeps
    /// the finder entity alive to reopen it, so accumulating subscriptions
    /// would fire once per past open.
    finder_dismiss_sub: Option<gpui::Subscription>,
    /// A liveness probe (`ps` + tmux) is in flight; don't stack another.
    liveness_pending: bool,
    /// Where the last preview file tab sat when zed closed it, as
    /// `(worktree key, index)`. Zed replaces a preview by removing the old
    /// item *before* adding the new one, so by the time the new preview
    /// arrives its predecessor is already gone; this keeps the slot so the
    /// new tab lands where the old one was instead of at the end.
    preview_slot: Option<(String, usize, std::time::Instant)>,
    /// Horizontal scroll of the center tab strip, and the tab it last brought
    /// into view — scrolled only when the active tab *changes*, so a user
    /// scrolling the strip by hand is not snapped back every frame.
    tab_scroll: gpui::ScrollHandle,
    tab_scrolled_to: std::cell::Cell<u64>,
    /// Explorer vs Search inside the Files tools tab (session; not persisted).
    files_sub_tab: FilesSubTab,
    /// Query editor + results for Files → Search. Created lazily.
    project_search: Option<ProjectSearchUi>,
    diff: worktrees::Diff,
    /// Multi-member task changeset (when selection is a Task).
    unified_diff: UnifiedDiff,
    /// Repos included when creating a new task (order: first = default home).
    new_task_repos: Vec<String>,
    /// Home repo for the pending task create (must be in `new_task_repos`).
    new_task_home: String,
    /// Files whose diff is expanded. A set, not a single selection: reviewing
    /// usually means comparing two files, and collapsing one to look at
    /// another loses your place in the first.
    expanded: std::collections::HashSet<String>,
    /// The file whose diff the right column shows, in the two-pane layout.
    ///
    /// Separate from `expanded` on purpose. The accordion needs a SET — reviewing
    /// usually means comparing two files, and collapsing one to open another
    /// loses your place in the first. Two-pane needs exactly ONE. Sharing a
    /// field would have cost the accordion its multi-expand; keeping both means
    /// neither layout has to be reconciled when the other is in use.
    diff_focus: Option<String>,
    /// Active project-hooks editor context (repo name, layer, phase). Drives
    /// the Repo/Local · Setup/Teardown chrome above the hosted editor.
    hooks_edit: Option<(String, HookLayer, HookScriptPhase)>,
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
    settings_focus: ui::ModalFocus,
    /// Which Settings pane is showing. Not persisted: reopening Settings should
    /// land where the page starts, not where you happened to leave it.
    settings_section: SettingsSection,
    review_model_catalogs: std::collections::BTreeMap<ReviewCli, ModelCatalog>,
    /// The requirements screen is open. Not dismissible while a Required tool
    /// is missing — see `preflight::blocked`.
    preflight_open: bool,
    /// Last probe result. Re-run by the screen's Re-check button, so installing
    /// a tool does not mean restarting the app.
    preflight: Vec<crate::preflight::Check>,
    /// The conductor pet: message shown, expiry, and which beat of the
    /// two-pose arm cycle it is on.
    /// The always-on conductor (`Prefs::conductor_always_on`): his state,
    /// what he showed last tick, his rendered frames and the accent they were
    /// rendered in, whether his tick loop runs, and a press on him.
    conductor: crate::conductor::Conductor,
    conductor_view: Option<crate::conductor::View>,
    conductor_frames: Option<Vec<Arc<gpui::RenderImage>>>,
    conductor_frames_accent: Option<[f32; 3]>,
    conductor_ticking: bool,
    /// The pointer is over the conductor or his bubble: show it in full.
    conductor_hovered: bool,
    conductor_drag: Option<ui::conductor::ConductorDrag>,
    /// Shared clock for the sidebar's agent indicators (spinner angle, pulse
    /// phase), and whether the loop that repaints them is running.
    anim_clock: std::time::Instant,
    indicator_ticking: bool,
    /// A press on the top bar that becomes a window move once the pointer
    /// moves — deferred so clicks on the bar's own buttons still land.
    titlebar_should_move: bool,
    /// A shortcut row waiting for its new chord (`shortcuts::key_id`).
    rebinding: Option<String>,
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
    /// Agent tabs whose terminal or tmux session is still starting.
    pending_agent_launches: std::collections::HashSet<u64>,
    /// Persisted open-tab records for reattaching tmux sessions on reopen.
    panes_state: crate::panes_state::PanesState,
    panes_path: PathBuf,
    /// Whether `tmux` is available; when false, sessions are not persistent.
    has_tmux: bool,
    /// The active inline edit, if any. `Editor`-backed, so it carries its own
    /// focus, caret, and selection — no more hand-rolled caret blink.
    edit: Option<ActiveEdit>,
    /// Centered new-task dialog (description, branch, members).
    new_task: Option<NewTaskDialog>,
    /// Guide starting per-member agents after a multi-member task create.
    start_specialists: Option<StartSpecialistsDialog>,
    /// Localhost A2A brokers keyed by openOrchestra task id.
    task_brokers: HashMap<String, TaskBrokerInfo>,
    /// Coalesced wake-ups when any task broker's A2A task state changes.
    a2a_change_tx: crossbeam_channel::Sender<()>,
    /// Inbox deliveries for tmux inject into specialist agents.
    a2a_inbox_tx: crossbeam_channel::Sender<a2a_broker::InboxDelivery>,
    /// Queued inbox messages awaiting a live Agent session (keyed by pane key).
    a2a_pending_inject: HashMap<String, Vec<a2a_broker::InboxMessage>>,
    /// Task pane keys whose agent should be running but is not (computer
    /// restart, agent quit). Refreshed off-thread every few seconds; drives
    /// the STOPPED pill and its Restart.
    missing_agents: std::collections::BTreeSet<String>,
    /// Pane keys where a Restart already tried continuing the conversation;
    /// if the agent is still missing, the next Restart starts it fresh.
    restart_tried: std::collections::HashSet<String>,
    /// One-shot: the next agent command continues its last conversation.
    continue_next_agent: bool,
    /// Recent A2A snapshots and events per task, for the sidebar's
    /// coordinator → repo connector animation.
    a2a_links: ui::sidebar::links::LinkTracker,
    new_target_repo: String,
    menu: Option<CardMenu>,
    /// The ⋯ menu on a project header: repo name and where it opened.
    project_menu: Option<(String, Pixels, Pixels)>,
    /// The open reorder panel (PROJECTS or TASKS header ⇅), and where it opened.
    reorder_panel: Option<(ReorderList, Pixels, Pixels)>,
    /// Open "new tab" menu, anchored under the `+` in the tab strip.
    tab_menu: Option<Pixels>,
    /// Right-click menu on a Shell/Agent tab: the tab id and where it opened.
    tab_context_menu: Option<(u64, Pixels)>,
    /// Open diff-base menu, anchored under its button in the diff header.
    diff_base_menu: Option<gpui::Point<Pixels>>,
    /// The quit confirmation is open (keep vs. close terminals). Present only
    /// while the user is deciding; carries the "remember my choice" checkbox.
    close_prompt: Option<ClosePrompt>,
    /// The open "open this file to edit it?" question, raised when the user
    /// tries to type into a diff.
    edit_prompt: Option<EditPrompt>,
    /// Tasks (or lone worktrees) whose archive/delete is running off-thread.
    ending: HashMap<String, lifecycle::EndKind>,
    /// An archive/delete waiting on its loss check or the user's answer.
    end_confirm: Option<lifecycle::EndConfirm>,
    /// Setup paused until the user approves these repos' committed hooks.
    hook_trust: Option<HookTrustPrompt>,
    /// The latest status message ("archived ‘x’", "push failed: …"), shown
    /// as a toast until [`NOTE_TTL`] after `note_at`. Set it with `set_note`.
    note: String,
    note_at: Option<std::time::Instant>,
    forge_status: HashMap<PathBuf, HashMap<String, BranchStatus>>,
    agent_status: HashMap<PathBuf, AgentState>,
    /// oo-remote: marks cache, phone server lifecycle, keep-awake.
    remote: remote::RemoteRuntime,
    _poller: Option<Poller>,
    /// One watcher for every StatusFile-signalled worktree (pi, Claude Code).
    status_source: Option<StatusFileSource>,
    /// Watches the selected root's files so the diff panel keeps up with an
    /// agent's edits in near real time (retargeted on every selection change).
    diff_watch: crate::diff_watch::DiffWatcher,
    /// Watches every repo's `.git` so the sidebar's ahead/behind/dirty badges
    /// follow git itself, not just the structural mutations `refresh()` runs on.
    git_watch: crate::git_watch::GitWatcher,
    /// A live status recompute is in flight. Two overlapping passes would spawn
    /// twice the git processes to agree on the same numbers, and the watcher can
    /// fire far faster than git can answer.
    status_pending: bool,
    /// A recompute was asked for while one was in flight. Dropping that request
    /// is how badges went stale: the pass in flight had already read git
    /// *before* the event that asked again, so its answer predates it, and
    /// nothing re-asked until the 10 s backstop — or never, if the backstop
    /// itself landed mid-flight. Set instead of dropped, and run once more when
    /// the current pass lands.
    status_rerun: bool,
    /// The tools panel fills the center's space (⤢ in its tab strip, or a
    /// double-click on its splitter). Session-only on purpose: launching into
    /// a window with the terminals hidden would read as the app having lost
    /// them. The center is not rendered meanwhile, so terminals keep their
    /// size rather than being squeezed to zero columns.
    tools_maximized: bool,
    /// The selected root's standing against its own tracking branch, tagged with
    /// the path it was measured in. Tagged rather than bare because the answer
    /// arrives asynchronously and the user may have selected another worktree by
    /// then — a push count from the previous selection is worse than none.
    push_state: Option<(PathBuf, worktrees::PushState)>,
    /// The push control is armed: the next click pushes. Cleared by selecting
    /// anything else, so an armed button never survives the branch it was for.
    push_armed: bool,
    /// A push is running. The control shows it and stops accepting clicks —
    /// git would reject the second one anyway, but only after the user has been
    /// given two "pushing…" states to interpret.
    pushing: bool,
    /// A push-state probe is in flight (same reasoning as `status_pending`).
    push_probe_pending: bool,
    /// Parsed diff rows per file, for the life of the current diff. Rendering
    /// takes `&self`, hence the `RefCell`; the view is single-threaded.
    ///
    /// Bodies are fetched one file at a time now, so an entry also has to be
    /// able to say "asked for, not here yet" — without that state every frame
    /// of the wait would spawn another `git diff` for the same file.
    row_cache: std::cell::RefCell<HashMap<String, FileRows>>,
    /// Where the adapters materialize their extensions and shims.
    state_dir: PathBuf,
    prefs: Prefs,
    prefs_path: PathBuf,
    drag: Option<DragState>,
    bus: Bus,
    focus_handle: FocusHandle,
    /// Focus target for the hosted-editor content wrapper. That wrapper carries
    /// the `"Workspace"` key context and the workspace action handlers, so
    /// focusing it makes ⌘P / ⌘⇧F resolve even when no file is open — otherwise
    /// focus drifts to the `"Harness"` root, which has neither, and the
    /// keystrokes silently no-op.
    editor_host_focus: FocusHandle,
    /// Focus target for the diff surface. Needed because the surface listens for
    /// keystrokes — typing over a selected line is one of the two ways to say
    /// "I want to edit this" — and gpui delivers key events only along the
    /// focus path. Without it a Diff tab would take focus nowhere and swallow
    /// nothing, which reads as the trigger simply not working.
    diff_surface_focus: FocusHandle,
    /// Scroll state of the changed-files list, so ↑/↓ can keep the selected
    /// file in view.
    diff_files_scroll: gpui::ScrollHandle,
    /// Latest resource sample driving the sidebar band; `None` until the first
    /// tick lands (and while CPU has no delta yet).
    resource: Option<crate::resource_monitor::Snapshot>,
    /// Rolling trail of recent totals, drawn as the band's sparklines.
    resource_history: crate::resource_monitor::History,
    /// Latest listening-ports sample for the Ports tool tab; `None` until the
    /// tab is first shown. Sampled only while the tab is visible.
    ports_sample: Option<crate::ports::PortsSample>,
    /// Ports scans are numbered as they start; `ports_seq_applied` is the
    /// newest one shown, so an older scan finishing late is dropped.
    ports_seq_issued: u64,
    ports_seq_applied: u64,
    /// One Ports scan at a time (`ports::ScanGate`).
    ports_gate: crate::ports::ScanGate,
    /// When `ports_sample` was taken, and whether the last scan failed: an
    /// old sample is shown as updating, never as current.
    ports_sample_at: Option<std::time::Instant>,
    ports_scan_failed: bool,
    /// Which Ports sort dropdown is open, if any.
    ports_sort_menu: Option<crate::workspace::ui::ports_panel::SortMenu>,
    /// The last press on a sort dropdown's button (which menu, where), so the
    /// open menu's outside handler leaves that press to the button
    /// (`ports_panel::dismisses`).
    ports_sort_trigger_press: Option<(
        crate::workspace::ui::ports_panel::SortMenu,
        gpui::Point<Pixels>,
    )>,
    /// Project roots the user archived — hidden from the active list but kept so
    /// they can be restored. Persisted to `archived.json`; never deletes files.
    archived_repos: Vec<PathBuf>,
    archived_path: PathBuf,
    /// A removal waiting on the confirm dialog — so a stray click can't drop
    /// a whole project.
    confirm_removal: Option<Removal>,
}

/// Repos whose committed hooks need approval before setup runs:
/// (display name, repo root, scripts that would run).
#[derive(Clone, Debug)]
pub(crate) struct HookTrustPrompt {
    pub repos: Vec<(String, PathBuf, Vec<PathBuf>)>,
}

/// What the remove-confirmation dialog is asking about.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Removal {
    /// An active project, by display name.
    Project(String),
    /// An entry in the archived list, by root.
    Archived(PathBuf),
}

/// How many samples the sidebar sparkline retains (≈2 min at the 2 s cadence).
const RESOURCE_HISTORY: usize = 60;

// The chrome view's methods are split across the `workspace/` module by
// feature area; each file is an `impl HarnessWorkspace` block. Only the
// constructor, shared types, the Render/Focusable roots, and free
// helpers live here.
mod actions;
mod agents;
mod construct;
mod diff_rows;
mod navigation;
mod prefs;
mod render;
pub(crate) use agents::*;
pub(crate) use diff_rows::*;
pub(crate) use prefs::*;
pub(crate) use render::*;
mod a2a;
mod agent_restart;
mod coordinator;
pub(crate) mod lifecycle;
pub(crate) mod remote;
mod state;
mod ui;

/// One matching line in the Files → Search panel.
#[derive(Debug, Clone)]
pub(crate) struct ProjectSearchHit {
    /// 1-based line number (same convention as the diff gutter / `open_file_at`).
    pub line: u32,
    /// The full line text (trimmed of trailing newline).
    pub text: String,
}

/// Matches for one file, keyed by worktree-relative path.
#[derive(Debug, Clone)]
pub(crate) struct ProjectSearchFile {
    pub path: String,
    pub hits: Vec<ProjectSearchHit>,
}

/// Live state for the harness project-search panel (not Zed's DeploySearch tab).
struct ProjectSearchUi {
    query: Entity<Editor>,
    _query_sub: gpui::Subscription,
    /// Bumped on every new query so a slow prior search is discarded.
    generation: u64,
    results: Vec<ProjectSearchFile>,
    searching: bool,
    /// Soft status: "Searching…", "No matches", limit note, etc.
    status: String,
}

fn now_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
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
        // Cap width so longer help (new-task info) wraps instead of spanning
        // the whole window; short tips stay compact because content shrinks.
        div()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .rounded_md()
            .px_1p5()
            .py_0p5()
            .max_w(px(280.0))
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
mod slugify_tests {
    use super::task_name_slug;

    #[test]
    fn slugify_makes_feat_branch_placeholders() {
        assert_eq!(task_name_slug("fix login bug"), "fix-login-bug");
        assert_eq!(task_name_slug(""), "task");
        assert_eq!(task_name_slug("  Hello, World!  "), "hello-world");
    }
}
