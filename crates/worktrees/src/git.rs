//! Git worktree operations + status/diff, behind the [`CommandRunner`] trait.
//!
//! All git invocations run with `cwd` set to the relevant directory (repo root
//! for worktree management, the worktree itself for status/diff) rather than
//! `-C`, so the scripted test runner sees plain arg lists.
//!
//! Diff base is the merge-base with the repo's default branch (§6.1 D10), so the
//! panel always shows "what this task changes". The diff is taken against the
//! *working tree* (merge-base → working tree), capturing both committed and
//! uncommitted work in the task.

use std::path::Path;

use crate::runner::{CommandOutput, CommandRunner};

/// Aggregate git status for a worktree card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GitStatus {
    /// Number of changed paths in `git status --porcelain` (staged+unstaged).
    pub dirty_files: usize,
    /// Commits on HEAD not on upstream.
    pub ahead: usize,
    /// Commits on upstream not on HEAD.
    pub behind: usize,
}

/// The kind of change for a file in the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
    Other,
}

impl Change {
    /// Single-letter badge for the diff panel (M/A/D/R).
    pub fn badge(self) -> char {
        match self {
            Change::Added => 'A',
            Change::Modified => 'M',
            Change::Deleted => 'D',
            Change::Renamed => 'R',
            Change::Other => '?',
        }
    }

    fn from_name_status(token: &str) -> Self {
        match token.chars().next() {
            Some('A') => Change::Added,
            Some('M') => Change::Modified,
            Some('D') => Change::Deleted,
            Some('R') => Change::Renamed,
            _ => Change::Other,
        }
    }
}

/// One changed file in the diff panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub change: Change,
    pub adds: usize,
    pub dels: usize,
}

/// A worktree's diff against its merge-base.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    /// The merge-base commit the diff is taken against (empty if it could not
    /// be resolved — e.g. a fresh repo with no default branch).
    pub base: String,
    pub files: Vec<DiffFile>,
    /// Full unified diff text (for the scrollable diff view).
    pub unified: String,
}

impl Diff {
    /// Total (adds, dels) across all files, for the panel header.
    pub fn totals(&self) -> (usize, usize) {
        self.files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.adds, d + f.dels))
    }
}

/// One row of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeListEntry {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// Could not start `git` (binary missing, etc.).
    Spawn(String),
    /// `git` ran but exited non-zero.
    Failed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::Spawn(msg) => write!(f, "could not run git: {msg}"),
            GitError::Failed {
                command,
                code,
                stderr,
            } => write!(
                f,
                "git {command} failed (exit {code:?}): {}",
                stderr.trim()
            ),
        }
    }
}

impl std::error::Error for GitError {}

/// Git worktree manager over an injectable [`CommandRunner`].
pub struct GitWorktrees<R: CommandRunner> {
    runner: R,
    git: String,
}

impl<R: CommandRunner> GitWorktrees<R> {
    pub fn new(runner: R) -> Self {
        GitWorktrees {
            runner,
            git: "git".to_string(),
        }
    }

    /// Run git in `cwd`, returning `Ok` only on exit 0.
    fn git_ok(&self, cwd: &Path, args: &[&str]) -> Result<CommandOutput, GitError> {
        let out = self
            .runner
            .run(Some(cwd), &self.git, args)
            .map_err(|e| GitError::Spawn(e.to_string()))?;
        if out.success() {
            Ok(out)
        } else {
            Err(GitError::Failed {
                command: args.join(" "),
                code: out.status,
                stderr: out.stderr,
            })
        }
    }

    // ---- lifecycle (§6.1) ------------------------------------------------

    /// `git worktree add <path> -b <branch> [<base_ref>]` from `repo_root`.
    pub fn add_worktree(
        &self,
        repo_root: &Path,
        path: &Path,
        branch: &str,
        base_ref: Option<&str>,
    ) -> Result<(), GitError> {
        let path_str = path.to_string_lossy();
        let mut args = vec!["worktree", "add", &path_str, "-b", branch];
        if let Some(base) = base_ref {
            args.push(base);
        }
        self.git_ok(repo_root, &args).map(|_| ())
    }

    /// `git worktree remove --force <path>` — removes the worktree dir, leaving
    /// the branch intact (the Archive flow keeps the branch; Delete follows up
    /// with [`delete_branch`]).
    ///
    /// [`delete_branch`]: GitWorktrees::delete_branch
    pub fn remove_worktree(&self, repo_root: &Path, path: &Path) -> Result<(), GitError> {
        let path_str = path.to_string_lossy();
        self.git_ok(repo_root, &["worktree", "remove", "--force", &path_str])
            .map(|_| ())
    }

    /// `git branch -D <branch>` (Delete flow only — §6.1 D6).
    pub fn delete_branch(&self, repo_root: &Path, branch: &str) -> Result<(), GitError> {
        self.git_ok(repo_root, &["branch", "-D", branch]).map(|_| ())
    }

    /// Parse `git worktree list --porcelain`.
    pub fn list_worktrees(&self, repo_root: &Path) -> Result<Vec<WorktreeListEntry>, GitError> {
        let out = self.git_ok(repo_root, &["worktree", "list", "--porcelain"])?;
        Ok(parse_worktree_list(&out.stdout))
    }

    // ---- status / diff ---------------------------------------------------

    /// The main checkout root for a directory (`git rev-parse --show-toplevel`).
    pub fn repo_root(&self, dir: &Path) -> Result<std::path::PathBuf, GitError> {
        let out = self.git_ok(dir, &["rev-parse", "--show-toplevel"])?;
        Ok(std::path::PathBuf::from(out.stdout.trim()))
    }

