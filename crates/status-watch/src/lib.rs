//! Watches worktrees for `.harness/status-tab-<id>.json` and legacy
//! `.harness/status.json` changes and delivers parsed [`AgentStatus`] updates.
//!
//! See `docs/architecture.md` §4.1/§6.2 and `docs/spike2-notes.md` for the
//! frozen `status.json` schema:
//!
//! ```json
//! {
//!   "state": "running" | "waiting" | "idle",
//!   "timestamp": "<ISO-8601>",
//!   "sessionFile": "<absolute path>" | null,
//!   "tool": { "name": "Edit", "id": "toolu_…", "input": { … } } | null
//! }
//! ```
//!
//! Robustness rules:
//! - file missing            -> [`AgentState::Unknown`] (UI renders as idle/hidden)
//! - malformed JSON          -> skipped; the last good status is kept
//! - unrecognized `state`    -> [`AgentState::Unknown`] (forward compatibility)
//! - atomic rename writes    -> we watch the `.harness` *directory*, not the
//!   file inode, so temp-file + rename (the extension's write strategy) is seen

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;

/// Agent lifecycle state as surfaced in the sidebar pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Running,
    Waiting,
    Idle,
    /// No status file yet, or a `state` string this build doesn't know.
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AgentState::Running => "running",
            AgentState::Waiting => "waiting",
            AgentState::Idle => "idle",
            AgentState::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

/// Parsed contents of a worktree status file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub state: AgentState,
    /// A tagged copy of the per-tab status may also be written to status.json
    /// for existing readers of that file.
    #[serde(default)]
    pub tab_id: Option<u64>,
    /// Which agent wrote this file. Legacy files may use the worktree default.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub session_file: Option<String>,
    /// The tool the agent is about to run, when its hook reports one (Claude
    /// Code's PreToolUse). Absent for every other writer.
    #[serde(default)]
    pub tool: Option<ToolCall>,
}

/// A tool call an agent announced before running it. `input` holds only a few
/// short fields (path, command head, search pattern), never file contents.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ToolCall {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub input: serde_json::Value,
}

impl AgentStatus {
    /// The status used when no status file exists (yet).
    pub fn unknown() -> Self {
        AgentStatus {
            state: AgentState::Unknown,
            tab_id: None,
            agent: None,
            timestamp: None,
            session_file: None,
            tool: None,
        }
    }
}

/// A status update for one tab or a worktree's legacy status file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEvent {
    pub worktree_root: PathBuf,
    pub tab_id: Option<u64>,
    pub status: AgentStatus,
}

pub fn status_path(root: &Path, tab_id: Option<u64>) -> PathBuf {
    root.join(".harness").join(status_filename(tab_id))
}

fn status_filename(tab_id: Option<u64>) -> String {
    tab_id.map_or_else(
        || "status.json".into(),
        |id| format!("status-tab-{id}.json"),
    )
}

fn status_tab(path: &Path) -> Option<Option<u64>> {
    let name = path.file_name()?.to_str()?;
    if name == "status.json" {
        return Some(None);
    }
    let id = name
        .strip_prefix("status-tab-")?
        .strip_suffix(".json")?
        .parse()
        .ok()?;
    (status_filename(Some(id)) == name).then_some(Some(id))
}

#[derive(Debug)]
pub enum WatchError {
    Io(std::io::Error),
    Notify(notify::Error),
    /// `watch()` called for a root that is already watched.
    AlreadyWatched(PathBuf),
    /// `unwatch()` called for a root that is not watched.
    NotWatched(PathBuf),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::Io(e) => write!(f, "io error: {e}"),
            WatchError::Notify(e) => write!(f, "watch error: {e}"),
            WatchError::AlreadyWatched(p) => write!(f, "already watching {}", p.display()),
            WatchError::NotWatched(p) => write!(f, "not watching {}", p.display()),
        }
    }
}

impl std::error::Error for WatchError {}

impl From<std::io::Error> for WatchError {
    fn from(e: std::io::Error) -> Self {
        WatchError::Io(e)
    }
}

impl From<notify::Error> for WatchError {
    fn from(e: notify::Error) -> Self {
        WatchError::Notify(e)
    }
}

