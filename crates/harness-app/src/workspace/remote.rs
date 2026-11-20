//! oo-remote glue: the snapshot of marked tabs the phone server publishes,
//! the server's lifecycle (start/stop off the GPUI thread, `OtherBuild`
//! retry), keep-awake, and the `remote.json` watcher.
//!
//! Threading: `RemoteServer::start`/`stop` block (they own a tokio runtime),
//! so they run on the background executor and hand results back through the
//! entity. The GPUI thread only builds snapshots (cheap), reads/writes
//! `remote.json` through the store (one small file), and spawns/kills the
//! `caffeinate` child. One bounded exception, at quit/teardown only: while a
//! phone has "Fit to phone" on, `shutdown_detached` restores those window
//! sizes synchronously (argv tmux, at most `QUIT_RESTORE`), because the
//! process may exit before the stop thread gets to it and tmux sessions
//! outlive the app. With no active fit it spawns nothing.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use agent_runtime::AgentState;
use oo_remote::{
    AgentInfo, KeepAwake, RemoteFile, RemoteServer, ServerConfig, ServerStatus, Snapshot,
    StartError, Store, Tmux, auth, snapshot_channel,
};

use super::*;

/// Plain-HTTP setup port (served only while "Add a device" is open).
pub(crate) const SETUP_PORT: u16 = 7080;
/// Watcher/refresh poll cadence; also the debounce for `remote.json` events.
const TICK: Duration = Duration::from_millis(250);
/// Snapshot + status refresh every this many ticks (1 s).
const REFRESH_TICKS: u32 = 4;
/// While another build holds the port, retry every this many ticks (30 s).
const RETRY_TICKS: u32 = 120;
/// Upper bound on the synchronous fit restore at quit (see module docs).
const QUIT_RESTORE: Duration = Duration::from_secs(1);
/// How long a device's "Confirm revoke" stays armed.
pub(crate) const REVOKE_CONFIRM: Duration = Duration::from_secs(4);

/// Whether this app instance is serving phones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RemoteServe {
    Off,
    Serving,
    /// The port is held by the other openOrchestra build (Dev vs release share
    /// `~/.openorchestra`); retried every 30 s so this one takes over when it
    /// quits.
    OtherBuild,
    Error(String),
}

/// Settings → Remote panel state. Transient, never persisted.
#[derive(Default)]
pub(crate) struct RemoteUi {
    /// "Add a device" is open; the setup port listens while it is, and a
    /// server (re)started meanwhile opens it too.
    pub setup_open: bool,
    /// `http://<primary private IP>:7080/setup` and its QR modules, fixed when
    /// the panel opens. `None` when this Mac has no private IPv4 address.
    pub setup_url: Option<String>,
    pub setup_qr: Vec<Vec<bool>>,
    /// The pairing code on show and when it was issued.
    pub code: Option<(String, Instant)>,
    pub code_error: Option<String>,
    /// The device whose Revoke was clicked once, and when.
    pub revoke_armed: Option<(String, Instant)>,
    /// Why the last Port entry was refused.
    pub port_error: Option<String>,
    /// Device ids already paired when "Add a device" opened: a device that
    /// appears beyond these means a phone just paired, so the panel closes.
    pub setup_known: Vec<String>,
    /// "Listening on" shows every address, not just the main connection's.
    pub show_all_urls: bool,
}

/// The first device in `devices` that isn't in `known` — a phone that paired
/// while "Add a device" was open.
pub(crate) fn newly_paired<'a>(
    known: &[String],
    devices: &'a [oo_remote::Device],
) -> Option<&'a oo_remote::Device> {
    devices.iter().find(|d| !known.contains(&d.id))
}

/// What `remote_reconcile` should do with the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReconcileAction {
    Nothing,
    Start,
    Stop,
    /// Running on a port other than `remote.json`'s: stop, then start anew.
    Restart,
}

/// The reconcile decision, pure. `running` is the live server's port; `busy`
/// is a start or stop in flight (it calls back into reconcile when done).
fn reconcile_action(
    enabled: bool,
    running: Option<u16>,
    want_port: u16,
    busy: bool,
    serve: &RemoteServe,
) -> ReconcileAction {
    match (enabled, running) {
        (false, Some(_)) => ReconcileAction::Stop,
        (false, None) => ReconcileAction::Nothing,
        (true, Some(port)) if port != want_port && !busy => ReconcileAction::Restart,
        (true, Some(_)) => ReconcileAction::Nothing,
        (true, None) if !busy && *serve == RemoteServe::Off => ReconcileAction::Start,
        (true, None) => ReconcileAction::Nothing,
    }
}

/// The steady level when remote is off.
pub(crate) const REMOTE_DOT_OFF: f32 = 0.45;
/// Repaint cadence for the fade — the same ~15 fps the spinners use, so the
/// fade reads smooth rather than stepped.
const REMOTE_BLINK_FRAME: std::time::Duration = crate::workspace::render::INDICATOR_FRAME;

/// Opacity of a marked tab's remote dot at clock time `t`: while a phone can
/// reach it (`live`) it fades smoothly out and back in (cosine ease, full at
/// the start of each period, faintest at the middle); steady and dimmed
/// otherwise.
pub(crate) fn remote_dot_opacity(live: bool, t: f32) -> f32 {
    if !live {
        return REMOTE_DOT_OFF;
    }
    dot_blink_opacity(t)
}

pub(crate) struct RemoteRuntime {
    signals: HashMap<(PathBuf, String), (AgentState, SystemTime, Option<String>)>,
    /// The blink repaint loop is running.
    pub blinking: bool,
    pub store: Store,
    /// UI cache of `remote.json`, reloaded after our own writes and on every
    /// watcher event. Never written back: every change goes through `store`.
    pub file: RemoteFile,
    pub serve: RemoteServe,
    pub status: Option<ServerStatus>,
    pub ui: RemoteUi,
    /// Device ids seen at the last `remote.json` read (`None` before the
    /// first), to notice a device pairing.
    known_devices: Option<HashSet<String>>,
    state_dir: PathBuf,
    server: Option<RemoteServer>,
    snapshot_tx: tokio::sync::watch::Sender<Snapshot>,
    /// Read-only copy of `panes.json` (with its mtime) for marks made in the
    /// other build; re-read only while such marks exist and the file changed.
    disk_panes: Option<(SystemTime, crate::panes_state::PanesState)>,
    keep_awake: Option<KeepAwake>,
    /// Newest transcript per (worktree root, agent kind), from adapter signals
    /// and the registry at startup; forgotten when a tab of that kind opens
    /// or closes there.
    pub(crate) session_files: HashMap<(PathBuf, String), PathBuf>,
    /// The store's file watcher (type-erased: two `notify` versions are in
    /// the graph) and its change events, taken by the poll loop.
    _watcher: Option<Box<dyn std::any::Any>>,
    changes: Option<crossbeam_channel::Receiver<()>>,
    /// A start/stop is running on the background executor.
    starting: bool,
    stopping: bool,
}

impl RemoteRuntime {
    /// Environment for a new shell tab's tmux session: loads the zsh hook
    /// that logs each command for the phone (`oo_remote::shell`). Empty when
    /// its startup files can't be written; the tab is then Terminal-only.
    pub(crate) fn shell_env(&self, session: &str) -> Vec<(String, String)> {
        match oo_remote::shell::install(&self.state_dir) {
            Ok(zdotdir) => oo_remote::shell::session_env(
                &zdotdir,
                &oo_remote::shell::log_path(&self.state_dir, session),
            ),
            Err(e) => {
                log::warn!("remote: shell integration unavailable: {e}");
                Vec::new()
            }
        }
    }

    pub(crate) fn new(state_dir: &Path) -> RemoteRuntime {
        let store = Store::new(state_dir);
        let file = store.load();
        let (watcher, changes) = match store.watch() {
            Ok((w, rx)) => (Some(Box::new(w) as Box<dyn std::any::Any>), Some(rx)),
            Err(e) => {
                log::warn!("remote: cannot watch {}: {e}", store.path().display());
                (None, None)
            }
        };
        let (snapshot_tx, _) = snapshot_channel();
        RemoteRuntime {
            signals: HashMap::new(),
            blinking: false,
            store,
            file,
            serve: RemoteServe::Off,
            status: None,
            ui: RemoteUi::default(),
            known_devices: None,
            state_dir: state_dir.to_path_buf(),
            server: None,
            snapshot_tx,
            disk_panes: None,
            keep_awake: None,
            session_files: HashMap::new(),
            _watcher: watcher,
            changes,
            starting: false,
            stopping: false,
        }
    }

