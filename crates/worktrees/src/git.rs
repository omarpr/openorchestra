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

use std::path::{Path, PathBuf};

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

/// Whether a branch's commits have reached its remote, for the diff surface's
/// push control.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PushState {
    /// The tracking branch, e.g. `origin/feat-x`. `None` means this branch has
    /// never been published, so pushing it is a `-u` publish rather than an
    /// update.
    pub upstream: Option<String>,
    /// Commits a push would send. Zero means there is nothing to offer.
    pub unpushed: usize,
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
    /// Untracked — git never lists it in a range diff, so fetching its body
    /// needs `--no-index` against `/dev/null` rather than `diff <base>`.
    /// The viewer cannot infer this from [`Change::Added`], which a staged new
    /// file also reports.
    pub untracked: bool,
    /// The path this file had at the base, when it was renamed.
    ///
    /// Needed because bodies are fetched per path: `git diff <base> -- <new>`
    /// alone cannot see a rename — at the base nothing existed under that name,
    /// so the whole file reads as an addition. Both ends have to be named for
    /// git to pair them back up.
    pub renamed_from: Option<String>,
}

/// A worktree's diff against its merge-base.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    /// The merge-base commit the diff is taken against (empty if it could not
    /// be resolved — e.g. a fresh repo with no default branch).
    pub base: String,
    pub files: Vec<DiffFile>,
    /// Untracked files past [`MAX_UNTRACKED_FILES`] that this diff does not
    /// describe. Non-zero means the panel must say so rather than imply the
    /// list is complete.
    pub untracked_elided: usize,
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

