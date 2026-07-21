//! forge — PR and CI status for worktree branches, behind a [`Forge`] trait
//! (architecture §6.3). The v1 backend is [`GhCli`], which shells out to the
//! GitHub CLI:
//!
//! ```text
//! gh pr view  <branch> --json number,state,isDraft,title
//! gh pr checks <branch> --json name,state,bucket
//! ```
//!
//! Field names verified against the gh manual (cli.github.com/manual):
//! - `gh pr view --json` exposes `number`, `state` (`OPEN`/`CLOSED`/`MERGED`),
//!   `isDraft`, `title` (among others).
//! - `gh pr checks --json` exposes `bucket`, `completedAt`, `description`,
//!   `event`, `link`, `name`, `startedAt`, `state`, `workflow`; `bucket`
//!   "categorizes the state field into pass, fail, pending, skipping, or
//!   cancel". `gh pr checks` exits 8 while checks are pending and non-zero
//!   when checks fail, so exit codes are ignored whenever stdout parses.
//!
//! R7 requirements: a missing `gh` binary is detected at construction and
//! surfaced via [`Forge::available`] (callers hide badges); malformed output
//! never panics; [`Poller`] polls with jitter and per-branch backoff.

mod gh;
mod poller;
mod runner;

pub use gh::GhCli;
pub use poller::{BranchKey, BranchStatus, ForgeUpdate, Poller, PollerConfig};
pub use runner::{CommandOutput, CommandRunner, SystemRunner};

use std::path::Path;

/// PR lifecycle state, as shown on the sidebar PR badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

/// A pull request associated with a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub state: PrState,
    pub title: String,
}

/// Status of a single CI check, as shown on the sidebar CI indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail,
    Running,
    Skipped,
}

/// One CI check run for a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub name: String,
    pub status: CheckStatus,
}

/// Errors from a forge backend. Callers treat any `Err` as "data temporarily
/// unavailable" (keep last known badges, back off); they never panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    /// Backend tooling is not installed / not usable (e.g. no `gh` binary).
    Unavailable,
    /// The command ran but failed for a reason other than "no PR found".
    CommandFailed { message: String },
    /// The command produced output we could not parse.
    Parse { message: String },
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeError::Unavailable => write!(f, "forge backend unavailable"),
            ForgeError::CommandFailed { message } => write!(f, "forge command failed: {message}"),
            ForgeError::Parse { message } => write!(f, "forge output unparsable: {message}"),
        }
    }
}

impl std::error::Error for ForgeError {}

/// A source of PR + CI data for branches. v1 ships [`GhCli`]; a direct-API or
/// multi-forge backend can replace it without UI changes (§6.3).
pub trait Forge: Send + Sync {
    /// Whether the backend can serve requests at all. When `false`, callers
    /// hide PR/CI badges entirely (R7) and should not bother polling.
    fn available(&self) -> bool;

    /// The PR associated with `branch`, or `None` if the branch has no PR.
    fn pr_for_branch(&self, repo_dir: &Path, branch: &str)
        -> Result<Option<PrInfo>, ForgeError>;

    /// CI check runs for `branch`'s PR. A branch with a PR but no configured
    /// checks yields `Ok(vec![])`.
    fn checks_for_branch(&self, repo_dir: &Path, branch: &str)
        -> Result<Vec<CheckRun>, ForgeError>;

    /// Open a pull request for `branch` (§12 cross-cutting, D3-adjacent).
    ///
    /// The only *mutating* forge call, so it is deliberately explicit rather
    /// than something a poller could ever trigger: creating a PR is
    /// outward-facing and must stay a thing the user asked for.
    fn create_pr(&self, repo_dir: &Path, req: &CreatePrRequest)
        -> Result<PrInfo, ForgeError>;
}

/// A request to open a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePrRequest {
    pub branch: String,
    pub title: String,
    pub body: String,
    /// Base branch; `None` lets gh pick the repo's default.
    pub base: Option<String>,
    pub draft: bool,
}
