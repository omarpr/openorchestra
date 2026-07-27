//! View models built from the on-disk registry, task store, and live git.

use std::path::{Path, PathBuf};

use worktrees::{Diff, DiffFile, GitWorktrees, Registry, SystemRunner, TaskStore};

/// One worktree under a project (used for status attribution and forge poll).
pub struct WorktreeView {
    pub id: String,
    pub name: String,
    pub branch: String,
    pub path: PathBuf,
    pub task_id: Option<String>,
}

/// A member project under a task.
pub struct TaskMemberView {
    pub worktree_id: String,
    pub repo: String,
    pub branch: String,
    pub path: PathBuf,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: usize,
    pub is_home: bool,
}

/// A task row in the sidebar (1..N members).
pub struct TaskView {
    pub id: String,
    pub name: String,
    pub initials: String,
    pub color: u8,
    pub members: Vec<TaskMemberView>,
    pub dirty: usize,
}

/// A repo group: its `main` entry (Projects foot) + member worktree list.
pub struct RepoView {
    pub name: String,
    pub repo_root: PathBuf,
    pub default_branch: String,
    pub head_branch: String,
    pub compare_ref: String,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: usize,
    pub worktrees: Vec<WorktreeView>,
}

pub struct WorkspaceData {
    pub repos: Vec<RepoView>,
    pub tasks: Vec<TaskView>,
}

impl WorkspaceData {
    pub fn build(
        git: &GitWorktrees<SystemRunner>,
        registry: &Registry,
        tasks: &TaskStore,
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
                rv.worktrees.push(WorktreeView {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    branch: entry.branch.clone(),
                    path: entry.path.clone(),
                    task_id: entry.task_id.clone(),
                });
            }
            repos.push(rv);
        }

        let mut task_views: Vec<TaskView> = Vec::new();
        for task in tasks.active() {
            let mut members = Vec::new();
            let mut dirty = 0;
            let mut initials = "TK".to_string();
            let mut color = 0u8;
            for m in &task.members {
                let compare = repos
                    .iter()
                    .find(|r| r.name == m.repo)
                    .map(|r| r.compare_ref.clone())
                    .unwrap_or_else(|| "HEAD".into());
                let status = git.status(&m.path, Some(&compare)).unwrap_or_default();
                dirty += status.dirty_files;
                if let Some(entry) = registry.get(&m.worktree_id) {
                    initials = entry.avatar.initials.clone();
                    color = entry.avatar.color;
                }
                members.push(TaskMemberView {
                    worktree_id: m.worktree_id.clone(),
                    repo: m.repo.clone(),
                    branch: m.branch.clone(),
                    path: m.path.clone(),
                    ahead: status.ahead,
                    behind: status.behind,
                    dirty: status.dirty_files,
                    is_home: m.repo == task.home_repo,
                });
            }
            task_views.push(TaskView {
                id: task.id.clone(),
                name: task.name.clone(),
                initials,
                color,
                members,
                dirty,
            });
        }

        WorkspaceData {
            repos,
            tasks: task_views,
        }
    }
}

fn new_repo_view(git: &GitWorktrees<SystemRunner>, name: &str, root: &Path) -> RepoView {
    let default_branch = git
        .default_branch(root)
        .unwrap_or_else(|_| "main".to_string());
    let head_branch = git
        .current_branch(root)
        .unwrap_or_else(|_| default_branch.clone());
    let origin_ref = format!("origin/{default_branch}");
    let compare_ref = if git.ref_exists(root, &origin_ref) {
        origin_ref
    } else {
        default_branch.clone()
    };
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

/// Diff for a selected path against its compare ref.
pub fn compute_diff(git: &GitWorktrees<SystemRunner>, path: &Path, compare_ref: &str) -> Diff {
    git.diff(path, compare_ref).unwrap_or_default()
}

/// One file in a unified multi-member diff, tagged with its member worktree.
#[derive(Clone)]
pub struct UnifiedDiffFile {
    pub member_id: String,
    pub repo: String,
    pub file: DiffFile,
}

/// Aggregated changeset across task members.
#[derive(Clone, Default)]
pub struct UnifiedDiff {
    pub files: Vec<UnifiedDiffFile>,
    pub unified: String,
}

impl UnifiedDiff {
    pub fn from_members(
        git: &GitWorktrees<SystemRunner>,
        members: &[(String, String, PathBuf, String)], // id, repo, path, compare_ref
    ) -> Self {
        let mut files = Vec::new();
        let mut unified = String::new();
        for (id, repo, path, compare) in members {
            let diff = compute_diff(git, path, compare);
            if !diff.unified.is_empty() {
                if !unified.is_empty() {
                    unified.push_str("\n");
                }
                unified.push_str(&format!("# {repo}\n"));
                unified.push_str(&diff.unified);
            }
            for file in diff.files {
                files.push(UnifiedDiffFile {
                    member_id: id.clone(),
                    repo: repo.clone(),
                    file,
                });
            }
        }
        UnifiedDiff { files, unified }
    }
}