    /// Stop the server (if any) on a detached std thread and drop
    /// keep-awake. `RemoteServer::stop` (and its `Drop`) wait up to ~5 s for
    /// sockets to flush, so that never blocks the caller; if the process
    /// exits while that thread runs, the OS closes the sockets. Windows a
    /// phone fitted are restored first, synchronously but bounded by
    /// `QUIT_RESTORE` (instant when nothing is fitted): the quit paths call
    /// `cx.quit()` right after this.
    pub(crate) fn shutdown_detached(&mut self) {
        self.keep_awake = None;
        self.status = None;
        if let Some(server) = self.server.take() {
            server.restore_fits_now(QUIT_RESTORE);
            let spawned = std::thread::Builder::new()
                .name("oo-remote-stop".into())
                .spawn(move || server.stop());
            if let Err(e) = spawned {
                // No thread: leak rather than block the main thread.
                log::warn!("remote: stop thread failed to spawn: {e}");
            }
        }
    }

    /// A start or stop is in flight.
    pub(crate) fn busy(&self) -> bool {
        self.starting || self.stopping
    }

    /// Remember the transcript an adapter reported for `root`.
    pub(crate) fn record_session(&mut self, root: &Path, kind: &str, file: &Path) {
        self.session_files
            .insert((root.to_path_buf(), kind.to_string()), file.to_path_buf());
    }

    /// An agent tab of `kind` opened or closed in `root`: the file on record
    /// belongs to another session, so a new tab shows Terminal until its own
    /// first signal records a fresh one.
    pub(crate) fn forget_session(&mut self, root: &Path, kind: &str) {
        self.signals.remove(&(root.to_owned(), kind.to_owned()));
        self.session_files
            .remove(&(root.to_path_buf(), kind.to_string()));
    }

    pub(crate) fn record_signal(
        &mut self,
        root: &Path,
        kind: &str,
        state: AgentState,
        tool: Option<String>,
    ) {
        let entry = self
            .signals
            .entry((root.to_owned(), kind.to_owned()))
            .or_insert_with(|| (state, SystemTime::now(), None));
        if entry.0 != state {
            entry.1 = SystemTime::now();
        }
        entry.0 = state;
        entry.2 = tool;
    }

    fn reload(&mut self) {
        self.file = self.store.load();
    }

    /// Hold `caffeinate` iff serving with keep-awake on.
    fn sync_keep_awake(&mut self) {
        let want = self.serve == RemoteServe::Serving && self.file.keep_awake;
        if want && self.keep_awake.is_none() {
            match KeepAwake::start() {
                Ok(k) => self.keep_awake = Some(k),
                Err(e) => log::warn!("remote: keep-awake failed: {e}"),
            }
        } else if !want {
            self.keep_awake = None;
        }
    }
}

impl Drop for RemoteRuntime {
    /// The runtime drops with the workspace on the GPUI main thread (quit,
    /// window close, app teardown): hand a live server to a std thread.
    fn drop(&mut self) {
        self.shutdown_detached();
    }
}

/// The tab context menu's remote item for a tab kind: its label and, for a
/// shell, a warning line. `None` for tabs that cannot be remote-controlled.
pub(crate) fn remote_menu_item(
    kind: &TabKind,
    marked: bool,
) -> Option<(&'static str, Option<&'static str>)> {
    let label = if marked {
        "Stop remote control"
    } else {
        "Allow remote control"
    };
    match kind {
        TabKind::Shell => Some((label, Some("Gives paired devices this shell"))),
        TabKind::Agent(_) => Some((label, None)),
        TabKind::Chat
        | TabKind::Diff
        | TabKind::Editor
        | TabKind::Browser
        | TabKind::ReviewOverview => None,
    }
}

/// Whether any tab of the worktree with pane key `key` is marked.
pub(crate) fn worktree_has_marks(file: &RemoteFile, key: &str) -> bool {
    file.marked.iter().any(|m| m.worktree_key == key)
}

/// The pane key of a registry worktree: task members use the broker's
/// member key, plain worktrees their id. Same key `remote_sources` uses.
pub(crate) fn worktree_pane_key(task_id: Option<&str>, id: &str) -> String {
    match task_id {
        Some(tid) => a2a_broker::task_member_pane_key(tid, id),
        None => id.to_string(),
    }
}

/// Status message after a toggle: marking while remote is off is accepted but
/// unreachable until the user turns it on; while the other build serves,
/// phones reach the tab through that build.
pub(crate) fn mark_note(
    enabled: bool,
    other_build: bool,
    now_marked: bool,
) -> Option<&'static str> {
    if !now_marked {
        return None;
    }
    if !enabled {
        return Some("Marked \u{2014} turn on Remote control in Settings to reach it");
    }
    other_build.then_some("Marked \u{2014} devices reach it through the other openOrchestra build")
}

/// What a click on the top-bar chip does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChipClick {
    /// Remote control is on: mark / unmark the tab.
    Toggle,
    /// Remote control is off: keep the tab marked (marking it if needed) and
    /// open Settings → Remote, where it can be turned on.
    OpenSettings { mark: bool },
}

pub(crate) fn chip_click(enabled: bool, marked: bool) -> ChipClick {
    if enabled {
        ChipClick::Toggle
    } else {
        ChipClick::OpenSettings { mark: !marked }
    }
}

/// The top-bar chip's state for the current tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteChip {
    Off,
    OnLive,
    OnUnreachable,
}

/// `None` when the tab isn't a terminal (nothing to mark).
pub(crate) fn remote_chip(is_terminal: bool, marked: bool, live: bool) -> Option<RemoteChip> {
    if !is_terminal {
        return None;
    }
    Some(match (marked, live) {
        (false, _) => RemoteChip::Off,
        (true, true) => RemoteChip::OnLive,
        (true, false) => RemoteChip::OnUnreachable,
    })
}

/// One terminal tab in Settings → Remote's "Agents on phone" list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteAgentRow {
    pub key: String,
    pub tab_id: u64,
    pub project: String,
    pub worktree: String,
    pub title: String,
    pub kind: Option<String>,
    pub marked: bool,
}

/// Every terminal tab in `sources`, in source order, with its mark.
pub(crate) fn remote_agent_rows(sources: &[MarkSource], file: &RemoteFile) -> Vec<RemoteAgentRow> {
    sources
        .iter()
        .flat_map(|s| {
            s.tabs.iter().map(move |(id, title, kind)| RemoteAgentRow {
                key: s.key.clone(),
                tab_id: *id,
                project: s.project.clone(),
                worktree: s.worktree.clone(),
                title: title.clone(),
                kind: kind.clone(),
                marked: file.mark_for(&s.key, *id).is_some(),
            })
        })
        .collect()
}

/// One worktree's terminal tabs, as the snapshot builder sees them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MarkSource {
    /// Pane key (`main:<repo>`, a worktree id, a task member / coordinator key).
    pub key: String,
    /// The `agent_status` key for this worktree.
    pub root: PathBuf,
    pub project: String,
    pub worktree: String,
    /// Git branch of this worktree; empty for a coordinator.
    pub branch: String,
    /// `(tab id, title, agent kind)`, terminal tabs only.
    pub tabs: Vec<(u64, String, Option<String>)>,
}