/// Resolve a `.harness` directory to the form the notify backend reports.
///
/// macOS's FSEvents backend emits **canonicalized** paths, so an event for a
/// worktree under `/var/...` arrives as `/private/var/...`. Routing events by
/// prefix against the caller-supplied path therefore matched nothing whenever
/// any component was a symlink. We canonicalize once, at `watch()` time, and
/// use the result both as the path handed to notify (so the Linux/inotify
/// backend echoes the same prefix back) and as the registry key.
///
/// Falls back to the path as given when it cannot be resolved — that is the
/// pre-existing behavior, and no worse than it was.
fn resolve_harness_dir(harness_dir: &Path) -> PathBuf {
    harness_dir
        .canonicalize()
        .unwrap_or_else(|_| harness_dir.to_path_buf())
}

struct WatchEntry {
    /// The root **as the caller passed it**. Events are reported under this
    /// path so callers can match against their own keys; the registry key is
    /// the resolved form and is an implementation detail.
    worktree_root: PathBuf,
    /// Last status delivered on the channel; used to suppress duplicate
    /// deliveries when the OS reports several events per atomic write.
    last_emitted: HashMap<Option<u64>, AgentStatus>,
}

type Registry = Arc<Mutex<HashMap<PathBuf, WatchEntry>>>;

/// Watches worktree roots for tab and legacy status changes in `.harness`.
///
/// ```no_run
/// use status_watch::StatusWatcher;
/// let (mut watcher, rx) = StatusWatcher::new().unwrap();
/// watcher.watch("/path/to/worktree").unwrap();
/// for event in rx {
///     println!("{:?} -> {:?}", event.worktree_root, event.status.state);
/// }
/// ```
pub struct StatusWatcher {
    watcher: RecommendedWatcher,
    registry: Registry,
    tx: Sender<StatusEvent>,
}

impl StatusWatcher {
    /// Create a watcher and the receiver its events are delivered on.
    pub fn new() -> Result<(Self, Receiver<StatusEvent>), WatchError> {
        let (tx, rx) = unbounded::<StatusEvent>();
        let registry: Registry = Arc::new(Mutex::new(HashMap::new()));

        let cb_registry = Arc::clone(&registry);
        let cb_tx = tx.clone();
        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let event = match res {
                Ok(event) => event,
                Err(_) => return, // transient backend error; next event re-syncs
            };
            handle_fs_event(&cb_registry, &cb_tx, &event);
        })?;

        Ok((
            StatusWatcher {
                watcher,
                registry,
                tx,
            },
            rx,
        ))
    }

    /// Start watching `worktree_root`. Creates `.harness/` if it does not
    /// exist yet (the directory must exist to be watched; the pi extension
    /// creates it too — whichever side gets there first wins). Immediately
    /// delivers the current status (or `Unknown` if no file) on the channel.
    pub fn watch(&mut self, worktree_root: impl AsRef<Path>) -> Result<(), WatchError> {
        let root = worktree_root.as_ref().to_path_buf();
        // Must exist before it can be resolved or watched. The pi extension
        // creates it too — whichever side gets there first wins.
        std::fs::create_dir_all(root.join(".harness"))?;
        let harness_dir = resolve_harness_dir(&root.join(".harness"));

        {
            let registry = self.registry.lock().unwrap();
            if registry.contains_key(&harness_dir) {
                return Err(WatchError::AlreadyWatched(root));
            }
        }

        // Watch the directory (not the file): atomic temp+rename replaces the
        // inode, which would silently detach a file-level watch.
        let mut registry = self.registry.lock().unwrap();
        self.watcher
            .watch(&harness_dir, RecursiveMode::NonRecursive)?;
        let mut entry = WatchEntry {
            worktree_root: root,
            last_emitted: HashMap::new(),
        };
        entry.refresh_dir(&harness_dir, &self.tx);
        registry.insert(harness_dir, entry);
        Ok(())
    }

    /// Stop watching `worktree_root`.
    pub fn unwatch(&mut self, worktree_root: impl AsRef<Path>) -> Result<(), WatchError> {
        let root = worktree_root.as_ref().to_path_buf();
        // Resolve the same way `watch` did, or we would fail to find the entry
        // (and leak the underlying watch) for any symlinked root.
        let harness_dir = resolve_harness_dir(&root.join(".harness"));
        let removed = self.registry.lock().unwrap().remove(&harness_dir);
        if removed.is_none() {
            return Err(WatchError::NotWatched(root));
        }
        self.watcher.unwatch(&harness_dir)?;
        Ok(())
    }

    /// Currently watched worktree roots.
    pub fn watched(&self) -> Vec<PathBuf> {
        self.registry
            .lock()
            .unwrap()
            .values()
            .map(|e| e.worktree_root.clone())
            .collect()
    }
}

