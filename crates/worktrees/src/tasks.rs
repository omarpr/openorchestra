//! Task index — project-agnostic unit of work spanning 1..N member worktrees.
//!
//! Persisted at `~/.openorchestra/tasks.json`. Each member remains a
//! [`WorktreeEntry`] in the registry with an optional `task_id`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::registry::{AgentKind, Registry, WorktreeEntry};
use crate::{CommandRunner, CreateRequest, Manager, WorktreeError};

/// One member project inside a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMember {
    pub repo: String,
    pub worktree_id: String,
    pub path: PathBuf,
    pub branch: String,
}

/// A task: one effort, one agent home, one or more repo worktrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub slug: String,
    /// Repo display name of the home member (agent cwd).
    pub home_repo: String,
    pub members: Vec<TaskMember>,
    #[serde(default)]
    pub agent_kind: AgentKind,
    pub created_at: String,
    #[serde(default)]
    pub archived: bool,
}

impl Task {
    pub fn id_for(slug: &str) -> String {
        slug.to_string()
    }

    pub fn home_member(&self) -> Option<&TaskMember> {
        self.members
            .iter()
            .find(|m| m.repo == self.home_repo)
            .or_else(|| self.members.first())
    }

    pub fn home_path(&self) -> Option<&Path> {
        self.home_member().map(|m| m.path.as_path())
    }

    pub fn member_paths(&self) -> Vec<(String, PathBuf)> {
        self.members
            .iter()
            .map(|m| (m.repo.clone(), m.path.clone()))
            .collect()
    }
}

