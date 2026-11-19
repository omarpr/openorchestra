//! training — session-exhaust harvesting (architecture §6.4, roadmap M7).
//!
//! This is the step that makes the project pay for itself. Every worktree
//! session is an append-only JSONL tree we control the reading of; archived
//! sessions plus their final diffs plus a merged/abandoned outcome label are
//! the raw material for the next training round.
//!
//! The pipeline:
//!
//! ```text
//!   registry (sessionFile + archived)   forge poller (PR state)   git (merge-base diff)
//!            \                                  |                        /
//!             `---------------> HarvestRequest <'------------------------'
//!                                     |
//!                              TrainingRecord  (stamped with the §4.4 schema version)
//!                                     |
//!                                 JSONL out
//! ```
//!
//! Two deliberate properties:
//!
//! - **Outcome labels are honest.** A session with no PR and no archive flag is
//!   `Unknown`, not optimistically `Abandoned`. Mislabeled outcomes are worse
//!   than missing ones — they teach the model the wrong lesson.
//! - **Records are schema-versioned.** Each carries the `agent-schema` version
//!   in force when it was harvested, so a training set can never be silently
//!   mixed across a breaking toolset change (§4.4, R5).
//!
//! Headless and UI-free like the other backend crates, so harvesting can run
//! as a batch job with no window.

mod session;

pub use session::{
    parse_session, read_session, Session, SessionError, ToolCall, Turn, SUPPORTED_FORMAT_VERSION,
};

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use worktrees::{Diff, WorktreeEntry};

/// How a task ended — the label the training round actually optimizes for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The PR merged. The strongest positive signal we have.
    Merged,
    /// The PR was closed unmerged, or the worktree was archived with no PR.
    Abandoned,
    /// A PR is still open — the outcome is not known yet, so it is not a
    /// label. Excluded from training sets by default.
    Open,
    /// No evidence either way.
    Unknown,
}

impl Outcome {
    /// Derive from the forge's PR state plus the registry's archived flag.
    ///
    /// `pr_state` is `None` when the branch has no PR — which is genuinely
    /// ambiguous: an archived worktree with no PR was abandoned, but an active
    /// one may simply not have been pushed yet.
    pub fn derive(pr_state: Option<forge::PrState>, archived: bool) -> Outcome {
        match pr_state {
            Some(forge::PrState::Merged) => Outcome::Merged,
            Some(forge::PrState::Closed) => Outcome::Abandoned,
            Some(forge::PrState::Open | forge::PrState::Draft) => Outcome::Open,
            None if archived => Outcome::Abandoned,
            None => Outcome::Unknown,
        }
    }

    /// Whether this outcome is a usable training label.
    pub fn is_labeled(self) -> bool {
        matches!(self, Outcome::Merged | Outcome::Abandoned)
    }
}

/// Where a record came from, so any row can be traced back to its worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub worktree_id: String,
    pub repo: String,
    pub branch: String,
    pub task_slug: String,
    pub created_at: String,
    pub agent_kind: String,
    pub pr_number: Option<u64>,
    pub session_id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// A summary of the final diff — what the session actually produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub base: String,
    pub files_changed: usize,
    pub adds: usize,
    pub dels: usize,
    pub paths: Vec<String>,
    /// Full unified diff. Large, but it *is* the supervision signal.
    pub unified: String,
}

impl DiffSummary {
    /// Build a summary from a [`Diff`] and the full unified text.
    ///
    /// `unified` is passed in rather than read off the diff because
    /// [`worktrees::Diff`] no longer carries bodies — the viewer fetches them
    /// one file at a time, so a whole-changeset text now has to be assembled
    /// deliberately. That is the right shape for a harvester anyway: the
    /// supervision signal is worth paying for explicitly, and a summary that
    /// silently carried an empty `unified` would poison the dataset in a way
    /// nothing downstream could detect.
    pub fn new(diff: &Diff, unified: String) -> Self {
        let (adds, dels) = diff.totals();
        DiffSummary {
            base: diff.base.clone(),
            files_changed: diff.files.len(),
            adds,
            dels,
            paths: diff.files.iter().map(|f| f.path.clone()).collect(),
            unified,
        }
    }
}