/// The marked tabs present in `sources`, with their worktree's status. Pure:
/// a mark whose tab isn't live or persisted here is left out, and the tmux
/// session is derived from the mark's key and tab, never sent by a phone.
pub(crate) fn build_snapshot(
    file: &RemoteFile,
    sources: &[MarkSource],
    signals: &HashMap<(PathBuf, String), (AgentState, SystemTime, Option<String>)>,
    session_files: &HashMap<(PathBuf, String), PathBuf>,
) -> Snapshot {
    // Open tabs per (worktree root, agent kind), marked or not: with two,
    // the one transcript on record could be either's, so both stay
    // Terminal-only.
    let mut open: HashMap<(&Path, &str), usize> = HashMap::new();
    for src in sources {
        for (_, _, kind) in &src.tabs {
            if let Some(k) = kind {
                *open.entry((src.root.as_path(), k.as_str())).or_default() += 1;
            }
        }
    }
    let mut agents = Vec::new();
    for src in sources {
        for (tab_id, _, kind) in &src.tabs {
            let Some(mark) = file.mark_for(&src.key, *tab_id) else {
                continue;
            };
            // Shell tabs are Terminal-only: which agent runs there is unknown.
            let key = kind
                .as_ref()
                .filter(|k| open.get(&(src.root.as_path(), k.as_str())) == Some(&1))
                .map(|k| (src.root.clone(), k.clone()));
            let transcript = key.as_ref().and_then(|k| session_files.get(k)).cloned();
            let (status, status_since, tool) = match key.as_ref().and_then(|k| signals.get(k)) {
                Some((state, at, tool)) if *state != AgentState::Unknown => {
                    (state.to_string(), Some(rfc3339(*at)), tool.clone())
                }
                _ => (AgentState::Unknown.to_string(), None, None),
            };
            agents.push(AgentInfo {
                id: mark.id.clone(),
                title: terminal_label(kind.as_deref().map(AgentKind::from).as_ref()),
                agent_kind: kind.clone(),
                project: src.project.clone(),
                worktree: src.worktree.clone(),
                status: status.clone(),
                status_since: status_since.clone(),
                tool: tool.clone(),
                branch: (!src.branch.is_empty()).then(|| src.branch.clone()),
                has_conversation: false,
                session: crate::tmux::session_name(&src.key, *tab_id),
                transcript,
            });
        }
    }
    Snapshot { agents }
}

/// Whether some mark of a worktree in `sources` names a tab none of them has
/// (e.g. a tab the other build opened in a worktree that is also live here).
pub(crate) fn has_unresolved_marks(file: &RemoteFile, sources: &[MarkSource]) -> bool {
    file.marked.iter().any(|m| {
        sources
            .iter()
            .find(|s| s.key == m.worktree_key)
            .is_some_and(|s| !s.tabs.iter().any(|(id, _, _)| *id == m.tab_id))
    })
}

/// Add, per mark, the terminal tab the panes file on disk has for it when
/// `sources` lacks it. Dev and release share `~/.openorchestra`; only one
/// serves, and a tab the other opened is in neither our panes nor our
/// in-memory state. Pure: only Shell/Agent records, only for worktrees we
/// already enumerate; `disk` is read-only and never merged into our state.
pub(crate) fn fill_from_disk(
    sources: &mut [MarkSource],
    file: &RemoteFile,
    disk: &crate::panes_state::PanesState,
) {
    use crate::panes_state::TabRecordKind as K;
    for m in &file.marked {
        let Some(src) = sources.iter_mut().find(|s| s.key == m.worktree_key) else {
            continue;
        };
        if src.tabs.iter().any(|(id, _, _)| *id == m.tab_id) {
            continue;
        }
        let Some(rec) = disk
            .worktrees
            .get(&m.worktree_key)
            .and_then(|wp| wp.tabs.iter().find(|t| t.id == m.tab_id))
        else {
            continue;
        };
        let kind = match &rec.kind {
            K::Shell => None,
            K::Agent(a) => Some(a.as_str().to_string()),
            _ => continue,
        };
        src.tabs.push((rec.id, rec.title.clone(), kind));
    }
}

