//! On-disk worktree registry (architecture §6.1).
//!
//! The registry is the harness's index of worktrees: it maps each worktree to
//! its repo, branch, session file, avatar, PR number, and lifecycle state.
//! Renames and avatar edits mutate the registry only — never git state (§6.1).
//!
//! Persistence is an atomic JSON file (write-temp + rename) so a crash mid-write
//! never corrupts the index. Unknown fields are tolerated and defaults fill
//! missing ones, so the schema can grow without breaking old files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Number of avatar swatches in the design palette (§3.4).
pub const AVATAR_PALETTE_LEN: u8 = 8;

/// Which coding agent a worktree runs (§4.5, D12).
///
/// This lives here rather than in `agent-runtime` because it is *registry
/// schema* — persisted per worktree and read back on every load — and keeping
/// it here lets `agent-runtime` depend on `worktrees` without a cycle.
///
/// Serialized as a plain string. Unrecognized values are preserved verbatim in
/// [`AgentKind::Other`] rather than collapsed to a catch-all variant: an older
/// build must never rewrite a newer build's `"kimi"` as `"unknown"` and silently
/// repoint the worktree at a different CLI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AgentKind {
    /// The reference runtime — the one we train (§4.4).
    #[default]
    Pi,
    OpenCode,
    Codex,
    ClaudeCode,
    Kimi,
    /// An agent this build does not know about. Round-trips unchanged.
    Other(String),
}

impl AgentKind {
    pub fn as_str(&self) -> &str {
        match self {
            AgentKind::Pi => "pi",
            AgentKind::OpenCode => "opencode",
            AgentKind::Codex => "codex",
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Kimi => "kimi",
            AgentKind::Other(s) => s,
        }
    }

    /// Every kind this build knows how to drive, in sidebar order.
    pub fn known() -> [AgentKind; 5] {
        [
            AgentKind::Pi,
            AgentKind::OpenCode,
            AgentKind::Codex,
            AgentKind::ClaudeCode,
            AgentKind::Kimi,
        ]
    }
}

impl From<&str> for AgentKind {
    fn from(s: &str) -> Self {
        match s {
            "pi" => AgentKind::Pi,
            "opencode" => AgentKind::OpenCode,
            "codex" => AgentKind::Codex,
            "claude-code" => AgentKind::ClaudeCode,
            "kimi" => AgentKind::Kimi,
            other => AgentKind::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for AgentKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(AgentKind::from(String::deserialize(d)?.as_str()))
    }
}

/// A worktree card's avatar: 2-letter initials + a palette color index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Avatar {
    pub initials: String,
    /// Index into the 8-swatch palette (`0..AVATAR_PALETTE_LEN`).
    pub color: u8,
}

impl Avatar {
    /// Derive initials + a stable color from a task slug (deterministic, so the
    /// same slug always gets the same avatar until the user edits it).
    pub fn from_slug(slug: &str) -> Self {
        Avatar {
            initials: derive_initials(slug),
            color: (fnv1a(slug) % AVATAR_PALETTE_LEN as u32) as u8,
        }
    }
}

/// One worktree in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeEntry {
    /// Stable id: `"<repo>/<task_slug>"`. Unique within the registry.
    pub id: String,
    /// Repo display name (the main checkout's directory name).
    pub repo: String,
    /// Absolute path to the repo's main checkout.
    pub repo_root: PathBuf,
    pub branch: String,
    pub task_slug: String,
    /// Editable display name (defaults to the slug). Mutating this is registry-
    /// only; it never touches git.
    pub name: String,
    /// Absolute path to the worktree directory.
    pub path: PathBuf,
    #[serde(default)]
    pub session_file: Option<PathBuf>,
    pub avatar: Avatar,
    #[serde(default)]
    pub pr_number: Option<u64>,
    /// ISO-8601 creation timestamp (supplied by the caller so the core stays
    /// deterministic and testable — no clock in this crate).
    pub created_at: String,
    /// Archived worktrees keep their branch + session but have no live dir
    /// (§6.1). Kept in the registry as training-data provenance (§6.4).
    #[serde(default)]
    pub archived: bool,
    /// Which agent CLI this worktree runs (§4.5). Additive: registries written
    /// before this field existed load as [`AgentKind::Pi`].
    #[serde(default)]
    pub agent_kind: AgentKind,
}

impl WorktreeEntry {
    pub fn id_for(repo: &str, task_slug: &str) -> String {
        format!("{repo}/{task_slug}")
    }
}

/// The persisted index. Load with [`Registry::load`], mutate, then [`save`].
///
/// [`save`]: Registry::save
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    entries: Vec<WorktreeEntry>,
    #[serde(skip)]
    path: PathBuf,
}

