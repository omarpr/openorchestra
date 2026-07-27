//! Watches every repo's `.git` so the sidebar's ahead/behind/dirty badges keep
//! up with git itself.
//!
//! Those badges came from `WorkspaceData::build`, which ran only at startup and
//! inside `refresh()` — and every `refresh()` caller is a structural mutation
//! (add/remove a repo, create/delete a worktree). Commit, push, fetch, checkout,
//! stage: none of them moved a badge. A card could sit on `↑9 ~1` long after the
//! branch was pushed and the tree was clean, which is worse than showing nothing
//! — a stale number reads as a current one.
//!
//! [`DiffWatcher`](crate::diff_watch::DiffWatcher) cannot do this job. It watches
//! the *selected* root only, and its filter deliberately ignores `.git`, because
//! it exists to answer "did a file the diff renders change?". This watcher asks
//! the opposite question — "did git move?" — across every repo at once.
//!
//! # What is watched, and what is not
//!
//! Per repo root, three watches instead of one recursive watch on `.git`:
//!
//! - `.git` non-recursively — `HEAD`, `index`, `packed-refs`, `ORIG_HEAD`.
//!   A commit rewrites the index; a checkout rewrites `HEAD`.
//! - `.git/refs` recursively — `refs/heads/<branch>` on commit, and
//!   `refs/remotes/origin/<branch>` on push or fetch, which is the one that
//!   moves `↑`/`↓` when the work leaves the machine. This is nested, so a
//!   non-recursive watch on `.git` alone would miss every push.
//! - `.git/worktrees` recursively, when it exists — a linked worktree's `HEAD`
//!   and `index` live under the *main* repo's `.git`, not in its own directory,
//!   so this is what makes a worktree card react to its own commits.
//!
//! `.git/objects` is deliberately excluded: fetching a large branch writes
//! thousands of files there and not one of them changes a badge. Ref writes are
//! the signal; objects are the noise.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// Watches the `.git` of every known repo root and signals that git moved.
///
/// The payload is the changed path, for logging and nothing else: the workspace
/// recomputes every card's status regardless of which repo fired, because the
/// recompute is two cheap git calls per worktree and the alternative is a map
/// from watch paths back to cards that can only ever drift.
pub struct GitWatcher {
    /// `None` until the first [`watch_repos`](Self::watch_repos), and whenever
    /// no watch could be established.
    watcher: Option<RecommendedWatcher>,
    /// Canonical repo roots currently watched. Shared with the notify callback
    /// so it can drop events from a repo we have since removed, and compared on
    /// retarget to skip rebuilding an identical watch set.
    current: Arc<Mutex<Vec<PathBuf>>>,
    tx: Sender<PathBuf>,
}

impl GitWatcher {
    /// Create a watcher and the receiver its "git moved" signals arrive on.
    pub fn new() -> (Self, Receiver<PathBuf>) {
        let (tx, rx) = unbounded();
        (
            Self {
                watcher: None,
                current: Arc::new(Mutex::new(Vec::new())),
                tx,
            },
            rx,
        )
    }