fn apply_tab_record(agent: &mut AgentInfo, state_dir: &Path) {
    if !matches!(agent.agent_kind.as_deref(), Some("claude-code" | "codex")) {
        return;
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TabRecord {
        agent: String,
        state: AgentState,
        timestamp: String,
        status_since: Option<String>,
        session_file: Option<PathBuf>,
        tool: Option<agent_runtime::ToolCall>,
    }
    let stem = oo_remote::session_file_stem(&agent.session);
    let path = state_dir.join("remote/tabs").join(format!("{stem}.json"));
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(record) = serde_json::from_slice::<TabRecord>(&bytes) else {
        return;
    };
    if agent.agent_kind.as_ref() != Some(&record.agent) {
        return;
    }
    agent.status = record.state.to_string();
    agent.status_since = Some(record.status_since.unwrap_or(record.timestamp));
    agent.tool = record.tool.map(|t| t.name);
    agent.transcript = record.session_file;
}

fn rfc3339(t: SystemTime) -> String {
    time::OffsetDateTime::from(t)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

impl HarnessWorkspace {
    /// Start the watcher/refresh loop and, when `remote.json` says enabled,
    /// the server. Called once from `new`.
    pub(crate) fn remote_init(&mut self, cx: &mut Context<Self>) {
        // Reattached agents keep invoking the installed shim without going through on_spawn.
        for result in [
            agent_runtime::ClaudeCodeRuntime::new(&self.remote.state_dir).materialize_shim(),
            agent_runtime::CodexRuntime::new(&self.remote.state_dir).materialize_shim(),
        ] {
            if let Err(e) = result {
                log::warn!("remote: refreshing agent hook: {e}");
            }
        }
        // A previous run that was killed (not quit) can leave shell tabs
        // switched to record their commands. With Remote off nothing should.
        if !self.remote.file.enabled
            && let Err(e) = oo_remote::shell::sync_flags(&self.remote.state_dir, &HashSet::new())
        {
            log::warn!("remote: clearing shell logging switches failed: {e}");
        }
        let changes = self.remote.changes.take();
        cx.spawn(async move |view, cx| {
            let mut ticks: u32 = 0;
            loop {
                cx.background_executor().timer(TICK).await;
                ticks = ticks.wrapping_add(1);
                let mut changed = false;
                if let Some(rx) = &changes {
                    while rx.try_recv().is_ok() {
                        changed = true;
                    }
                }
                let refresh = ticks.is_multiple_of(REFRESH_TICKS);
                let retry = ticks.is_multiple_of(RETRY_TICKS);
                if !(changed || refresh || retry) {
                    continue;
                }
                let alive = view.update(cx, |ws, cx| {
                    if changed {
                        ws.remote.reload();
                        ws.remote_note_new_devices();
                        ws.remote_close_setup_if_paired(cx);
                        ws.remote_reconcile(cx);
                    }
                    if retry {
                        ws.remote_retry_other_build(cx);
                    }
                    if refresh || changed {
                        ws.remote_refresh(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        // Every quit path, including the app-level `Quit` fallback (no
        // workspace in the focus path) and Dock/system quits that skip
        // `request_quit`: restore fitted windows and hand off the stop.
        // Idempotent; a no-op after `request_quit` already did it.
        cx.on_app_quit(|ws, _cx| {
            ws.remote.shutdown_detached();
            async {}
        })
        .detach();
        // Seed transcripts from the registry (already in memory): sessions
        // started before this launch.
        for e in self.manager.registry.active() {
            if let Some(file) = &e.session_file {
                self.remote
                    .record_session(&e.path, e.agent_kind.as_str(), file);
            }
        }
        self.remote_refresh(cx);
        self.remote_reconcile(cx);
    }

    /// Every worktree that can hold tabs (same enumeration as
    /// `apply_resource`), with its terminal tabs. Live pane sets come from
    /// `self.panes`; a worktree whose panes are evicted or not yet restored
    /// falls back to its persisted records, since its tmux sessions live on.
    pub(crate) fn remote_sources(&self) -> Vec<MarkSource> {
        let mut out = Vec::new();
        for repo in &self.data.repos {
            out.push(self.remote_source(
                format!("main:{}", repo.name),
                repo.repo_root.clone(),
                repo.name.clone(),
                String::new(),
                repo.head_branch.clone(),
            ));
            for w in &repo.worktrees {
                let key = worktree_pane_key(w.task_id.as_deref(), &w.id);
                out.push(self.remote_source(
                    key,
                    w.path.clone(),
                    repo.name.clone(),
                    w.name.clone(),
                    w.branch.clone(),
                ));
            }
        }
        let tasks_root = self.data_dir.join("tasks");
        for task in self
            .task_store
            .active()
            .filter(|t| super::coordinator::has_coordinator(t))
        {
            out.push(self.remote_source(
                a2a_broker::coordinator_pane_key(&task.id),
                worktrees::task_coordinator_dir(&tasks_root, &task.slug),
                task.name.clone(),
                "coordinator".to_string(),
                String::new(),
            ));
        }
        out
    }

    fn remote_source(
        &self,
        key: String,
        root: PathBuf,
        project: String,
        worktree: String,
        branch: String,
    ) -> MarkSource {
        use crate::panes_state::TabRecordKind as K;
        let tabs = if let Some(pane) = self.panes.get(&key) {
            pane.tabs
                .iter()
                .filter(|t| matches!(t.content, TabContent::Terminal(_)))
                .map(|t| {
                    (
                        t.id,
                        t.title.clone(),
                        t.agent.as_ref().map(|a| a.as_str().to_string()),
                    )
                })
                .collect()
        } else if let Some(saved) = self.panes_state.worktrees.get(&key) {
            saved
                .tabs
                .iter()
                .filter_map(|t| match &t.kind {
                    K::Shell => Some((t.id, t.title.clone(), None)),
                    K::Agent(a) => Some((t.id, t.title.clone(), Some(a.as_str().to_string()))),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        MarkSource {
            key,
            root,
            project,
            worktree,
            branch,
            tabs,
        }
    }

    /// Rebuild the snapshot and publish it if it changed.
    /// Phones can reach marked tabs: this build or the other one is serving.
    /// Replace the certificate authority devices trust (the old one is kept
    /// as `*.bak`) and restart the server with a new one covering this Mac's
    /// current name and addresses. Every device must trust it again.
    pub(crate) fn remote_renew_ca(&mut self, cx: &mut Context<Self>) {
        if self.remote.server.is_none() || self.remote.busy() {
            return;
        }
        if let Err(e) = oo_remote::certs::retire_ca(&self.remote.state_dir.join("remote")) {
            self.set_error_note(format!("remote: could not replace the certificate: {e}"));
            return;
        }
        log::info!("remote: new certificate requested; restarting the server");
        self.remote_stop(cx);
        self.remote.serve = RemoteServe::Off;
        self.set_note(
            "New certificate made \u{2014} add each device again (Add a device) to trust it"
                .to_string(),
        );
        cx.notify();
    }

    /// Let devices connect from (`trust`) or stop them connecting from the
    /// network with `key`.
    pub(crate) fn remote_set_network_trust(
        &mut self,
        key: &str,
        trust: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = &self.remote.server else {
            return;
        };
        let result = if trust {
            server.trust_network(key)
        } else {
            server.forget_network(key)
        };
        let status = server.status();
        self.remote.status = Some(status);
        if let Err(e) = result {
            self.set_error_note(format!("remote: could not save network setting: {e}"));
        }
        cx.notify();
    }

    /// The device that typed into tmux session `session` in the last few
    /// seconds, if any (from the server's status, refreshed every second).
    pub(crate) fn remote_typing(&self, session: &str) -> Option<String> {
        self.remote
            .status
            .as_ref()?
            .remote_input
            .iter()
            .find(|i| i.session == session)
            .map(|i| i.device.clone())
    }

    pub(crate) fn remote_live(&self) -> bool {
        matches!(
            self.remote.serve,
            RemoteServe::Serving | RemoteServe::OtherBuild
        )
    }

    /// Current opacity for every remote dot (they blink in step).
    pub(crate) fn remote_dot_opacity(&self) -> f32 {
        remote_dot_opacity(self.remote_live(), self.anim_secs())
    }

    /// Keep the dots blinking while remote is live and something is marked;
    /// the loop stops by itself when either goes away.
    fn ensure_remote_blink(&mut self, cx: &mut Context<Self>) {
        let wanted = |ws: &Self| ws.remote_live() && !ws.remote.file.marked.is_empty();
        if self.remote.blinking || !wanted(self) {
            return;
        }
        self.remote.blinking = true;
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(REMOTE_BLINK_FRAME).await;
                let keep = view
                    .update(cx, |ws, cx| {
                        let keep = wanted(ws);
                        ws.remote.blinking = keep;
                        cx.notify();
                        keep
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn remote_refresh(&mut self, cx: &mut Context<Self>) {
        self.ensure_remote_blink(cx);
        self.remote_publish();
        if let Some(server) = &self.remote.server {
            let status = server.status();
            if self.remote.status.as_ref() != Some(&status) {
                self.remote.status = Some(status);
                cx.notify();
            }
        }
    }

    /// `remote_refresh` without a context (for call sites that have none).
    /// Runs on the refresh tick and after changes, not per frame.
    fn remote_publish(&mut self) {
        let mut sources = self.remote_sources();
        if has_unresolved_marks(&self.remote.file, &sources) {
            self.remote_refresh_disk_panes();
            if let Some((_, disk)) = &self.remote.disk_panes {
                fill_from_disk(&mut sources, &self.remote.file, disk);
            }
        } else {
            self.remote.disk_panes = None;
        }
        let mut snap = build_snapshot(
            &self.remote.file,
            &sources,
            &self.remote.signals,
            &self.remote.session_files,
        );
        for agent in &mut snap.agents {
            apply_tab_record(agent, &self.remote.state_dir);
        }
        self.remote.snapshot_tx.send_if_modified(|cur| {
            if *cur == snap {
                return false;
            }
            *cur = snap;
            true
        });
    }

    /// Re-read `panes.json` (read-only) if its mtime moved since the cached
    /// copy. One stat per refresh while unresolved marks exist.
    fn remote_refresh_disk_panes(&mut self) {
        let Ok(mtime) = std::fs::metadata(&self.panes_path).and_then(|m| m.modified()) else {
            self.remote.disk_panes = None;
            return;
        };
        if self
            .remote
            .disk_panes
            .as_ref()
            .is_some_and(|(at, _)| *at == mtime)
        {
            return;
        }
        let disk = crate::panes_state::load(&self.panes_path);
        self.remote.disk_panes = Some((mtime, disk));
    }

    pub(crate) fn remote_set_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Err(e) = self.remote.store.update(|f| f.enabled = on) {
            log::warn!("remote: saving enabled={on} failed: {e}");
            self.set_error_note(format!("remote: could not save setting: {e}"));
        }
        self.remote.reload();
        // An explicit enable retries a failed start.
        if on && matches!(self.remote.serve, RemoteServe::Error(_)) {
            self.remote.serve = RemoteServe::Off;
        }
        self.remote_reconcile(cx);
    }

    pub(crate) fn remote_set_keep_awake(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Err(e) = self.remote.store.update(|f| f.keep_awake = on) {
            log::warn!("remote: saving keep_awake={on} failed: {e}");
            self.set_error_note(format!("remote: could not save setting: {e}"));
        }
        self.remote.reload();
        self.remote.sync_keep_awake();
        cx.notify();
    }

    pub(crate) fn remote_toggle_mark(&mut self, key: &str, tab_id: u64, cx: &mut Context<Self>) {
        // Decide on the file's current contents, not the cache.
        let marked = self.remote.store.load().mark_for(key, tab_id).is_some();
        let result = if marked {
            self.remote.store.unmark(key, Some(tab_id))
        } else {
            self.remote.store.mark(key, tab_id).map(|_| ())
        };
        if let Err(e) = result {
            log::warn!("remote: toggling mark failed: {e}");
            self.set_error_note(format!("remote: could not save mark: {e}"));
        }
        self.remote.reload();
        self.remote_refresh(cx);
        cx.notify();
    }

    /// Cheap (cached file only): safe to call from render.
    pub(crate) fn remote_is_marked(&self, key: &str, tab_id: u64) -> bool {
        self.remote.file.mark_for(key, tab_id).is_some()
    }

    /// Open or close the plain-HTTP setup port ("Add a device" panel). A
    /// no-op unless this instance is serving. Binding is a quick syscall.
    pub(crate) fn remote_set_setup_open(&mut self, open: bool) {
        self.remote.ui.setup_open = open;
        if let Some(server) = &self.remote.server {
            server.set_setup_open(open);
            // Show a bind failure (or its clearing) now, not on the next tick.
            self.remote.status = Some(server.status());
        }
    }

    /// Open "Add a device": fix the setup URL and its QR, issue a pairing
    /// code, and open the setup port.
    pub(crate) fn remote_open_setup(&mut self, cx: &mut Context<Self>) {
        // The server picks the default-route interface's address off-thread
        // (a VM bridge or VPN must not win); before its first status, the
        // lowest private address.
        let url = self
            .remote
            .status
            .as_ref()
            .and_then(|s| s.primary_ip)
            .or_else(|| oo_remote::certs::private_ipv4s().into_iter().next())
            .map(|ip| format!("http://{ip}:{SETUP_PORT}/setup"));
        self.remote.ui.setup_qr = url
            .as_deref()
            .map(super::ui::settings_remote::qr_cells)
            .unwrap_or_default();
        self.remote.ui.setup_url = url;
        self.remote.ui.setup_known = self
            .remote
            .file
            .devices
            .iter()
            .map(|d| d.id.clone())
            .collect();
        // Adding a device here means trusting this network: devices only
        // connect from trusted ones.
        if let Some(server) = &self.remote.server {
            if let Err(e) = server.trust_primary_network() {
                log::warn!("remote: trusting this network failed: {e}");
            }
            self.remote.status = Some(server.status());
        }
        self.remote_new_code(cx);
        self.remote_set_setup_open(true);
        cx.notify();
    }

    /// A phone paired while "Add a device" was open: close the panel (and the
    /// setup port) and say which device it was — no Done click needed.
    /// A device that paired while "Add a device" was not open (that dialog
    /// announces its own): say so, so an unexpected one gets noticed.
    fn remote_note_new_devices(&mut self) {
        let ids: HashSet<String> = self
            .remote
            .file
            .devices
            .iter()
            .map(|d| d.id.clone())
            .collect();
        let Some(prev) = self.remote.known_devices.replace(ids) else {
            return;
        };
        if self.remote.ui.setup_open {
            return;
        }
        if let Some(d) = self
            .remote
            .file
            .devices
            .iter()
            .find(|d| !prev.contains(&d.id))
        {
            self.set_note(format!(
                "New device paired: \u{2018}{}\u{2019} \u{2014} revoke it in Settings \u{2192} Remote if that wasn\u{2019}t you",
                d.name
            ));
        }
    }

    fn remote_close_setup_if_paired(&mut self, cx: &mut Context<Self>) {
        if !self.remote.ui.setup_open {
            return;
        }
        let Some(name) = newly_paired(&self.remote.ui.setup_known, &self.remote.file.devices)
            .map(|d| d.name.clone())
        else {
            return;
        };
        self.remote_close_setup();
        self.set_note(format!("Paired \u{2018}{name}\u{2019}"));
        cx.notify();
    }

    /// Close "Add a device" (and the setup port). Called when the panel is
    /// closed, and whenever Settings closes or leaves the Remote section.
    pub(crate) fn remote_close_setup(&mut self) {
        if !self.remote.ui.setup_open {
            return;
        }
        self.remote_set_setup_open(false);
        // A code is only good while its dialog is open.
        if let Err(e) = auth::withdraw_codes(&self.remote.store) {
            log::warn!("remote: withdrawing pairing codes failed: {e}");
        }
        let ui = &mut self.remote.ui;
        ui.setup_url = None;
        ui.setup_qr = Vec::new();
        ui.code = None;
        ui.code_error = None;
        ui.setup_known = Vec::new();
    }

    /// Issue a fresh pairing code for the open panel.
    pub(crate) fn remote_new_code(&mut self, cx: &mut Context<Self>) {
        match auth::issue_code(&self.remote.store, SystemTime::now()) {
            Ok(code) => {
                self.remote.ui.code = Some((code, Instant::now()));
                self.remote.ui.code_error = None;
            }
            Err(e) => {
                log::warn!("remote: issuing a pairing code failed: {e}");
                self.remote.ui.code = None;
                self.remote.ui.code_error = Some(format!("Could not create a code: {e}"));
            }
        }
        self.remote.reload();
        cx.notify();
    }

    /// Save the HTTPS port; a running server restarts on it (off-thread,
    /// through `remote_reconcile`). `port` is already validated.
    pub(crate) fn remote_set_port(&mut self, port: u16, cx: &mut Context<Self>) {
        self.remote.ui.port_error = None;
        if let Err(e) = self.remote.store.update(|f| f.port = port) {
            log::warn!("remote: saving port={port} failed: {e}");
            self.remote.ui.port_error = Some(format!("Could not save the port: {e}"));
        }
        self.remote.reload();
        // A failed or blocked start retries on the new port now, not in 30 s.
        if self.remote.server.is_none()
            && matches!(
                self.remote.serve,
                RemoteServe::Error(_) | RemoteServe::OtherBuild
            )
        {
            self.remote.serve = RemoteServe::Off;
        }
        self.remote_reconcile(cx);
    }

    /// Revoke, behind a confirm: the first click arms the device's button
    /// for `REVOKE_CONFIRM`, a second click within it revokes.
    pub(crate) fn remote_revoke_click(&mut self, device_id: &str, cx: &mut Context<Self>) {
        let now = Instant::now();
        let armed = matches!(
            &self.remote.ui.revoke_armed,
            Some((id, at)) if id == device_id && now.duration_since(*at) < REVOKE_CONFIRM
        );
        if !armed {
            self.remote.ui.revoke_armed = Some((device_id.to_string(), now));
            let id = device_id.to_string();
            cx.spawn(async move |view, cx| {
                cx.background_executor().timer(REVOKE_CONFIRM).await;
                let _ = view.update(cx, |ws, cx| {
                    // Only this arming: a later click re-armed with a new time.
                    if matches!(&ws.remote.ui.revoke_armed, Some((d, at)) if *d == id && *at == now)
                    {
                        ws.remote.ui.revoke_armed = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
            return;
        }
        self.remote.ui.revoke_armed = None;
        if let Err(e) = auth::revoke(&self.remote.store, device_id) {
            log::warn!("remote: revoking {device_id} failed: {e}");
            self.set_error_note(format!("remote: could not revoke device: {e}"));
        }
        self.remote.reload();
        self.remote_refresh(cx);
        cx.notify();
    }

    pub(crate) fn remote_set_read_only(
        &mut self,
        device_id: &str,
        read_only: bool,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = auth::set_read_only(&self.remote.store, device_id, read_only) {
            log::warn!("remote: saving read_only for {device_id} failed: {e}");
            self.set_error_note(format!("remote: could not save setting: {e}"));
        }
        self.remote.reload();
        cx.notify();
    }

    /// Drop marks for a closed tab (`Some`) or a removed worktree (`None`).
    /// Writes `remote.json` only when something there actually matches.
    pub(crate) fn remote_unmark(&mut self, key: &str, tab_id: Option<u64>) {
        let current = self.remote.store.load();
        let hit = current
            .marked
            .iter()
            .any(|m| m.worktree_key == key && tab_id.is_none_or(|t| m.tab_id == t));
        if !hit {
            return;
        }
        if let Err(e) = self.remote.store.unmark(key, tab_id) {
            log::warn!("remote: unmarking {key} failed: {e}");
        }
        self.remote.reload();
        self.remote_publish();
    }

    /// Bring the server in line with `file.enabled` and `file.port`.
    /// Idempotent; a start or stop in flight finishes first and calls back
    /// in here.
    fn remote_reconcile(&mut self, cx: &mut Context<Self>) {
        let r = &self.remote;
        let action = reconcile_action(
            r.file.enabled,
            r.server.as_ref().map(RemoteServer::port),
            r.file.port,
            r.busy(),
            &r.serve,
        );
        match action {
            ReconcileAction::Start => self.remote_start(cx),
            ReconcileAction::Stop => self.remote_stop(cx),
            // The stop's callback reconciles again and, being `Off`, starts
            // on the new port.
            ReconcileAction::Restart => {
                log::info!("remote: port changed; restarting the server");
                self.remote_stop(cx);
                self.remote.serve = RemoteServe::Off;
            }
            ReconcileAction::Nothing => {}
        }
        if !self.remote.file.enabled {
            self.remote.serve = RemoteServe::Off;
            self.remote.status = None;
        }
        self.remote.sync_keep_awake();
        cx.notify();
    }

    /// The 30 s retry while the other build holds the port.
    fn remote_retry_other_build(&mut self, cx: &mut Context<Self>) {
        if self.remote.file.enabled
            && self.remote.serve == RemoteServe::OtherBuild
            && !self.remote.starting
            && !self.remote.stopping
        {
            self.remote_start(cx);
        }
    }

    fn remote_start(&mut self, cx: &mut Context<Self>) {
        self.remote.starting = true;
        // The server reads the channel's current value on start.
        self.remote_publish();
        let port = self.remote.file.port;
        let cfg = ServerConfig {
            state_dir: self.remote.state_dir.clone(),
            port,
            setup_port: SETUP_PORT,
            tmux: Tmux::default(),
            bind_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            hosts_override: None,
        };
        let rx = self.remote.snapshot_tx.subscribe();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move { RemoteServer::start(cfg, rx) })
                .await;
            let mut result = Some(result);
            let _ = view.update(cx, |ws, cx| {
                if let Some(r) = result.take() {
                    ws.remote_started(r, port, cx);
                }
            });
            // The workspace is gone: stop off-thread rather than blocking
            // here in `Drop`.
            if let Some(Ok(server)) = result {
                cx.background_spawn(async move { server.stop() }).detach();
            }
        })
        .detach();
    }

    /// `port` is the port this start tried (`file.port` may have moved on).
    fn remote_started(
        &mut self,
        result: Result<RemoteServer, StartError>,
        port: u16,
        cx: &mut Context<Self>,
    ) {
        self.remote.starting = false;
        let enabled = self.remote.file.enabled;
        match result {
            Ok(server) if enabled => {
                // "Add a device" is open across a (re)start.
                if self.remote.ui.setup_open {
                    server.set_setup_open(true);
                }
                self.remote.status = Some(server.status());
                self.remote.server = Some(server);
                self.remote.serve = RemoteServe::Serving;
            }
            // Disabled while starting: let it go again.
            Ok(server) => {
                self.remote.server = Some(server);
                self.remote_stop(cx);
                self.remote.serve = RemoteServe::Off;
            }
            Err(StartError::PortInUse(port)) => {
                log::info!("remote: port {port} in use (other build?); retrying every 30 s");
                self.remote.serve = if enabled {
                    RemoteServe::OtherBuild
                } else {
                    RemoteServe::Off
                };
            }
            Err(StartError::Other(e)) => {
                log::warn!("remote: server failed to start: {e}");
                self.remote.serve = if enabled {
                    RemoteServe::Error(e)
                } else {
                    RemoteServe::Off
                };
            }
        }
        // The port changed while starting: a failure on the old port retries
        // on the new one now; a server on the old port restarts (reconcile).
        if enabled && port != self.remote.file.port && self.remote.server.is_none() {
            self.remote.serve = RemoteServe::Off;
        }
        self.remote_reconcile(cx);
    }

    /// Stop the server (if running) on the background executor: `stop`
    /// waits for sockets to flush and close.
    fn remote_stop(&mut self, cx: &mut Context<Self>) {
        let Some(server) = self.remote.server.take() else {
            return;
        };
        self.remote.stopping = true;
        self.remote.status = None;
        cx.spawn(async move |view, cx| {
            cx.background_spawn(async move { server.stop() }).await;
            let _ = view.update(cx, |ws, cx| {
                ws.remote.stopping = false;
                ws.remote_reconcile(cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str) -> oo_remote::Device {
        oo_remote::Device {
            id: id.into(),
            name: name.into(),
            token_sha256: String::new(),
            created: String::new(),
            last_seen: None,
            read_only: false,
            origin_host: String::new(),
        }
    }

    #[test]
    fn newly_paired_finds_a_device_added_since_the_panel_opened() {
        let known = vec!["d_1".to_string()];
        let before = [device("d_1", "iPad")];
        assert!(newly_paired(&known, &before).is_none());
        let after = [device("d_1", "iPad"), device("d_2", "iPhone")];
        assert_eq!(
            newly_paired(&known, &after).map(|d| d.name.as_str()),
            Some("iPhone")
        );
        // A revoke while the panel is open is not a pairing.
        assert!(newly_paired(&known, &[]).is_none());
    }
    use oo_remote::Mark;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn remote_dot_fades_only_while_reachable() {
        let p = DOT_BLINK_PERIOD;
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        // Reachable: full at the period start, faintest at mid-period, and a
        // smooth ramp between (no jumps).
        assert!(close(remote_dot_opacity(true, 0.0), 1.0));
        assert!(close(remote_dot_opacity(true, p * 0.5), DOT_BLINK_DIM));
        assert!(close(remote_dot_opacity(true, p), 1.0));
        let quarter = remote_dot_opacity(true, p * 0.25);
        assert!(quarter > DOT_BLINK_DIM && quarter < 1.0);
        let step = (0..100)
            .map(|i| {
                let t = p * i as f32 / 100.0;
                (remote_dot_opacity(true, t + p / 100.0) - remote_dot_opacity(true, t)).abs()
            })
            .fold(0.0_f32, f32::max);
        assert!(step < 0.05, "fade jumps by {step} per 1% of the period");
        // Remote off: steady and dimmed, whatever the clock says.
        assert_eq!(remote_dot_opacity(false, 0.1), REMOTE_DOT_OFF);
        assert_eq!(remote_dot_opacity(false, p * 0.75), REMOTE_DOT_OFF);
    }

    fn mark(key: &str, tab: u64, id: &str) -> Mark {
        Mark {
            worktree_key: key.into(),
            tab_id: tab,
            id: id.into(),
        }
    }

    fn source(key: &str, root: &str, tabs: &[(u64, &str, Option<&str>)]) -> MarkSource {
        MarkSource {
            key: key.into(),
            root: PathBuf::from(root),
            project: "proj".into(),
            worktree: "wt".into(),
            branch: String::new(),
            tabs: tabs
                .iter()
                .map(|(id, t, k)| (*id, t.to_string(), k.map(str::to_string)))
                .collect(),
        }
    }

    fn file(marks: Vec<Mark>) -> RemoteFile {
        RemoteFile {
            marked: marks,
            ..RemoteFile::default()
        }
    }

    #[test]
    fn chip_click_leads_to_settings_while_remote_is_off() {
        assert_eq!(chip_click(true, false), ChipClick::Toggle);
        assert_eq!(chip_click(true, true), ChipClick::Toggle);
        assert_eq!(
            chip_click(false, false),
            ChipClick::OpenSettings { mark: true }
        );
        // Never unmarks while off: the click is about getting it reachable.
        assert_eq!(
            chip_click(false, true),
            ChipClick::OpenSettings { mark: false }
        );
    }

    #[test]
    fn remote_chip_states() {
        for marked in [false, true] {
            for live in [false, true] {
                assert_eq!(remote_chip(false, marked, live), None);
            }
        }
        assert_eq!(remote_chip(true, false, false), Some(RemoteChip::Off));
        assert_eq!(remote_chip(true, false, true), Some(RemoteChip::Off));
        assert_eq!(remote_chip(true, true, true), Some(RemoteChip::OnLive));
        assert_eq!(
            remote_chip(true, true, false),
            Some(RemoteChip::OnUnreachable)
        );
    }

    #[test]
    fn remote_agent_rows_lists_every_terminal_tab_with_marks() {
        let mut main = source(
            "main:proj",
            "/r/proj",
            &[(1, "claude", Some("claude-code")), (2, "shell", None)],
        );
        main.worktree = "main".into();
        let wt = source("wt1", "/r/wt1", &[(3, "pi", Some("pi"))]);
        let f = file(vec![mark("wt1", 3, "r_x")]);
        let rows = remote_agent_rows(&[main, wt], &f);
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.key.as_str(), r.tab_id, r.marked))
            .collect();
        assert_eq!(
            got,
            [
                ("main:proj", 1, false),
                ("main:proj", 2, false),
                ("wt1", 3, true)
            ]
        );
        assert_eq!(rows[0].kind.as_deref(), Some("claude-code"));
        assert_eq!(rows[1].kind, None);
        assert_eq!(rows[0].project, "proj");
        assert_eq!(rows[0].worktree, "main");
        assert_eq!(rows[2].title, "pi");
    }

    /// Dropping the runtime (quit, window close, teardown) must not run the
    /// blocking `RemoteServer::stop` on the caller's (GPUI main) thread.
    #[test]
    fn dropping_runtime_with_live_server_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = RemoteRuntime::new(dir.path());
        let cfg = ServerConfig {
            state_dir: dir.path().to_path_buf(),
            port: 0,
            setup_port: 0,
            // Private socket name: never the user's default tmux server.
            tmux: Tmux {
                socket: Some(format!("oo-remote-test-{}", std::process::id())),
            },
            bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            hosts_override: Some((
                vec!["localhost".into()],
                vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            )),
        };
        let server = RemoteServer::start(cfg, rt.snapshot_tx.subscribe()).expect("start");
        let port = server.port();
        rt.server = Some(server);
        rt.serve = RemoteServe::Serving;
        let t = std::time::Instant::now();
        drop(rt);
        let took = t.elapsed();
        assert!(
            took < Duration::from_millis(100),
            "drop blocked for {took:?}"
        );
        // The detached stop really runs: the port is released shortly after.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "remote server did not release its listen socket"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn only_marked_tabs_with_session_status_since_and_tool() {
        let f = file(vec![mark("main:proj", 7, "r_abc")]);
        let sources = [source(
            "main:proj",
            "/r/proj",
            &[(7, "claude", Some("claude-code")), (8, "❯_ shell", None)],
        )];
        let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let signals = HashMap::from([(
            (PathBuf::from("/r/proj"), "claude-code".into()),
            (AgentState::Waiting, at, Some("Bash".into())),
        )]);

        let snap = build_snapshot(&f, &sources, &signals, &HashMap::new());

        assert_eq!(snap.agents.len(), 1);
        let a = &snap.agents[0];
        assert_eq!(a.id, "r_abc");
        assert_eq!(a.title, "Claude");
        assert_eq!(a.agent_kind.as_deref(), Some("claude-code"));
        assert_eq!(a.project, "proj");
        assert_eq!(a.worktree, "wt");
        assert_eq!(a.session, crate::tmux::session_name("main:proj", 7));
        assert_eq!(a.status, "waiting");
        assert_eq!(a.status_since.as_deref(), Some("2023-11-14T22:13:20Z"));
        assert_eq!(a.tool.as_deref(), Some("Bash"));
    }

    #[test]
    fn a_mark_whose_tab_is_not_in_sources_is_omitted() {
        let f = file(vec![
            mark("main:proj", 99, "r_gone"),
            mark("wt-x", 1, "r_other"),
        ]);
        let sources = [source("main:proj", "/r/proj", &[(7, "shell", None)])];
        let snap = build_snapshot(&f, &sources, &HashMap::new(), &HashMap::new());
        assert!(snap.agents.is_empty(), "{:?}", snap.agents);
    }

    #[test]
    fn session_records_restore_each_shared_tabs_transcript_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let tabs = dir.path().join("remote/tabs");
        std::fs::create_dir_all(&tabs).unwrap();
        let f = file(vec![mark("k", 1, "a"), mark("k", 2, "b")]);
        let sources = [source(
            "k",
            "/same",
            &[
                (1, "Claude", Some("claude-code")),
                (2, "Codex", Some("codex")),
            ],
        )];
        let mut snap = build_snapshot(&f, &sources, &HashMap::new(), &HashMap::new());
        for (agent, (kind, state, path)) in snap.agents.iter_mut().zip([
            ("claude-code", "idle", "/c.jsonl"),
            ("codex", "running", "/x.jsonl"),
        ]) {
            let data = serde_json::json!({"agent": kind, "state": state, "timestamp": "2026-11-18T12:00:00Z", "sessionFile": path});
            std::fs::write(
                tabs.join(format!("{}.json", agent.session)),
                data.to_string(),
            )
            .unwrap();
        }
        for agent in &mut snap.agents {
            apply_tab_record(agent, dir.path());
        }
        assert_eq!(
            (
                snap.agents[0].status.as_str(),
                snap.agents[0].transcript.as_deref()
            ),
            ("idle", Some(Path::new("/c.jsonl")))
        );
        assert_eq!(
            (
                snap.agents[1].status.as_str(),
                snap.agents[1].transcript.as_deref()
            ),
            ("running", Some(Path::new("/x.jsonl")))
        );
    }

    #[test]
    fn codex_running_does_not_mark_claude_running_in_the_same_worktree() {
        let f = file(vec![mark("wt-1", 3, "r_a"), mark("wt-1", 4, "r_b")]);
        let sources = [source(
            "wt-1",
            "/w/1",
            &[
                (3, "codex", Some("codex")),
                (4, "claude", Some("claude-code")),
            ],
        )];
        let status = HashMap::from([(
            (PathBuf::from("/w/1"), "codex".into()),
            (AgentState::Running, UNIX_EPOCH, None),
        )]);
        let snap = build_snapshot(&f, &sources, &status, &HashMap::new());
        let ids: Vec<_> = snap.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["r_a", "r_b"]);
        assert_eq!(snap.agents[0].status, "running");
        assert_eq!(snap.agents[1].status, "unknown");
        assert_eq!(snap.agents[1].session, crate::tmux::session_name("wt-1", 4));
    }

    #[test]
    fn unknown_and_missing_status_read_unknown() {
        let f = file(vec![mark("a", 1, "r_a"), mark("b", 2, "r_b")]);
        let sources = [
            source("a", "/a", &[(1, "t", None)]),
            source("b", "/b", &[(2, "t", None)]),
        ];
        let status = HashMap::new();
        let snap = build_snapshot(&f, &sources, &status, &HashMap::new());
        assert_eq!(snap.agents.len(), 2);
        assert!(snap.agents.iter().all(|a| a.status == "unknown"));
        assert!(
            snap.agents
                .iter()
                .all(|a| a.status_since.is_none() && a.tool.is_none())
        );
    }

    fn sessions(entries: &[(&str, &str, &str)]) -> HashMap<(PathBuf, String), PathBuf> {
        let mut rt = RemoteRuntime::new(tempfile::tempdir().unwrap().path());
        for (root, kind, f) in entries {
            rt.record_session(Path::new(root), kind, Path::new(f));
        }
        rt.session_files.clone()
    }

    fn transcripts(
        tabs: &[(u64, &str, Option<&str>)],
        entries: &[(&str, &str, &str)],
    ) -> Vec<Option<PathBuf>> {
        let f = file(
            tabs.iter()
                .map(|(id, _, _)| mark("k", *id, &format!("r{id}")))
                .collect(),
        );
        let sources = [source("k", "/R", tabs)];
        let sf = sessions(entries);
        let snap = build_snapshot(&f, &sources, &HashMap::new(), &sf);
        snap.agents.into_iter().map(|a| a.transcript).collect()
    }

    #[test]
    fn agent_tab_gets_its_own_kind_transcript() {
        let got = transcripts(
            &[(1, "c", Some("claude-code")), (2, "p", Some("pi"))],
            &[("/R", "claude-code", "/c.jsonl"), ("/R", "pi", "/p.jsonl")],
        );
        assert_eq!(
            got,
            [
                Some(PathBuf::from("/c.jsonl")),
                Some(PathBuf::from("/p.jsonl"))
            ]
        );
    }

    #[test]
    fn shell_tab_is_terminal_only() {
        let got = transcripts(
            &[(1, "sh", None)],
            &[("/R", "claude-code", "/c.jsonl"), ("/R", "pi", "/p.jsonl")],
        );
        assert_eq!(got, [None]);
    }

    #[test]
    fn same_kind_tabs_in_one_worktree_are_terminal_only() {
        let got = transcripts(
            &[
                (1, "c1", Some("claude-code")),
                (2, "c2", Some("claude-code")),
                (3, "p", Some("pi")),
            ],
            &[("/R", "claude-code", "/c.jsonl"), ("/R", "pi", "/p.jsonl")],
        );
        assert_eq!(got, [None, None, Some(PathBuf::from("/p.jsonl"))]);
        // An unmarked duplicate counts too: the file may be either tab's.
        let f = file(vec![mark("k", 1, "r1")]);
        let sources = [source(
            "k",
            "/R",
            &[
                (1, "c1", Some("claude-code")),
                (2, "c2", Some("claude-code")),
            ],
        )];
        let sf = sessions(&[("/R", "claude-code", "/c.jsonl")]);
        let snap = build_snapshot(&f, &sources, &HashMap::new(), &sf);
        assert_eq!(snap.agents.len(), 1);
        assert_eq!(snap.agents[0].transcript, None);
        // The same kind in another worktree is no duplicate.
        let f = file(vec![mark("k", 1, "r1"), mark("j", 2, "r2")]);
        let sources = [
            source("k", "/R", &[(1, "c1", Some("claude-code"))]),
            source("j", "/S", &[(2, "c2", Some("claude-code"))]),
        ];
        let sf = sessions(&[
            ("/R", "claude-code", "/c.jsonl"),
            ("/S", "claude-code", "/s.jsonl"),
        ]);
        let snap = build_snapshot(&f, &sources, &HashMap::new(), &sf);
        let got: Vec<_> = snap.agents.into_iter().map(|a| a.transcript).collect();
        assert_eq!(
            got,
            [
                Some(PathBuf::from("/c.jsonl")),
                Some(PathBuf::from("/s.jsonl"))
            ]
        );
    }

    #[test]
    fn opening_or_closing_an_agent_tab_forgets_that_kinds_transcript() {
        let mut rt = RemoteRuntime::new(tempfile::tempdir().unwrap().path());
        rt.record_session(Path::new("/R"), "claude-code", Path::new("/old.jsonl"));
        rt.record_session(Path::new("/R"), "pi", Path::new("/p.jsonl"));
        rt.record_session(Path::new("/S"), "claude-code", Path::new("/s.jsonl"));
        rt.forget_session(Path::new("/R"), "claude-code");
        let f = file(vec![mark("k", 1, "r1"), mark("k", 2, "r2")]);
        let sources = [source(
            "k",
            "/R",
            &[(1, "c", Some("claude-code")), (2, "p", Some("pi"))],
        )];
        let snap = build_snapshot(&f, &sources, &HashMap::new(), &rt.session_files);
        let got: Vec<_> = snap.agents.into_iter().map(|a| a.transcript).collect();
        assert_eq!(got, [None, Some(PathBuf::from("/p.jsonl"))]);
        assert!(
            rt.session_files
                .contains_key(&(PathBuf::from("/S"), "claude-code".to_string()))
        );
        // The new tab's first signal records its own file.
        rt.record_session(Path::new("/R"), "claude-code", Path::new("/new.jsonl"));
        assert_eq!(
            rt.session_files
                .get(&(PathBuf::from("/R"), "claude-code".to_string())),
            Some(&PathBuf::from("/new.jsonl"))
        );
    }

    #[test]
    fn no_transcript_when_unknown() {
        let got = transcripts(
            &[(1, "x", Some("codex")), (2, "sh", None)],
            &[("/R", "claude-code", "/c.jsonl")],
        );
        assert_eq!(got, [None, None]);
        let got = transcripts(&[(1, "sh", None)], &[]);
        assert_eq!(got, [None]);
    }

    #[test]
    fn snapshot_carries_branch() {
        let f = file(vec![mark("k", 1, "r1")]);
        let mut src = source("k", "/R", &[(1, "sh", None)]);
        src.branch = "feat/x".into();
        let snap = build_snapshot(&f, &[src.clone()], &HashMap::new(), &HashMap::new());
        assert_eq!(snap.agents[0].branch.as_deref(), Some("feat/x"));
        src.branch = String::new();
        let snap = build_snapshot(&f, &[src], &HashMap::new(), &HashMap::new());
        assert_eq!(snap.agents[0].branch, None);
    }

    #[test]
    fn menu_item_only_for_shell_and_agent() {
        let agent = TabKind::Agent(AgentKind::Pi);
        assert_eq!(
            remote_menu_item(&TabKind::Shell, false),
            Some((
                "Allow remote control",
                Some("Gives paired devices this shell")
            ))
        );
        assert_eq!(
            remote_menu_item(&TabKind::Shell, true),
            Some((
                "Stop remote control",
                Some("Gives paired devices this shell")
            ))
        );
        assert_eq!(
            remote_menu_item(&agent, false),
            Some(("Allow remote control", None))
        );
        assert_eq!(
            remote_menu_item(&agent, true),
            Some(("Stop remote control", None))
        );
        for k in [TabKind::Chat, TabKind::Diff, TabKind::Editor] {
            assert_eq!(remote_menu_item(&k, false), None);
        }
    }

    #[test]
    fn worktree_dot_follows_marks() {
        let f = file(vec![mark("wt-1", 3, "r_a")]);
        assert!(worktree_has_marks(&f, "wt-1"));
        assert!(!worktree_has_marks(&f, "wt-2"));
        assert!(!worktree_has_marks(&file(vec![]), "wt-1"));
    }

    #[test]
    fn pane_key_for_worktrees() {
        assert_eq!(worktree_pane_key(None, "wt-1"), "wt-1");
        assert_eq!(
            worktree_pane_key(Some("t1"), "wt-1"),
            a2a_broker::task_member_pane_key("t1", "wt-1")
        );
    }

    #[test]
    fn disabled_note_only_when_newly_marked_while_off() {
        assert_eq!(
            mark_note(false, false, true),
            Some("Marked \u{2014} turn on Remote control in Settings to reach it")
        );
        assert_eq!(mark_note(true, false, true), None);
        assert_eq!(mark_note(false, false, false), None);
        assert_eq!(mark_note(true, false, false), None);
    }

    #[test]
    fn other_build_note_when_marked_while_it_serves() {
        assert_eq!(
            mark_note(true, true, true),
            Some("Marked \u{2014} devices reach it through the other openOrchestra build")
        );
        assert_eq!(mark_note(true, true, false), None);
        // Off wins: turning it on is the first thing to do.
        assert_eq!(
            mark_note(false, true, true),
            Some("Marked \u{2014} turn on Remote control in Settings to reach it")
        );
    }

    fn disk_state(json: &str) -> crate::panes_state::PanesState {
        serde_json::from_str(json).unwrap()
    }

    /// A tab the other build opened in a worktree that is live here too:
    /// only its mark's record comes from disk, per tab, never other kinds,
    /// never worktrees this build doesn't enumerate.
    #[test]
    fn unresolved_marks_fall_back_to_panes_on_disk() {
        let f = file(vec![
            mark("wt-1", 3, "r_live"),
            mark("wt-1", 9, "r_dev"),
            mark("wt-1", 10, "r_editor"),
            mark("wt-unknown", 1, "r_elsewhere"),
            mark("wt-2", 5, "r_gone"),
        ]);
        let mut sources = vec![
            source("wt-1", "/w/1", &[(3, "claude", Some("claude"))]),
            source("wt-2", "/w/2", &[]),
        ];
        assert!(has_unresolved_marks(&f, &sources));
        let disk = disk_state(
            r#"{
              "next_tab_id": 20,
              "worktrees": {
                "wt-1": { "tabs": [
                  { "id": 3, "kind": "Shell", "title": "stale title" },
                  { "id": 9, "kind": { "Agent": "claude" }, "title": "dev claude" },
                  { "id": 10, "kind": "Editor", "title": "main.rs" },
                  { "id": 11, "kind": "Shell", "title": "unmarked" }
                ] },
                "wt-unknown": { "tabs": [ { "id": 1, "kind": "Shell", "title": "x" } ] }
              }
            }"#,
        );
        fill_from_disk(&mut sources, &f, &disk);
        assert_eq!(
            sources[0].tabs,
            vec![
                (3, "claude".to_string(), Some("claude".to_string())),
                (9, "dev claude".to_string(), Some("claude".to_string())),
            ]
        );
        assert!(sources[1].tabs.is_empty());
        let snap = build_snapshot(&f, &sources, &HashMap::new(), &HashMap::new());
        let ids: Vec<_> = snap.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["r_live", "r_dev"]);
        assert_eq!(snap.agents[1].session, crate::tmux::session_name("wt-1", 9));
    }

    #[test]
    fn all_marks_resolved_needs_no_disk_read() {
        let f = file(vec![mark("wt-1", 3, "r_a"), mark("wt-x", 1, "r_b")]);
        let sources = [source("wt-1", "/w/1", &[(3, "t", None)])];
        // wt-x isn't enumerated here: nothing on disk could place it.
        assert!(!has_unresolved_marks(&f, &sources));
    }

    #[test]
    fn reconcile_decisions() {
        use ReconcileAction::*;
        use RemoteServe as S;
        let act = reconcile_action;
        // Off: stop a live server, else nothing.
        assert_eq!(act(false, Some(7443), 7443, false, &S::Serving), Stop);
        assert_eq!(act(false, None, 7443, false, &S::Off), Nothing);
        // On, nothing running: start only when idle and Off.
        assert_eq!(act(true, None, 7443, false, &S::Off), Start);
        assert_eq!(act(true, None, 7443, true, &S::Off), Nothing);
        assert_eq!(act(true, None, 7443, false, &S::OtherBuild), Nothing);
        assert_eq!(act(true, None, 7443, false, &S::Error("x".into())), Nothing);
        // On and running: restart on a port change, unless busy.
        assert_eq!(act(true, Some(7443), 7443, false, &S::Serving), Nothing);
        assert_eq!(act(true, Some(7443), 9000, false, &S::Serving), Restart);
        assert_eq!(act(true, Some(7443), 9000, true, &S::Serving), Nothing);
    }
}