/// Result of attempting to read a status file.
enum ReadOutcome {
    Parsed(AgentStatus),
    Missing,
    Malformed,
}

fn read_status(path: &Path) -> ReadOutcome {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return ReadOutcome::Missing,
    };
    match serde_json::from_slice::<AgentStatus>(&bytes) {
        Ok(status) => ReadOutcome::Parsed(status),
        Err(_) => ReadOutcome::Malformed,
    }
}

fn handle_fs_event(registry: &Registry, tx: &Sender<StatusEvent>, event: &Event) {
    let mut registry = registry.lock().unwrap();
    if event.paths.is_empty() {
        for (dir, entry) in registry.iter_mut() {
            entry.refresh_dir(dir, tx);
        }
        return;
    }
    for path in &event.paths {
        if let Some(entry) = registry.get_mut(path) {
            entry.refresh_dir(path, tx);
        } else if let Some((entry, tab_id)) = path
            .parent()
            .and_then(|dir| registry.get_mut(dir))
            .zip(status_tab(path))
        {
            entry.refresh_file(path, tab_id, tx);
        }
    }
}

impl WatchEntry {
    fn refresh_dir(&mut self, dir: &Path, tx: &Sender<StatusEvent>) {
        let mut tabs: Vec<_> = self.last_emitted.keys().filter_map(|&id| id).collect();
        tabs.extend(
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| status_tab(&entry.path()).flatten()),
        );
        tabs.sort_unstable();
        tabs.dedup();
        self.refresh_file(&dir.join("status.json"), None, tx);
        for id in tabs {
            self.refresh_file(&dir.join(status_filename(Some(id))), Some(id), tx);
        }
    }

    fn refresh_file(&mut self, path: &Path, tab_id: Option<u64>, tx: &Sender<StatusEvent>) {
        let (status, keep) = match read_status(path) {
            // The tagged status.json mirrors a dedicated tab file. Publishing
            // both would count that tab twice and repeat notifications.
            // A tagged status.json is a mirror of a tab file. If an untagged
            // status was already delivered, dropping the mirror leaves that
            // legacy record in place, so a later Idle on the tab file does
            // not clear the root's Running.
            ReadOutcome::Parsed(s) if tab_id.is_none() && s.tab_id.is_some() => {
                if self
                    .last_emitted
                    .get(&None)
                    .is_some_and(|previous| previous.state != AgentState::Unknown)
                {
                    self.last_emitted.insert(None, AgentStatus::unknown());
                    let _ = tx.send(StatusEvent {
                        worktree_root: self.worktree_root.clone(),
                        tab_id: None,
                        status: AgentStatus::unknown(),
                    });
                }
                return;
            }
            ReadOutcome::Parsed(s) => (s, true),
            ReadOutcome::Missing
                if tab_id.is_some() && self.last_emitted.remove(&tab_id).is_none() =>
            {
                return
            }
            ReadOutcome::Missing => (AgentStatus::unknown(), tab_id.is_none()),
            ReadOutcome::Malformed
                if tab_id.is_none() && !self.last_emitted.contains_key(&None) =>
            {
                (AgentStatus::unknown(), true)
            }
            ReadOutcome::Malformed => return,
        };
        if self.last_emitted.get(&tab_id) == Some(&status) {
            return;
        }
        if keep {
            self.last_emitted.insert(tab_id, status.clone());
        }
        let _ = tx.send(StatusEvent {
            worktree_root: self.worktree_root.clone(),
            tab_id,
            status,
        });
    }
}