    /// Watch exactly `roots`, replacing any previous set.
    ///
    /// Keys on **canonical** paths for the same reason `DiffWatcher` does: macOS
    /// FSEvents reports `/private/var/...` for a caller's `/var/...`, and keying
    /// on the caller path silently drops every event (architecture §9). A root
    /// whose `.git` is missing or is a file — a linked worktree passed in by
    /// mistake — is skipped with a warning rather than failing the whole set.
    pub fn watch_repos(&mut self, roots: &[PathBuf]) {
        let canonical: Vec<PathBuf> = roots
            .iter()
            .map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.clone()))
            .collect();
        if *self.current.lock().unwrap() == canonical {
            return; // already watching exactly this set
        }
        // Drop the old watcher first (stops its watches) and publish the new set
        // before the fresh watcher can deliver anything against it.
        self.watcher = None;
        *self.current.lock().unwrap() = canonical.clone();
        if canonical.is_empty() {
            return;
        }

        let tx = self.tx.clone();
        let current = Arc::clone(&self.current);
        let mut watcher = match notify::recommended_watcher(
            move |res: Result<Event, notify::Error>| {
                let Ok(event) = res else { return }; // transient backend error; next event re-syncs
                let roots = current.lock().unwrap().clone();
                for path in &event.paths {
                    if roots.iter().any(|r| path.starts_with(r)) && is_relevant_change(path) {
                        let _ = tx.send(path.clone());
                        break;
                    }
                }
            },
        ) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("git watcher unavailable: {e}");
                return;
            }
        };

        let mut watched_any = false;
        for root in &canonical {
            let git_dir = root.join(".git");
            if !git_dir.is_dir() {
                log::warn!("no .git directory at {}; badges stay static", root.display());
                continue;
            }
            let mut add = |path: PathBuf, mode: RecursiveMode| {
                if !path.exists() {
                    return; // refs/ always exists; worktrees/ only once one is created
                }
                match watcher.watch(&path, mode) {
                    Ok(()) => watched_any = true,
                    Err(e) => log::warn!("cannot watch {}: {e}", path.display()),
                }
            };
            add(git_dir.clone(), RecursiveMode::NonRecursive);
            add(git_dir.join("refs"), RecursiveMode::Recursive);
            add(git_dir.join("worktrees"), RecursiveMode::Recursive);
        }
        if watched_any {
            log::debug!("git watch armed for {} repo(s)", canonical.len());
            self.watcher = Some(watcher);
        }
    }
}

/// Whether a changed path under `.git` should trigger a status recompute.
///
/// Git writes through lock files: `index.lock` is created, written, then renamed
/// onto `index`. Both steps are events, and the lock one arrives while the
/// repository is mid-write — precisely when a `git status` would either block on
/// the lock or read a half-applied state. Dropping `.lock` paths costs nothing,
/// because the rename that follows is itself an event on the real file.
pub fn is_relevant_change(path: &Path) -> bool {
    if path.extension().is_some_and(|e| e == "lock") {
        return false;
    }
    // Not every write under `.git` is a ref write, and the ones that are not
    // change no badge: `COMMIT_EDITMSG` is the message buffer an editor writes
    // as you type, and `logs/` is the reflog, which mirrors moves we already saw.
    !path
        .components()
        .any(|c| matches!(c.as_os_str().to_string_lossy().as_ref(), "objects" | "logs"))
        && path.file_name().is_none_or(|n| n != "COMMIT_EDITMSG")
}

#[cfg(test)]
mod tests {
    use super::is_relevant_change;
    use std::path::Path;

    #[test]
    fn ref_and_index_writes_are_relevant() {
        assert!(is_relevant_change(Path::new("/repo/.git/index")));
        assert!(is_relevant_change(Path::new("/repo/.git/HEAD")));
        assert!(is_relevant_change(Path::new("/repo/.git/packed-refs")));
        assert!(is_relevant_change(Path::new("/repo/.git/refs/heads/master")));
        // The push case: a nested remote-tracking ref, and the reason `.git` is
        // watched recursively under `refs/`.
        assert!(is_relevant_change(Path::new(
            "/repo/.git/refs/remotes/origin/master"
        )));
        // A linked worktree keeps its HEAD under the main repo's .git.
        assert!(is_relevant_change(Path::new(
            "/repo/.git/worktrees/feat-x/HEAD"
        )));
    }

    #[test]
    fn locks_objects_and_message_buffers_are_ignored() {
        assert!(!is_relevant_change(Path::new("/repo/.git/index.lock")));
        assert!(!is_relevant_change(Path::new(
            "/repo/.git/refs/heads/master.lock"
        )));
        assert!(!is_relevant_change(Path::new(
            "/repo/.git/objects/ab/cdef0123"
        )));
        assert!(!is_relevant_change(Path::new("/repo/.git/logs/HEAD")));
        assert!(!is_relevant_change(Path::new("/repo/.git/COMMIT_EDITMSG")));
    }
}
