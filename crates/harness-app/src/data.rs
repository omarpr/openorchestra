//! Spike 4 — the real worktree view model, built from the on-disk registry +
//! live git (replaces Spike 1's `model.rs` mocks). Pure data: the workspace
//! view renders these structs; the `worktrees` crate does the git/registry work.

use std::path::{Path, PathBuf};

use worktrees::{Diff, GitWorktrees, Registry, SystemRunner};

/// One worktree card's data (real git status + registry avatar).
pub struct WorktreeView {
    pub id: String,
    pub name: String,
    pub branch: String,
    pub path: PathBuf,
    pub initials: String,
    pub color: u8,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: usize,
}

/// A repo group: its `main` entry + its worktrees.
///
/// The `main` row is a first-class citizen (D8 revised): it carries the same
/// live git status as a worktree card so it can show ahead/behind/dirty badges,
/// not just a static check.
pub struct RepoView {
    pub name: String,
    pub repo_root: PathBuf,
    pub default_branch: String,
    /// Branch actually checked out in the main working copy's HEAD. Usually the
    /// default branch, but the main checkout can sit on a feature branch — the
    /// sidebar shows this so "main" never lies about where HEAD is (`HEAD` here
    /// when detached).
    pub head_branch: String,
    /// Ref the diff/ahead-behind compares against — `origin/<default>` when a
    /// remote exists, else the local default branch (§6.1 D10).
    pub compare_ref: String,
    /// Commits on the main checkout's HEAD not on `compare_ref`.
    pub ahead: usize,
    /// Commits on `compare_ref` not on the main checkout's HEAD.
    pub behind: usize,
    /// Uncommitted changed paths in the main checkout.
    pub dirty: usize,
    pub worktrees: Vec<WorktreeView>,
}

pub struct WorkspaceData {
    pub repos: Vec<RepoView>,
}

impl WorkspaceData {
    /// Build the view from an explicit list of repo roots (persisted + added via
    /// the folder picker) + the registry. An empty `repo_roots` yields an empty
    /// `repos` (the app shows its empty state). Each repo shows even with zero
    /// worktrees, so its read-only `main` entry is always present.
    pub fn build(
        git: &GitWorktrees<SystemRunner>,
        registry: &Registry,
        repo_roots: &[std::path::PathBuf],
    ) -> Self {
        let mut repos: Vec<RepoView> = Vec::new();
        for root in repo_roots {
            let name = root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string());
            let mut rv = new_repo_view(git, &name, root);
            for entry in registry.active().filter(|e| &e.repo_root == root) {
                let status = git.status(&entry.path, Some(&rv.compare_ref)).unwrap_or_default();
                rv.worktrees.push(WorktreeView {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    branch: entry.branch.clone(),
                    path: entry.path.clone(),
                    initials: entry.avatar.initials.clone(),
                    color: entry.avatar.color,
                    ahead: status.ahead,
                    behind: status.behind,
                    dirty: status.dirty_files,
                });
            }
            repos.push(rv);
        }
        WorkspaceData { repos }
    }
}

fn new_repo_view(git: &GitWorktrees<SystemRunner>, name: &str, root: &Path) -> RepoView {
    let default_branch = git
        .default_branch(root)
        .unwrap_or_else(|_| "main".to_string());
    // What HEAD is actually on — falls back to the default branch only if the
    // probe fails, so the label degrades to the old behavior rather than blank.
    let head_branch = git
        .current_branch(root)
        .unwrap_or_else(|_| default_branch.clone());
    let origin_ref = format!("origin/{default_branch}");
    let compare_ref = if git.ref_exists(root, &origin_ref) {
        origin_ref
    } else {
        default_branch.clone()
    };
    // Same live status a worktree card gets — reused, not re-derived, so the
    // main row's badges cannot drift from the card ones.
    let status = git.status(root, Some(&compare_ref)).unwrap_or_default();
    RepoView {
        name: name.to_string(),
        repo_root: root.to_path_buf(),
        default_branch,
        head_branch,
        compare_ref,
        ahead: status.ahead,
        behind: status.behind,
        dirty: status.dirty_files,
        worktrees: Vec::new(),
    }
}

/// Diff for a selected path (worktree dir, or the repo root for the `main`
/// entry) against its compare ref. Errors degrade to an empty diff.
pub fn compute_diff(git: &GitWorktrees<SystemRunner>, path: &Path, compare_ref: &str) -> Diff {
    git.diff(path, compare_ref).unwrap_or_default()
}
