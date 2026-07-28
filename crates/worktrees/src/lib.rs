//! worktrees — git worktree manager + on-disk registry (architecture §6.1).
//!
//! Headless and UI-free (like `forge`/`status-watch`), so the same backend can
//! drive the GPUI app and batch evals. Three layers:
//!
//! - [`Registry`] — the persisted index of worktrees (repo, branch, session,
//!   avatar, PR, lifecycle state). Renames/avatar edits are registry-only.
//! - [`GitWorktrees`] — git operations (`worktree add/remove`, `branch -D`,
//!   status, merge-base diff) behind an injectable [`CommandRunner`].
//! - [`Manager`] — ties the two together for the create / archive / delete
//!   lifecycle flows (§6.1 D6), keeping git and the registry in step.
//!
//! One task = one or more member worktrees (§6.1). Worktree dirs live under
//! `<worktrees_root>/<repo>/<task_slug>`.

mod git;
mod hooks;
mod login_shell;
mod registry;
mod runner;
mod tasks;

pub use git::{
    Change, Diff, DiffFile, GitError, GitStatus, GitWorktrees, PushState, WorktreeListEntry,
    MAX_UNTRACKED_FILES,
};
pub use hooks::{
    classify_hook_script, clear_local_hooks, committed_hooks_path, ensure_hook_script,
    format_visible_setup_runner, hook_script_path, local_hooks_base, local_hooks_path,
    local_scripts_dir, merge_hooks_replace, repo_hooks_base, repo_scripts_dir, shell_export_line,
    visible_setup_shell_line, write_visible_setup_runner, HookEnv, HookLayer, HookPhase,
    HookScriptPhase, HooksConfig, HooksError, HooksFile, TaskHooks,
};
pub use login_shell::interactive_login_command;
pub use registry::{
    AVATAR_PALETTE_LEN, AgentKind, Avatar, Registry, RegistryError, RepoGroup, WorktreeEntry,
};
pub use runner::{CommandOutput, CommandRunner, SystemRunner};
pub use tasks::{
    agents_md_contents, archive_task, create_task, delete_task, migrate_worktrees_to_tasks,
    write_home_agents_md_at, CreateTaskMember, CreateTaskRequest, Task, TaskError, TaskMember,
    TaskStore,
};

use std::path::{Path, PathBuf};

/// A request to create a new worktree (one task).
#[derive(Debug, Clone)]
pub struct CreateRequest {
    /// Repo display name (main checkout dir name).
    pub repo: String,
    /// Absolute path to the repo's main checkout.
    pub repo_root: PathBuf,
    pub task_slug: String,
    pub branch: String,
    /// Base ref to branch from (e.g. `origin/main`); `None` = current HEAD.
    pub base_ref: Option<String>,
    /// ISO-8601 timestamp (caller-supplied — no clock in this crate).
    pub created_at: String,
    /// Which agent CLI this worktree will run (§4.5). Defaults to pi.
    pub agent_kind: AgentKind,
}

/// Errors from the lifecycle flows.
#[derive(Debug)]
pub enum WorktreeError {
    Git(GitError),
    Registry(RegistryError),
    Io(std::io::Error),
    /// The referenced worktree id is not in the registry.
    NotFound(String),
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorktreeError::Git(e) => write!(f, "{e}"),
            WorktreeError::Registry(e) => write!(f, "{e}"),
            WorktreeError::Io(e) => write!(f, "{e}"),
            WorktreeError::NotFound(id) => write!(f, "worktree {id:?} not found"),
        }
    }
}

impl std::error::Error for WorktreeError {}

impl From<GitError> for WorktreeError {
    fn from(e: GitError) -> Self {
        WorktreeError::Git(e)
    }
}
impl From<RegistryError> for WorktreeError {
    fn from(e: RegistryError) -> Self {
        WorktreeError::Registry(e)
    }
}
impl From<std::io::Error> for WorktreeError {
    fn from(e: std::io::Error) -> Self {
        WorktreeError::Io(e)
    }
}