/// Markers around the harness's block in `.git/info/exclude`, so repeated
/// calls replace their own entries instead of appending forever, and a human
/// reading the file knows what put them there.
const BLOCK_START: &str = "# >>> openOrchestra (harness-managed; safe to delete) >>>";
const BLOCK_END: &str = "# <<< openOrchestra <<<";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// Could not start `git` (binary missing, etc.).
    Spawn(String),
    /// A filesystem error while reading or writing git metadata.
    Io(String),
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
            GitError::Io(msg) => write!(f, "git metadata io error: {msg}"),
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

    pub fn runner(&self) -> &R {
        &self.runner
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

    /// `git worktree add -b <branch> -- <path> [<base_ref>]` from `repo_root`.
    pub fn add_worktree(
        &self,
        repo_root: &Path,
        path: &Path,
        branch: &str,
        base_ref: Option<&str>,
    ) -> Result<(), GitError> {
        let path_str = path.to_string_lossy();
        // `--` keeps caller-influenced positionals from ever parsing as flags.
        // `--no-track`: the base is a remote-tracking ref (`origin/<default>`),
        // and git's default `branch.autoSetupMerge` would make the new branch
        // track *it* — leaving every task branch claiming `origin/master` as
        // its upstream. See `push_state` for what that breaks.
        let mut args = vec!["worktree", "add", "--no-track", "-b", branch, "--", &path_str];
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
        self.git_ok(repo_root, &["worktree", "remove", "--force", "--", &path_str])
            .map(|_| ())
    }

    /// `git branch -D <branch>` (Delete flow only — §6.1 D6).
    pub fn delete_branch(&self, repo_root: &Path, branch: &str) -> Result<(), GitError> {
        self.git_ok(repo_root, &["branch", "-D", "--", branch]).map(|_| ())
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

    /// Add `patterns` to the repo's local exclude file, so harness-written
    /// files never show up as changes in the user's project.
    ///
    /// Uses `.git/info/exclude` rather than the repo's tracked `.gitignore`:
    /// the harness must not edit a file the user commits. Entries are wrapped
    /// in a marked block so a later call can replace them cleanly, and the
    /// whole thing is idempotent.
    ///
    /// **The path is the git *common* dir, not the worktree's git dir.**
    /// Verified against real git: writing to a linked worktree's own
    /// `$GIT_DIR/info/exclude` has **no effect** — git only reads
    /// `$GIT_COMMON_DIR/info/exclude`. One consequence worth knowing: the
    /// exclusions apply to the main checkout and every worktree of that repo,
    /// which is what we want (one entry covers every worktree the harness will
    /// ever create there).
    pub fn exclude_locally(&self, dir: &Path, patterns: &[&str]) -> Result<(), GitError> {
        if patterns.is_empty() {
            return Ok(());
        }
        let common = self.git_ok(dir, &["rev-parse", "--git-common-dir"])?;
        let common = common.stdout.trim();
        // `--git-common-dir` may be relative to `dir` (it is plain `.git` for a
        // primary checkout), so resolve it against the directory we asked in.
        let common = if Path::new(common).is_absolute() {
            std::path::PathBuf::from(common)
        } else {
            dir.join(common)
        };

        let info = common.join("info");
        let exclude = info.join("exclude");
        let existing = std::fs::read_to_string(&exclude).unwrap_or_default();

        let mut kept: Vec<&str> = Vec::new();
        let mut inside = false;
        for line in existing.lines() {
            if line == BLOCK_START {
                inside = true;
                continue;
            }
            if line == BLOCK_END {
                inside = false;
                continue;
            }
            if !inside {
                kept.push(line);
            }
        }

        let mut out = kept.join("\n");
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(BLOCK_START);
        out.push('\n');
        for pattern in patterns {
            out.push_str(pattern);
            out.push('\n');
        }
        out.push_str(BLOCK_END);
        out.push('\n');

        std::fs::create_dir_all(&info).map_err(|e| GitError::Io(e.to_string()))?;
        // Atomic, like every other write in this codebase: git may be reading
        // this file concurrently, and a truncated exclude file would start
        // showing spurious changes.
        let tmp = info.join(".exclude.harness.tmp");
        std::fs::write(&tmp, out).map_err(|e| GitError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &exclude).map_err(|e| GitError::Io(e.to_string()))?;
        Ok(())
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

    /// The branch currently checked out in `dir`'s HEAD, e.g. `master` or a
    /// feature branch. Returns `HEAD` for a detached head — the same string
    /// `git rev-parse --abbrev-ref HEAD` yields — so callers can render it
    /// verbatim without a separate detached-state probe.
    pub fn current_branch(&self, dir: &Path) -> Result<String, GitError> {
        let out = self.git_ok(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
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

    /// Where this branch stands against its **own** tracking branch.
    ///
    /// Deliberately not [`GitStatus::ahead`], which counts against the repo's
    /// default branch: a worktree on `feat/x` is legitimately 3 ahead of
    /// `origin/main` while being fully pushed to `origin/feat/x`, and a push
    /// button driven by that number would offer to push nothing. Only
    /// `@{upstream}` answers "is this work on the remote yet".
    ///
    /// With no upstream — a branch created here and never published — the count
    /// falls back to commits not on `fallback` (the repo's `origin/<default>`),
    /// which is exactly what publishing would send.
    pub fn push_state(&self, worktree: &Path, fallback: &str) -> PushState {
        let upstream = self
            .git_ok(
                worktree,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
            )
            .ok()
            .map(|o| o.stdout.trim().to_string())
            .filter(|s| !s.is_empty());

        // An upstream only counts as *published* when it is this branch's own
        // name on the remote. A worktree branched off `origin/<default>`
        // inherits that branch as its upstream — git's `branch.autoSetupMerge`
        // default — so `feat/x` comes into the world tracking `origin/master`.
        // Treating that as published makes the caller send a bare `git push`,
        // which under `push.default = simple` fails with "the upstream branch
        // of your current branch does not match the name of your current
        // branch", and under `upstream`/`matching` would quietly land the
        // task's commits on the default branch. Reporting `None` routes it to
        // the publish path instead, which sets the upstream correctly.
        //
        // `branch.<b>.merge` is the exact test. Comparing the abbreviated names
        // would call a branch `x` tracking `origin/feat/x` a match.
        let upstream = upstream.filter(|_| {
            let Ok(branch) = self.current_branch(worktree) else {
                return false;
            };
            self.git_ok(worktree, &["config", "--get", &format!("branch.{branch}.merge")])
                .is_ok_and(|o| upstream_is_own_branch(o.stdout.trim(), &branch))
        });

        let base = upstream.clone().unwrap_or_else(|| fallback.to_string());
        let range = format!("{base}..HEAD");
        let unpushed = self
            .git_ok(worktree, &["rev-list", "--count", &range])
            .ok()
            .and_then(|o| o.stdout.trim().parse().ok())
            .unwrap_or(0);

        PushState { upstream, unpushed }
    }

    /// The remote a never-published branch should be published to: `origin` when
    /// it exists, else the first remote configured. `None` means the repo has no
    /// remote at all, and the caller must not offer to publish.
    pub fn default_remote(&self, worktree: &Path) -> Option<String> {
        let out = self.git_ok(worktree, &["remote"]).ok()?;
        let names: Vec<&str> = out.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        names
            .iter()
            .find(|n| **n == "origin")
            .or_else(|| names.first())
            .map(|n| n.to_string())
    }

    /// `git push`, or `git push -u <remote> <branch>` when `publish_to` names a
    /// remote — the first publication of a branch has to say where it goes and
    /// record the upstream, or the next push asks the same question again.
    ///
    /// Returns git's stderr, which is where it writes the human-readable result
    /// ("Everything up-to-date", the new-branch hint, the rejection reason), so
    /// the caller can show what actually happened rather than a bare success.
    pub fn push(
        &self,
        worktree: &Path,
        publish_to: Option<(&str, &str)>,
    ) -> Result<String, GitError> {
        let out = match publish_to {
            // Fully-qualified refspec rather than a bare branch name: `git push
            // -u <remote> <branch>` still consults `push.default`, and on a
            // branch that inherited someone else's upstream that is how commits
            // end up on the wrong remote branch. `HEAD:refs/heads/<branch>` can
            // only ever mean this branch, there.
            Some((remote, branch)) => {
                let refspec = format!("HEAD:refs/heads/{branch}");
                self.git_ok(worktree, &["push", "-u", remote, &refspec])?
            }
            None => self.git_ok(worktree, &["push"])?,
        };
        Ok(out.stderr.trim().to_string())
    }

    /// Diff the worktree against `merge-base(base_ref, HEAD)` (§6.1 D10).
    ///
    /// **Summary only** — per-file numstat + name-status, no bodies. Reading
    /// the full unified text here is what made a large branch expensive: a
    /// changeset of tens of thousands of lines was fetched, held in memory, and
    /// re-split per file, all to draw the one file on screen. Bodies come from
    /// [`file_diff`](Self::file_diff), one file at a time, when the viewer asks.
    ///
    /// If the merge base cannot be resolved, returns an empty diff (not an
    /// error).
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

        let files = merge_diff(&numstat.stdout, &name_status.stdout);
        Ok(Diff {
            base,
            files,
            untracked_elided: 0,
        })
    }

    /// One file's unified diff, fetched on demand.
    ///
    /// `base` is the *resolved* merge-base from [`Diff::base`], not a ref: the
    /// range was already pinned when the summary was computed, and re-resolving
    /// it here would let a commit landing mid-review silently move the goalposts
    /// between the file list and the body beside it.
    ///
    /// An untracked file has no `base` side at all, so it goes through
    /// `--no-index` against `/dev/null`, which exits 1 to mean "these differ" —
    /// the expected outcome — and so cannot go through `git_ok`.
    ///
    /// `renamed_from` names the file's path at the base, and must be passed for
    /// a renamed file or the diff comes back as a whole-file addition.
    pub fn file_diff(
        &self,
        worktree: &Path,
        base: &str,
        path: &str,
        untracked: bool,
        renamed_from: Option<&str>,
    ) -> String {
        if untracked {
            return self
                .runner
                .run(
                    Some(worktree),
                    &self.git,
                    &["diff", "--no-index", "--", "/dev/null", path],
                )
                .map(|o| o.stdout)
                .unwrap_or_default();
        }
        if base.is_empty() {
            return String::new();
        }
        // Both ends named, so rename detection has a pair to work with; git
        // needs `--find-renames` explicitly once the pathspec is narrowed.
        let mut args = vec!["diff", base];
        if renamed_from.is_some() {
            args.push("--find-renames");
        }
        args.push("--");
        if let Some(old) = renamed_from {
            args.push(old);
        }
        args.push(path);
        self.git_ok(worktree, &args)
            .map(|o| o.stdout)
            .unwrap_or_default()
    }

    /// Paths git can see but is not tracking, with `.gitignore` respected.
    /// Sorted, so the diff's file list does not reshuffle between refreshes.
    pub fn untracked_files(&self, worktree: &Path) -> Vec<PathBuf> {
        let Ok(out) = self.git_ok(worktree, &["ls-files", "--others", "--exclude-standard"]) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        paths.sort();
        paths
    }

    /// The summary row for one untracked file: its path and how many lines it
    /// adds.
    ///
    /// Counted by reading the file rather than by shelling out to
    /// `git diff --no-index`, which is what this used to do. The output of that
    /// command was thrown away except for its line count, and a hundred
    /// untracked files meant a hundred process spawns before the panel could
    /// draw anything. The body still comes from git, via
    /// [`file_diff`](Self::file_diff), if and when the file is opened.
    ///
    /// A NUL byte in the first block means binary, git's own heuristic; binary
    /// files report zero adds, exactly as `git diff` does.
    fn untracked_summary(&self, worktree: &Path, path: &Path) -> Option<DiffFile> {
        let bytes = std::fs::read(worktree.join(path)).ok()?;
        let binary = bytes.iter().take(8000).any(|b| *b == 0);
        let adds = if binary {
            0
        } else {
            // A trailing newline terminates the last line rather than starting
            // an empty one, which is also how git counts.
            bytes.iter().filter(|b| **b == b'\n').count()
                + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
        };
        Some(DiffFile {
            path: path.to_string_lossy().into_owned(),
            change: Change::Added,
            adds,
            dels: 0,
            untracked: true,
            renamed_from: None,
        })
    }

    /// [`diff`](Self::diff) plus the untracked files, which `git diff` never
    /// lists.
    ///
    /// Both ends of the range are the working tree, and a file you just created
    /// is part of it — without this, the panel's default view would omit brand
    /// new files until they were staged, which is precisely when you most want
    /// to see them. The index is never touched: `git add -N` would produce this
    /// for free, but a viewer that stages your files as a side effect of being
    /// looked at is not one you can trust.
    pub fn diff_including_untracked(
        &self,
        worktree: &Path,
        base_ref: &str,
    ) -> Result<Diff, GitError> {
        let mut diff = self.diff(worktree, base_ref)?;
        let untracked = self.untracked_files(worktree);
        let shown = untracked.len().min(MAX_UNTRACKED_FILES);
        for path in &untracked[..shown] {
            if let Some(file) = self.untracked_summary(worktree, path) {
                diff.files.push(file);
            }
        }
        diff.untracked_elided = untracked.len() - shown;
        Ok(diff)
    }
}

/// How many untracked files a diff will describe before it stops and reports
/// the remainder. An un-ignored `node_modules` or build directory runs to
/// thousands, and each file costs its own `git diff --no-index`.
pub const MAX_UNTRACKED_FILES: usize = 100;

/// Whether a `branch.<name>.merge` value names the branch's own counterpart on
/// the remote, rather than some other branch it was created from.
fn upstream_is_own_branch(merge_ref: &str, branch: &str) -> bool {
    merge_ref == format!("refs/heads/{branch}")
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
        // For renames (R100 old new) the new path is the last column, and what
        // remains before it is the old one — which a per-path body fetch needs
        // in order to see the rename at all.
        let path = match cols.next_back() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let renamed_from = cols.next().map(str::to_string).filter(|p| !p.is_empty());
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
            // Everything `git diff` itself lists is tracked by definition;
            // untracked rows are appended by `diff_including_untracked`.
            untracked: false,
            renamed_from,
        });
    }
    files
}
