//! Per-selection filesystem watch that drives live diff refresh (Deliverable A).
//!
//! The diff panel was event-driven only — it recomputed on selection and on
//! structural mutations, so while an agent edited files it stayed frozen until
//! you re-clicked the worktree. This module watches the *currently-selected*
//! worktree/main root and signals the workspace to recompute, so the diff keeps
//! up with the agent in near real time.
//!
//! Cost is bounded to the one diff on screen: only the selected root is watched,
//! and the watch is retargeted when the selection changes. Events are filtered
//! here ([`is_relevant_change`]) and coalesced by the workspace's drain cadence —
//! that cadence *is* the debounce, so a burst of writes in one agent turn
//! collapses to a single recompute, which the existing `diff_gen` counter then
//! keeps from piling up.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, unbounded};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// Watches the currently-selected root and signals the workspace to recompute
/// its diff. Only one root is watched at a time — [`watch`](Self::watch)
/// retargets it — so the cost stays bounded to the diff on screen.
///
/// Holding the [`RecommendedWatcher`] alive is what keeps the watch running;
/// dropping the `DiffWatcher` (or retargeting) tears the old watch down.
pub struct DiffWatcher {
    /// `None` until the first `watch`, and while a watch cannot be established.
    watcher: Option<RecommendedWatcher>,
    /// The canonical root currently watched. Shared with the notify callback so
    /// it can drop events that arrive from a root we have since moved off, and
    /// read on retarget to skip re-establishing an identical watch.
    current: Arc<Mutex<Option<PathBuf>>>,
    tx: Sender<PathBuf>,
}

impl DiffWatcher {
    /// Create a watcher and the receiver its "something changed under the
    /// watched root" signals are delivered on. The payload is the changed path;
    /// the workspace only cares that *a* relevant change arrived and recomputes
    /// the current selection's diff.
    pub fn new() -> (Self, Receiver<PathBuf>) {
        let (tx, rx) = unbounded();
        (
            Self {
                watcher: None,
                current: Arc::new(Mutex::new(None)),
                tx,
            },
            rx,
        )
    }

    /// Point the watch at `root` (a worktree dir or the main checkout),
    /// replacing any previous watch.
    ///
    /// Keys on the **canonical** path: macOS FSEvents reports `/private/var/...`
    /// while callers pass `/var/...`, and keying on the caller path silently
    /// drops every event — the exact bug that froze `status-watch` on the first
    /// real macOS run (architecture §9). Errors log and leave the watch absent
    /// rather than panicking, matching `status-watch`.
    pub fn watch(&mut self, root: &Path) {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if self.current.lock().unwrap().as_ref() == Some(&canonical) {
            return; // already watching this root
        }
        // Drop the old watcher first (stops its watch) and publish the new root
        // before the fresh watcher can deliver anything.
        self.watcher = None;
        *self.current.lock().unwrap() = Some(canonical.clone());

        let tx = self.tx.clone();
        let current = Arc::clone(&self.current);
        let mut watcher = match notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else { return }; // transient backend error; next event re-syncs
            let Some(root) = current.lock().unwrap().clone() else {
                return;
            };
            // Only relevant edits under the *current* root count — a stale
            // in-flight event from a root we just left is ignored.
            for path in &event.paths {
                if path.starts_with(&root) && is_relevant_change(path) {
                    let _ = tx.send(path.clone());
                    break;
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("diff watcher unavailable: {e}");
                return;
            }
        };
        if let Err(e) = watcher.watch(&canonical, RecursiveMode::Recursive) {
            log::warn!("cannot watch {} for diffs: {e}", canonical.display());
            return;
        }
        self.watcher = Some(watcher);
    }
}

/// Whether a changed path should trigger a diff recompute.
///
/// Ignores VCS metadata and build output. A `cargo build` churns `target/`
/// continuously and would otherwise storm the diff. Git already excludes ignored
/// files from its diff output, so a miss here is only a wasted (deduped,
/// debounced) recompute, never a wrong diff — the filter is about cost, not
/// correctness.
pub fn is_relevant_change(path: &Path) -> bool {
    const IGNORED: [&str; 3] = [".git", "target", "node_modules"];
    !path
        .components()
        .any(|c| IGNORED.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

#[cfg(test)]
mod tests {
    use super::is_relevant_change;
    use std::path::Path;

    #[test]
    fn build_and_vcs_dirs_are_ignored() {
        assert!(!is_relevant_change(Path::new("/repo/.git/index")));
        assert!(!is_relevant_change(Path::new("/repo/target/debug/foo")));
        assert!(!is_relevant_change(Path::new("/repo/node_modules/x/y.js")));
        assert!(!is_relevant_change(Path::new("/repo/crates/target/x"))); // nested target
    }

    #[test]
    fn source_files_are_relevant() {
        assert!(is_relevant_change(Path::new("/repo/src/main.rs")));
        assert!(is_relevant_change(Path::new("/repo/Cargo.toml")));
        // A file literally named `target` (not a dir component) is still relevant.
        assert!(is_relevant_change(Path::new("/repo/src/target.rs")));
    }
}