/// Ties [`Registry`] + [`GitWorktrees`] together for the lifecycle flows.
/// Each mutating flow performs the git op first, then updates + persists the
/// registry, so a git failure never leaves a phantom registry entry.
pub struct Manager<R: CommandRunner> {
    pub registry: Registry,
    git: GitWorktrees<R>,
    worktrees_root: PathBuf,
}

impl<R: CommandRunner> Manager<R> {
    pub fn new(
        registry: Registry,
        git: GitWorktrees<R>,
        worktrees_root: impl Into<PathBuf>,
    ) -> Self {
        Manager {
            registry,
            git,
            worktrees_root: worktrees_root.into(),
        }
    }

    pub fn git(&self) -> &GitWorktrees<R> {
        &self.git
    }

    /// Where a worktree's dir lives: `<worktrees_root>/<repo>/<task_slug>`.
    pub fn worktree_path(&self, repo: &str, task_slug: &str) -> PathBuf {
        self.worktrees_root.join(repo).join(task_slug)
    }

    /// Create a worktree: `git worktree add` then register + persist.
    pub fn create(&mut self, req: CreateRequest) -> Result<String, WorktreeError> {
        let path = self.worktree_path(&req.repo, &req.task_slug);
        self.git
            .add_worktree(&req.repo_root, &path, &req.branch, req.base_ref.as_deref())?;

        let id = WorktreeEntry::id_for(&req.repo, &req.task_slug);
        let entry = WorktreeEntry {
            id: id.clone(),
            repo: req.repo,
            repo_root: req.repo_root,
            branch: req.branch,
            avatar: Avatar::from_slug(&req.task_slug),
            name: req.task_slug.clone(),
            task_slug: req.task_slug,
            path,
            session_file: None,
            pr_number: None,
            created_at: req.created_at,
            archived: false,
            agent_kind: req.agent_kind,
            task_id: None,
        };
        self.registry.add(entry)?;
        self.registry.save()?;
        Ok(id)
    }

    /// Attach an entry to a task (registry-only).
    pub fn set_task_id(&mut self, id: &str, task_id: Option<String>) -> Result<(), WorktreeError> {
        self.registry
            .get_mut(id)
            .ok_or_else(|| WorktreeError::NotFound(id.to_string()))?
            .task_id = task_id;
        Ok(())
    }

    /// Archive: remove the worktree dir, keep the branch + registry entry
    /// (marked archived) — the default end-of-task action (§6.1 D6, §6.4).
    pub fn archive(&mut self, id: &str) -> Result<(), WorktreeError> {
        let (repo_root, path) = self.entry_paths(id)?;
        self.git.remove_worktree(&repo_root, &path)?;
        self.registry.mark_archived(id)?;
        self.registry.save()?;
        Ok(())
    }

    /// Delete: destroy the worktree dir, its branch, and the registry entry —
    /// for abandoned experiments, behind a confirm in the UI (§6.1 D6).
    pub fn delete(&mut self, id: &str) -> Result<(), WorktreeError> {
        let entry = self
            .registry
            .get(id)
            .ok_or_else(|| WorktreeError::NotFound(id.to_string()))?
            .clone();
        self.git.remove_worktree(&entry.repo_root, &entry.path)?;
        self.git.delete_branch(&entry.repo_root, &entry.branch)?;
        self.registry.remove(id);
        self.registry.save()?;
        Ok(())
    }

    fn entry_paths(&self, id: &str) -> Result<(PathBuf, PathBuf), WorktreeError> {
        let entry = self
            .registry
            .get(id)
            .ok_or_else(|| WorktreeError::NotFound(id.to_string()))?;
        Ok((entry.repo_root.clone(), entry.path.clone()))
    }
}

/// Convenience: the default-branch merge-base ref for a worktree's diff, i.e.
/// `origin/<default>` when a remote exists, else the local default branch.
pub fn diff_base_ref<R: CommandRunner>(
    git: &GitWorktrees<R>,
    repo_root: &Path,
) -> Result<String, GitError> {
    let default = git.default_branch(repo_root)?;
    Ok(format!("origin/{default}"))
}
