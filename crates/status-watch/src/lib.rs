//! status-watch — file-watches worktree roots for `.harness/status.json`
//! changes (written by the pi extension `pi-ext/harness-status.ts`) and
//! delivers parsed [`AgentStatus`] updates over a crossbeam channel.
//!
//! See `docs/architecture.md` §4.1/§6.2 and `docs/spike2-notes.md` for the
//! frozen `status.json` schema:
//!
//! ```json
//! {
//!   "state": "running" | "waiting" | "idle",
//!   "timestamp": "<ISO-8601>",
//!   "sessionFile": "<absolute path>" | null
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

/// Parsed contents of `.harness/status.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub state: AgentState,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub session_file: Option<String>,
}

impl AgentStatus {
    /// The status used when no status file exists (yet).
    pub fn unknown() -> Self {
        AgentStatus {
            state: AgentState::Unknown,
            timestamp: None,
            session_file: None,
        }
    }
}

/// A status update for one watched worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEvent {
    pub worktree_root: PathBuf,
    pub status: AgentStatus,
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
    last_emitted: Option<AgentStatus>,
    /// Last successfully parsed status; malformed writes never clobber it.
    last_good: Option<AgentStatus>,
}

type Registry = Arc<Mutex<HashMap<PathBuf, WatchEntry>>>;

/// Watches any number of worktree roots for `.harness/status.json` changes.
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
        self.watcher
            .watch(&harness_dir, RecursiveMode::NonRecursive)?;

        let initial = read_status(&harness_dir.join("status.json"));
        let mut entry = WatchEntry {
            worktree_root: root,
            last_emitted: None,
            last_good: None,
        };
        let status = match initial {
            ReadOutcome::Parsed(s) => {
                entry.last_good = Some(s.clone());
                s
            }
            // Malformed on first read: nothing better to report than Unknown.
            ReadOutcome::Missing | ReadOutcome::Malformed => AgentStatus::unknown(),
        };
        entry.last_emitted = Some(status.clone());

        let event = StatusEvent {
            worktree_root: entry.worktree_root.clone(),
            status,
        };
        self.registry.lock().unwrap().insert(harness_dir, entry);
        // Receiver dropped just means nobody is listening anymore.
        let _ = self.tx.send(event);
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
    // Collect the .harness dirs this event touches. Events carry paths like
    // `<root>/.harness/status.json` (or the dir itself). If the event has no
    // usable paths, conservatively re-check every watched entry.
    let mut registry = registry.lock().unwrap();
    let dirs: Vec<PathBuf> = if event.paths.is_empty() {
        registry.keys().cloned().collect()
    } else {
        registry
            .keys()
            .filter(|dir| event.paths.iter().any(|p| p.starts_with(dir) || p == *dir))
            .cloned()
            .collect()
    };

    for dir in dirs {
        // Only status.json (and dir-level churn) matters; ignore events that
        // are clearly about other files (e.g. lingering temp files).
        let relevant = event.paths.is_empty()
            || event.paths.iter().any(|p| {
                p == &dir
                    || p.file_name()
                        .map(|n| n == "status.json")
                        .unwrap_or(true)
            });
        if !relevant {
            continue;
        }

        let Some(entry) = registry.get_mut(&dir) else {
            continue;
        };

        let status = match read_status(&dir.join("status.json")) {
            ReadOutcome::Parsed(s) => {
                entry.last_good = Some(s.clone());
                s
            }
            ReadOutcome::Missing => AgentStatus::unknown(),
            // Malformed: keep last good, deliver nothing new.
            ReadOutcome::Malformed => continue,
        };

        if entry.last_emitted.as_ref() == Some(&status) {
            continue; // duplicate OS event for the same content
        }
        entry.last_emitted = Some(status.clone());
        let _ = tx.send(StatusEvent {
            worktree_root: entry.worktree_root.clone(),
            status,
        });
    }
}
