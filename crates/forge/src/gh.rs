//! GitHub backend: shells out to the `gh` CLI and parses its `--json` output.

use std::path::Path;

use serde::Deserialize;

use crate::runner::{CommandOutput, CommandRunner, SystemRunner};
use crate::{CheckRun, CheckStatus, Forge, ForgeError, PrInfo, PrState};

/// `gh`-CLI-backed [`Forge`]. Reuses the user's `gh auth login` — zero auth
/// work in the harness (§6.3).
pub struct GhCli<R: CommandRunner = SystemRunner> {
    runner: R,
    available: bool,
}

impl GhCli<SystemRunner> {
    /// Probe for a usable `gh` binary (`gh --version`). A missing binary is
    /// not an error: the instance is constructed with `available() == false`
    /// and every data call returns [`ForgeError::Unavailable`] (R7).
    pub fn new() -> Self {
        Self::with_runner(SystemRunner)
    }
}

impl Default for GhCli<SystemRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: CommandRunner> GhCli<R> {
    /// Construct with a custom runner (tests inject recorded output here).
    /// Probes `gh --version` through the runner.
    pub fn with_runner(runner: R) -> Self {
        let available = matches!(
            runner.run(None, "gh", &["--version"]),
            Ok(ref out) if out.success()
        );
        GhCli { runner, available }
    }

    fn ensure_available(&self) -> Result<(), ForgeError> {
        if self.available {
            Ok(())
        } else {
            Err(ForgeError::Unavailable)
        }
    }

    fn run_gh(&self, repo_dir: &Path, args: &[&str]) -> Result<CommandOutput, ForgeError> {
        self.runner
            .run(Some(repo_dir), "gh", args)
            .map_err(|e| ForgeError::CommandFailed {
                message: format!("failed to spawn gh: {e}"),
            })
    }
}

/// Wire shape of `gh pr view --json number,state,isDraft,title`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrView {
    number: u64,
    /// `OPEN` | `CLOSED` | `MERGED`
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    title: String,
}

/// Wire shape of one element of `gh pr checks --json name,state,bucket`.
#[derive(Deserialize)]
struct GhCheck {
    #[serde(default)]
    name: String,
    /// Raw state, e.g. `SUCCESS`, `FAILURE`, `IN_PROGRESS`, `QUEUED`, ...
    #[serde(default)]
    state: String,
    /// `pass` | `fail` | `pending` | `skipping` | `cancel`
    #[serde(default)]
    bucket: String,
}

/// gh's stderr when a branch has no PR (`gh pr view`/`gh pr checks`):
/// `no pull requests found for branch "<name>"`.
fn is_no_pr_error(stderr: &str) -> bool {
    stderr.contains("no pull requests found")
}

/// gh's stderr when a PR exists but has no checks:
/// `no checks reported on the '<branch>' branch`.
fn is_no_checks_error(stderr: &str) -> bool {
    stderr.contains("no checks reported")
}

fn map_pr_state(state: &str, is_draft: bool) -> Result<PrState, ForgeError> {
    match state {
        "OPEN" if is_draft => Ok(PrState::Draft),
        "OPEN" => Ok(PrState::Open),
        "MERGED" => Ok(PrState::Merged),
        "CLOSED" => Ok(PrState::Closed),
        other => Err(ForgeError::Parse {
            message: format!("unknown PR state {other:?}"),
        }),
    }
}

fn map_check(check: &GhCheck) -> CheckStatus {
    match check.bucket.as_str() {
        "pass" => CheckStatus::Pass,
        "fail" | "cancel" => CheckStatus::Fail,
        "pending" => CheckStatus::Running,
        "skipping" => CheckStatus::Skipped,
        // Unknown bucket (format drift, R7): fall back to the raw state.
        _ => match check.state.as_str() {
            "SUCCESS" | "COMPLETED" => CheckStatus::Pass,
            "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED"
            | "STARTUP_FAILURE" => CheckStatus::Fail,
            "SKIPPED" | "NEUTRAL" => CheckStatus::Skipped,
            _ => CheckStatus::Running,
        },
    }
}

impl<R: CommandRunner> Forge for GhCli<R> {
    fn available(&self) -> bool {
        self.available
    }

    fn pr_for_branch(
        &self,
        repo_dir: &Path,
        branch: &str,
    ) -> Result<Option<PrInfo>, ForgeError> {
        self.ensure_available()?;
        let out = self.run_gh(
            repo_dir,
            &["pr", "view", branch, "--json", "number,state,isDraft,title"],
        )?;

        if !out.success() {
            if is_no_pr_error(&out.stderr) {
                return Ok(None);
            }
            return Err(ForgeError::CommandFailed {
                message: format!(
                    "gh pr view exited with {:?}: {}",
                    out.status,
                    out.stderr.trim()
                ),
            });
        }

        let view: GhPrView =
            serde_json::from_str(&out.stdout).map_err(|e| ForgeError::Parse {
                message: format!("gh pr view output: {e}"),
            })?;
        Ok(Some(PrInfo {
            number: view.number,
            state: map_pr_state(&view.state, view.is_draft)?,
            title: view.title,
        }))
    }

    fn checks_for_branch(
        &self,
        repo_dir: &Path,
        branch: &str,
    ) -> Result<Vec<CheckRun>, ForgeError> {
        self.ensure_available()?;
        let out = self.run_gh(
            repo_dir,
            &["pr", "checks", branch, "--json", "name,state,bucket"],
        )?;

        // `gh pr checks` uses exit codes as signal (8 = pending, non-zero on
        // failing checks), so prefer parsing stdout whenever it is JSON.
        if let Ok(checks) = serde_json::from_str::<Vec<GhCheck>>(&out.stdout) {
            return Ok(checks
                .iter()
                .map(|c| CheckRun {
                    name: c.name.clone(),
                    status: map_check(c),
                })
                .collect());
        }

        if is_no_pr_error(&out.stderr) || is_no_checks_error(&out.stderr) {
            return Ok(Vec::new());
        }

        if !out.success() {
            return Err(ForgeError::CommandFailed {
                message: format!(
                    "gh pr checks exited with {:?}: {}",
                    out.status,
                    out.stderr.trim()
                ),
            });
        }

        Err(ForgeError::Parse {
            message: "gh pr checks output was not a JSON array".to_string(),
        })
    }
}