/// One harvested training row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainingRecord {
    /// The `agent-schema` version in force at harvest time (§4.4).
    pub schema_version: String,
    /// The pi session-format version the transcript was parsed as.
    pub session_format_version: u32,
    pub outcome: Outcome,
    pub provenance: Provenance,
    pub diff: Option<DiffSummary>,
    pub turns: Vec<Turn>,
    pub stats: Stats,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub turn_count: usize,
    pub tool_call_count: usize,
    pub had_tool_errors: bool,
}

/// Everything needed to harvest one worktree.
#[derive(Debug)]
pub struct HarvestRequest<'a> {
    pub entry: &'a WorktreeEntry,
    /// PR state from the forge poller; `None` when there is no PR (or `gh` is
    /// unavailable — in which case the label degrades to `Unknown`, not a
    /// guess).
    pub pr_state: Option<forge::PrState>,
    /// The worktree's final merge-base diff, when it could still be computed.
    /// Archived worktrees have no directory left, so this is often `None` —
    /// which is why archiving keeps the branch (§6.1 D6).
    ///
    /// Built by the caller via [`DiffSummary::new`], which is also where the
    /// full unified text is supplied.
    pub diff: Option<DiffSummary>,
}

#[derive(Debug)]
pub enum HarvestError {
    /// The registry entry records no session file — nothing to harvest.
    NoSessionFile(String),
    Session(SessionError),
}

impl std::fmt::Display for HarvestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarvestError::NoSessionFile(id) => {
                write!(f, "worktree {id:?} has no recorded session file")
            }
            HarvestError::Session(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HarvestError {}

impl From<SessionError> for HarvestError {
    fn from(e: SessionError) -> Self {
        HarvestError::Session(e)
    }
}

/// Harvest one worktree into a training record.
pub fn harvest(req: &HarvestRequest<'_>) -> Result<TrainingRecord, HarvestError> {
    let session_path = req
        .entry
        .session_file
        .as_ref()
        .ok_or_else(|| HarvestError::NoSessionFile(req.entry.id.clone()))?;
    let session = read_session(session_path)?;
    Ok(build_record(req, session))
}

/// Assemble a record from an already-parsed session (the testable core).
pub fn build_record(req: &HarvestRequest<'_>, session: Session) -> TrainingRecord {
    let stats = Stats {
        turn_count: session.turns.len(),
        tool_call_count: session.tool_call_count(),
        had_tool_errors: session.had_tool_errors(),
    };
    TrainingRecord {
        schema_version: agent_schema::schema().schema_version,
        session_format_version: session.format_version,
        outcome: Outcome::derive(req.pr_state, req.entry.archived),
        provenance: Provenance {
            worktree_id: req.entry.id.clone(),
            repo: req.entry.repo.clone(),
            branch: req.entry.branch.clone(),
            task_slug: req.entry.task_slug.clone(),
            created_at: req.entry.created_at.clone(),
            agent_kind: req.entry.agent_kind.as_str().to_string(),
            pr_number: req.entry.pr_number,
            session_id: session.id,
            provider: session.provider,
            model: session.model,
        },
        diff: req.diff.clone(),
        turns: session.turns,
        stats,
    }
}

/// Only records with a decided outcome are training data; `Open` and `Unknown`
/// are provenance, not supervision.
pub fn labeled(records: Vec<TrainingRecord>) -> Vec<TrainingRecord> {
    records
        .into_iter()
        .filter(|r| r.outcome.is_labeled())
        .collect()
}

/// Write records as JSONL (one compact object per line).
pub fn write_jsonl(records: &[TrainingRecord], path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Atomic, like every other write in this codebase: a half-written training
    // set that still parses is worse than one that obviously failed.
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        for record in records {
            let line = serde_json::to_string(record)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            writeln!(out, "{line}")?;
        }
        out.flush()?;
    }
    std::fs::rename(&tmp, path)
}