/// Persisted task index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskStore {
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(skip)]
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskError {
    Duplicate(String),
    NotFound(String),
    NoMembers,
    HomeNotInMembers(String),
    Worktree(String),
    Io(String),
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskError::Duplicate(id) => write!(f, "task {id:?} already exists"),
            TaskError::NotFound(id) => write!(f, "task {id:?} not found"),
            TaskError::NoMembers => write!(f, "task needs at least one member project"),
            TaskError::HomeNotInMembers(repo) => {
                write!(f, "home repo {repo:?} is not among selected members")
            }
            TaskError::Worktree(e) => write!(f, "{e}"),
            TaskError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TaskError {}

impl From<WorktreeError> for TaskError {
    fn from(e: WorktreeError) -> Self {
        TaskError::Worktree(e.to_string())
    }
}

impl From<io::Error> for TaskError {
    fn from(e: io::Error) -> Self {
        TaskError::Io(e.to_string())
    }
}

impl TaskStore {
    pub fn new_at(path: impl Into<PathBuf>) -> Self {
        TaskStore {
            tasks: Vec::new(),
            path: path.into(),
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        match fs::read_to_string(&path) {
            Ok(text) => {
                let mut store: TaskStore = serde_json::from_str(&text).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("tasks {path:?} is corrupt: {e}"),
                    )
                })?;
                store.path = path;
                Ok(store)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(TaskStore {
                tasks: Vec::new(),
                path,
            }),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json.as_bytes())?;
        fs::rename(&tmp, &self.path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn add(&mut self, task: Task) -> Result<(), TaskError> {
        if self.get(&task.id).is_some() {
            return Err(TaskError::Duplicate(task.id));
        }
        self.tasks.push(task);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Option<Task> {
        let idx = self.tasks.iter().position(|t| t.id == id)?;
        Some(self.tasks.remove(idx))
    }

    pub fn mark_archived(&mut self, id: &str) -> Result<(), TaskError> {
        self.get_mut(id)
            .ok_or_else(|| TaskError::NotFound(id.to_string()))?
            .archived = true;
        Ok(())
    }

    pub fn rename(&mut self, id: &str, name: impl Into<String>) -> Result<(), TaskError> {
        self.get_mut(id)
            .ok_or_else(|| TaskError::NotFound(id.to_string()))?
            .name = name.into();
        Ok(())
    }

    pub fn active(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|t| !t.archived)
    }

    pub fn archived(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|t| t.archived)
    }
}

/// One project to include when creating a task.
#[derive(Debug, Clone)]
pub struct CreateTaskMember {
    pub repo: String,
    pub repo_root: PathBuf,
    pub branch: String,
    pub base_ref: Option<String>,
}

/// Request to create a multi-member (or single-member) task.
#[derive(Debug, Clone)]
pub struct CreateTaskRequest {
    pub name: String,
    pub slug: String,
    pub home_repo: String,
    pub members: Vec<CreateTaskMember>,
    pub created_at: String,
    pub agent_kind: AgentKind,
}

/// Create git worktrees for each member, register them, and add the task.
pub fn create_task<R: CommandRunner>(
    manager: &mut Manager<R>,
    store: &mut TaskStore,
    req: CreateTaskRequest,
) -> Result<String, TaskError> {
    if req.members.is_empty() {
        return Err(TaskError::NoMembers);
    }
    if !req.members.iter().any(|m| m.repo == req.home_repo) {
        return Err(TaskError::HomeNotInMembers(req.home_repo));
    }
    let task_id = Task::id_for(&req.slug);
    if store.get(&task_id).is_some() {
        return Err(TaskError::Duplicate(task_id));
    }

    let mut members = Vec::new();
    for m in &req.members {
        let wt_req = CreateRequest {
            repo: m.repo.clone(),
            repo_root: m.repo_root.clone(),
            task_slug: req.slug.clone(),
            branch: m.branch.clone(),
            base_ref: m.base_ref.clone(),
            created_at: req.created_at.clone(),
            agent_kind: req.agent_kind.clone(),
        };
        let worktree_id = manager.create(wt_req)?;
        // Attach task_id on the registry entry.
        if let Some(entry) = manager.registry.get_mut(&worktree_id) {
            entry.task_id = Some(task_id.clone());
            entry.name = req.name.clone();
        }
        let entry = manager
            .registry
            .get(&worktree_id)
            .ok_or_else(|| TaskError::Worktree(format!("missing {worktree_id}")))?;
        members.push(TaskMember {
            repo: m.repo.clone(),
            worktree_id: worktree_id.clone(),
            path: entry.path.clone(),
            branch: m.branch.clone(),
        });
    }
    manager.registry.save()?;

    let task = Task {
        id: task_id.clone(),
        name: req.name,
        slug: req.slug,
        home_repo: req.home_repo,
        members,
        agent_kind: req.agent_kind,
        created_at: req.created_at,
        archived: false,
    };
    store.add(task)?;
    store.save()?;
    Ok(task_id)
}

/// Archive every member worktree, then mark the task archived.
pub fn archive_task<R: CommandRunner>(
    manager: &mut Manager<R>,
    store: &mut TaskStore,
    task_id: &str,
) -> Result<(), TaskError> {
    let task = store
        .get(task_id)
        .ok_or_else(|| TaskError::NotFound(task_id.to_string()))?
        .clone();
    for m in &task.members {
        // Member may already be archived.
        if manager.registry.get(&m.worktree_id).map(|e| e.archived) == Some(false) {
            manager.archive(&m.worktree_id)?;
        }
    }
    store.mark_archived(task_id)?;
    store.save()?;
    Ok(())
}

/// Delete every member worktree and remove the task.
pub fn delete_task<R: CommandRunner>(
    manager: &mut Manager<R>,
    store: &mut TaskStore,
    task_id: &str,
) -> Result<(), TaskError> {
    let task = store
        .get(task_id)
        .ok_or_else(|| TaskError::NotFound(task_id.to_string()))?
        .clone();
    for m in &task.members {
        if manager.registry.get(&m.worktree_id).is_some() {
            manager.delete(&m.worktree_id)?;
        }
    }
    store.remove(task_id);
    store.save()?;
    Ok(())
}

/// Migrate registry entries without `task_id` into single-member tasks.
/// Idempotent; never deletes worktree dirs. Returns how many tasks were created.
pub fn migrate_worktrees_to_tasks(
    registry: &mut Registry,
    store: &mut TaskStore,
) -> Result<usize, TaskError> {
    let mut created = 0;
    let entries: Vec<WorktreeEntry> = registry.entries().to_vec();
    for entry in entries {
        if entry.task_id.is_some() {
            continue;
        }
        // Prefer slug as task id; if collision, namespace with full worktree id.
        let mut task_id = Task::id_for(&entry.task_slug);
        if store.get(&task_id).is_some() {
            task_id = entry.id.replace('/', "--");
        }
        if store.get(&task_id).is_some() {
            // Already migrated under another path — just backfill task_id.
            if let Some(e) = registry.get_mut(&entry.id) {
                e.task_id = Some(task_id);
            }
            continue;
        }
        let task = Task {
            id: task_id.clone(),
            name: entry.name.clone(),
            slug: entry.task_slug.clone(),
            home_repo: entry.repo.clone(),
            members: vec![TaskMember {
                repo: entry.repo.clone(),
                worktree_id: entry.id.clone(),
                path: entry.path.clone(),
                branch: entry.branch.clone(),
            }],
            agent_kind: entry.agent_kind.clone(),
            created_at: entry.created_at.clone(),
            archived: entry.archived,
        };
        store.add(task)?;
        if let Some(e) = registry.get_mut(&entry.id) {
            e.task_id = Some(task_id);
        }
        created += 1;
    }
    if created > 0 {
        store.save()?;
        registry.save()?;
    }
    Ok(created)
}

/// Absolute path of the harness-owned task context file for a slug.
///
/// Lives under `~/.openorchestra/tasks/<slug>/agent-context.md` — outside any
/// project worktree, so it never dirties git.
pub fn task_agent_context_path(tasks_root: &Path, slug: &str) -> PathBuf {
    tasks_root.join(slug).join("agent-context.md")
}

/// Brief multi-member task context for agent launch injection.
///
/// Only useful when the task has sibling members; single-member tasks need no
/// coordination file.
pub fn task_agent_context(task: &Task) -> String {
    let mut out = format!("# Task: {}\n", task.name);
    if let Some(home) = task.home_member() {
        out.push_str(&format!(
            "Home (cwd): {} `{}`\n",
            home.repo,
            home.path.display()
        ));
    }
    let siblings: Vec<_> = task
        .members
        .iter()
        .filter(|m| m.repo != task.home_repo)
        .collect();
    if !siblings.is_empty() {
        out.push_str("Also:\n");
        for m in siblings {
            out.push_str(&format!("- {}: `{}`\n", m.repo, m.path.display()));
        }
    }
    out
}

/// Write harness-owned agent context for a multi-member task.
///
/// Returns `Ok(None)` for single-member tasks (no file written). Never writes
/// into a project worktree.
pub fn write_task_agent_context(task: &Task, tasks_root: &Path) -> Result<Option<PathBuf>, TaskError> {
    if task.members.len() < 2 {
        return Ok(None);
    }
    let path = task_agent_context_path(tasks_root, &task.slug);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, task_agent_context(task))?;
    Ok(Some(path))
}