    /// Whether a ref resolves in this repo (e.g. to prefer `origin/main` over a
    /// local `main` for the diff base).
    pub fn ref_exists(&self, dir: &Path, git_ref: &str) -> bool {
        self.git_ok(dir, &["rev-parse", "--verify", "--quiet", git_ref])
            .is_ok()
    }

    /// Default branch name, from `origin/HEAD` when present, else a local
    /// `main`/`master` probe.
    pub fn default_branch(&self, repo_root: &Path) -> Result<String, GitError> {
        if let Ok(out) =
            self.git_ok(repo_root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        {
            let name = out.stdout.trim();
            if let Some(stripped) = name.strip_prefix("origin/") {
                return Ok(stripped.to_string());
            }
            if !name.is_empty() {
                return Ok(name.to_string());
            }
        }
        for candidate in ["main", "master"] {
            if self
                .git_ok(repo_root, &["rev-parse", "--verify", "--quiet", candidate])
                .is_ok()
            {
                return Ok(candidate.to_string());
            }
        }
        // Last resort: the current branch of the repo root.
        let out = self.git_ok(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        Ok(out.stdout.trim().to_string())
    }

    /// Dirty-file count + ahead/behind. `upstream` is the ref to compare
    /// against (e.g. `origin/main` or `@{upstream}`); when it cannot be
    /// resolved, ahead/behind are reported as 0 rather than erroring.
    pub fn status(&self, worktree: &Path, upstream: Option<&str>) -> Result<GitStatus, GitError> {
        let porcelain = self.git_ok(worktree, &["status", "--porcelain"])?;
        let dirty_files = porcelain.stdout.lines().filter(|l| !l.is_empty()).count();

        let (mut ahead, mut behind) = (0, 0);
        if let Some(upstream) = upstream {
            let range = format!("{upstream}...HEAD");
            if let Ok(out) =
                self.git_ok(worktree, &["rev-list", "--left-right", "--count", &range])
            {
                // "<behind>\t<ahead>" — left = upstream-only, right = HEAD-only.
                let mut nums = out.stdout.split_whitespace();
                behind = nums.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                ahead = nums.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            }
        }

        Ok(GitStatus {
            dirty_files,
            ahead,
            behind,
        })
    }

    /// Diff the worktree against `merge-base(base_ref, HEAD)` (§6.1 D10). Reads
    /// per-file numstat + name-status and the full unified diff. If the merge
    /// base cannot be resolved, returns an empty diff (not an error).
    pub fn diff(&self, worktree: &Path, base_ref: &str) -> Result<Diff, GitError> {
        let base = match self.git_ok(worktree, &["merge-base", base_ref, "HEAD"]) {
            Ok(out) => out.stdout.trim().to_string(),
            Err(_) => return Ok(Diff::default()),
        };
        if base.is_empty() {
            return Ok(Diff::default());
        }

        let numstat = self.git_ok(worktree, &["diff", "--numstat", &base])?;
        let name_status = self.git_ok(worktree, &["diff", "--name-status", &base])?;
        let unified = self.git_ok(worktree, &["diff", &base])?.stdout;

        let files = merge_diff(&numstat.stdout, &name_status.stdout);
        Ok(Diff {
            base,
            files,
            unified,
        })
    }
}

/// Parse `git worktree list --porcelain` (records separated by blank lines).
fn parse_worktree_list(stdout: &str) -> Vec<WorktreeListEntry> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut head: Option<String> = None;

    let mut flush = |path: &mut Option<String>,
                     branch: &mut Option<String>,
                     head: &mut Option<String>| {
        if let Some(p) = path.take() {
            out.push(WorktreeListEntry {
                path: p,
                branch: branch.take(),
                head: head.take(),
            });
        }
    };

    for line in stdout.lines() {
        if line.is_empty() {
            flush(&mut path, &mut branch, &mut head);
        } else if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            // "branch refs/heads/foo" -> "foo"
            branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            head = Some(h.to_string());
        }
    }
    flush(&mut path, &mut branch, &mut head);
    out
}

/// Merge `--numstat` (adds/dels) with `--name-status` (change kind), keyed by
/// path, preserving name-status order.
fn merge_diff(numstat: &str, name_status: &str) -> Vec<DiffFile> {
    // path -> (adds, dels)
    let mut counts: Vec<(String, usize, usize)> = Vec::new();
    for line in numstat.lines() {
        let mut cols = line.split('\t');
        let adds = cols.next();
        let dels = cols.next();
        let path = cols.next();
        if let (Some(adds), Some(dels), Some(path)) = (adds, dels, path) {
            // "-" marks a binary file; count as 0.
            let adds = adds.parse().unwrap_or(0);
            let dels = dels.parse().unwrap_or(0);
            counts.push((path.to_string(), adds, dels));
        }
    }

    let mut files = Vec::new();
    for line in name_status.lines() {
        let mut cols = line.split('\t');
        let status = match cols.next() {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        // For renames (R100 old new) the new path is the last column.
        let path = match cols.clone().last() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let (adds, dels) = counts
            .iter()
            .find(|(p, _, _)| *p == path)
            .map(|(_, a, d)| (*a, *d))
            .unwrap_or((0, 0));
        files.push(DiffFile {
            path,
            change: Change::from_name_status(status),
            adds,
            dels,
        });
    }
    files
}