impl Registry {
    /// An empty registry that will persist to `path` on [`save`]. Use as a
    /// fallback when [`load`] fails on a corrupt file but you still want a
    /// working (path-bound) registry.
    ///
    /// [`save`]: Registry::save
    /// [`load`]: Registry::load
    pub fn new_at(path: impl Into<PathBuf>) -> Self {
        Registry {
            entries: Vec::new(),
            path: path.into(),
        }
    }

    /// Load the registry from `path`. A missing file yields an empty registry
    /// (first run); a present-but-unparsable file is an error rather than
    /// silent data loss.
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        match fs::read_to_string(&path) {
            Ok(text) => {
                let mut registry: Registry = serde_json::from_str(&text).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("registry {path:?} is corrupt: {e}"),
                    )
                })?;
                registry.path = path;
                Ok(registry)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Registry {
                entries: Vec::new(),
                path,
            }),
            Err(e) => Err(e),
        }
    }

    /// Atomically persist the registry (write temp + rename).
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

    pub fn entries(&self) -> &[WorktreeEntry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&WorktreeEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut WorktreeEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    /// Insert a new entry. Returns an error if the id already exists.
    pub fn add(&mut self, entry: WorktreeEntry) -> Result<(), RegistryError> {
        if self.get(&entry.id).is_some() {
            return Err(RegistryError::Duplicate(entry.id));
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Remove an entry entirely (Delete flow — §6.1 D6).
    pub fn remove(&mut self, id: &str) -> Option<WorktreeEntry> {
        let idx = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(idx))
    }

    /// Mark an entry archived without removing it (Archive flow — §6.1 D6).
    pub fn mark_archived(&mut self, id: &str) -> Result<(), RegistryError> {
        self.get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?
            .archived = true;
        Ok(())
    }

    /// Registry-only rename (never touches git — §6.1).
    pub fn rename(&mut self, id: &str, name: impl Into<String>) -> Result<(), RegistryError> {
        self.get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?
            .name = name.into();
        Ok(())
    }

    /// Registry-only avatar edit (never touches git — §6.1).
    pub fn set_avatar(&mut self, id: &str, avatar: Avatar) -> Result<(), RegistryError> {
        self.get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?
            .avatar = avatar;
        Ok(())
    }

    pub fn set_session_file(
        &mut self,
        id: &str,
        session_file: Option<PathBuf>,
    ) -> Result<(), RegistryError> {
        self.get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?
            .session_file = session_file;
        Ok(())
    }

    pub fn set_pr_number(&mut self, id: &str, pr: Option<u64>) -> Result<(), RegistryError> {
        self.get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?
            .pr_number = pr;
        Ok(())
    }

    pub fn active(&self) -> impl Iterator<Item = &WorktreeEntry> {
        self.entries.iter().filter(|e| !e.archived)
    }

    pub fn archived(&self) -> impl Iterator<Item = &WorktreeEntry> {
        self.entries.iter().filter(|e| e.archived)
    }

    /// Group active entries by repo, preserving first-seen order (for the
    /// repo-grouped sidebar, D4). Returns `(repo, repo_root, entries)`.
    pub fn grouped_by_repo(&self) -> Vec<RepoGroup<'_>> {
        let mut groups: Vec<RepoGroup> = Vec::new();
        for entry in self.active() {
            match groups.iter_mut().find(|g| g.repo == entry.repo) {
                Some(group) => group.entries.push(entry),
                None => groups.push(RepoGroup {
                    repo: &entry.repo,
                    repo_root: &entry.repo_root,
                    entries: vec![entry],
                }),
            }
        }
        groups
    }
}

/// A repo and its active worktrees, for the grouped sidebar.
#[derive(Debug)]
pub struct RepoGroup<'a> {
    pub repo: &'a str,
    pub repo_root: &'a Path,
    pub entries: Vec<&'a WorktreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    Duplicate(String),
    NotFound(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Duplicate(id) => write!(f, "worktree {id:?} already in registry"),
            RegistryError::NotFound(id) => write!(f, "worktree {id:?} not in registry"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Two-letter initials from a slug: first letters of the first two
/// `-`/`_`/space-separated words, else the first two chars, uppercased.
fn derive_initials(slug: &str) -> String {
    let words: Vec<&str> = slug
        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect();

    let initials: String = match words.len() {
        0 => "wt".to_string(),
        1 => words[0].chars().take(2).collect(),
        _ => words
            .iter()
            .take(2)
            .filter_map(|w| w.chars().next())
            .collect(),
    };
    initials.to_uppercase()
}

/// FNV-1a over the slug bytes — a tiny stable hash for avatar color (no `rand`
/// dep, matching the forge crate's no-extra-deps discipline).
fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in s.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}