/// Ensure multi-member task context exists; return its path when applicable.
pub fn ensure_task_agent_context(task: &Task, tasks_root: &Path) -> Result<Option<PathBuf>, TaskError> {
    if task.members.len() < 2 {
        return Ok(None);
    }
    let path = task_agent_context_path(tasks_root, &task.slug);
    if path.is_file() {
        return Ok(Some(path));
    }
    write_task_agent_context(task, tasks_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Avatar;

    fn entry(repo: &str, slug: &str) -> WorktreeEntry {
        WorktreeEntry {
            id: WorktreeEntry::id_for(repo, slug),
            repo: repo.to_string(),
            repo_root: PathBuf::from(format!("/repos/{repo}")),
            branch: format!("feat/{slug}"),
            task_slug: slug.to_string(),
            name: slug.to_string(),
            path: PathBuf::from(format!("/wt/{repo}/{slug}")),
            session_file: None,
            avatar: Avatar::from_slug(slug),
            pr_number: None,
            created_at: "2026-07-19T00:00:00Z".to_string(),
            archived: false,
            agent_kind: AgentKind::default(),
            task_id: None,
        }
    }

    #[test]
    fn migrate_creates_one_task_per_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        let tasks_path = dir.path().join("tasks.json");
        let mut reg = Registry::new_at(&reg_path);
        reg.add(entry("app", "nav")).unwrap();
        reg.add(entry("api", "nav")).unwrap(); // same slug, different repo
        let mut store = TaskStore::new_at(&tasks_path);

        let n = migrate_worktrees_to_tasks(&mut reg, &mut store).unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.tasks().len(), 2);
        assert!(reg.get("app/nav").unwrap().task_id.is_some());
        assert!(reg.get("api/nav").unwrap().task_id.is_some());

        // Idempotent.
        let n2 = migrate_worktrees_to_tasks(&mut reg, &mut store).unwrap();
        assert_eq!(n2, 0);
        assert_eq!(store.tasks().len(), 2);
    }

    #[test]
    fn task_agent_context_lists_home_and_siblings() {
        let task = Task {
            id: "auth".into(),
            name: "add auth".into(),
            slug: "auth".into(),
            home_repo: "jam".into(),
            members: vec![
                TaskMember {
                    repo: "jam".into(),
                    worktree_id: "jam/auth".into(),
                    path: PathBuf::from("/wt/jam/auth"),
                    branch: "feat/auth".into(),
                },
                TaskMember {
                    repo: "api".into(),
                    worktree_id: "api/auth".into(),
                    path: PathBuf::from("/wt/api/auth"),
                    branch: "feat/auth".into(),
                },
            ],
            agent_kind: AgentKind::OpenCode,
            created_at: "t".into(),
            archived: false,
        };
        let md = task_agent_context(&task);
        assert!(md.contains("Home (cwd): jam"));
        assert!(md.contains("- api:"));
        assert!(!md.contains("do not commit"));
    }

    #[test]
    fn write_task_agent_context_skips_single_member() {
        let dir = tempfile::tempdir().unwrap();
        let task = Task {
            id: "solo".into(),
            name: "solo".into(),
            slug: "solo".into(),
            home_repo: "jam".into(),
            members: vec![TaskMember {
                repo: "jam".into(),
                worktree_id: "jam/solo".into(),
                path: PathBuf::from("/wt/jam/solo"),
                branch: "feat/solo".into(),
            }],
            agent_kind: AgentKind::default(),
            created_at: "t".into(),
            archived: false,
        };
        assert!(write_task_agent_context(&task, dir.path())
            .unwrap()
            .is_none());
        assert!(!task_agent_context_path(dir.path(), "solo").exists());
    }

    #[test]
    fn write_task_agent_context_writes_outside_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let task = Task {
            id: "auth".into(),
            name: "add auth".into(),
            slug: "auth".into(),
            home_repo: "jam".into(),
            members: vec![
                TaskMember {
                    repo: "jam".into(),
                    worktree_id: "jam/auth".into(),
                    path: PathBuf::from("/wt/jam/auth"),
                    branch: "feat/auth".into(),
                },
                TaskMember {
                    repo: "api".into(),
                    worktree_id: "api/auth".into(),
                    path: PathBuf::from("/wt/api/auth"),
                    branch: "feat/auth".into(),
                },
            ],
            agent_kind: AgentKind::default(),
            created_at: "t".into(),
            archived: false,
        };
        let path = write_task_agent_context(&task, dir.path())
            .unwrap()
            .expect("multi-member writes a file");
        assert_eq!(path, task_agent_context_path(dir.path(), "auth"));
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("- api:"));
    }
}
