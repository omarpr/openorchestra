//! GitHub backend: shells out to the `gh` CLI and parses its `--json` output.

use std::path::Path;

use serde::Deserialize;

use crate::runner::{CommandOutput, CommandRunner, SystemRunner};
use crate::{CheckRun, CheckStatus, CreatePrRequest, Forge, ForgeError, PrInfo, PrState};

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

    /// The injected runner (tests assert on the exact argv it received).
    pub fn runner(&self) -> &R {
        &self.runner
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

/// Wire shape of one element of `gh pr list --head <branch> --author @me
/// --state all --json number,state,isDraft,title,url`.
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
    url: Option<String>,
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

/// gh's stderr when a branch has no PR (`gh pr checks`):
/// `no pull requests found for branch "<name>"`.
///
/// Still needed for `pr checks`; `pr for_branch` no longer relies on it, since
/// `pr list` reports "none" as an empty array.
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

    fn pr_for_branch(&self, repo_dir: &Path, branch: &str) -> Result<Option<PrInfo>, ForgeError> {
        self.ensure_available()?;
        // `gh pr list --head` rather than `gh pr view <branch>`: the latter
        // only resolves PRs whose head branch lives in the *base* repo, so a
        // fork-pushed worktree branch silently shows no badge (M1 §5, verified
        // against gh 2.95.0). `--author @me` keeps the list to the signed-in
        // user, including a pull request they opened from a fork, and leaves
        // out anyone else's pull request on the same head. `pr list` reports
        // "no PR" as an empty array on exit 0, so the fragile stderr-string
        // match goes away too. `--state all` stays: a merged or closed pull
        // request is still returned for outcome labeling. The sidebar poller
        // is what hides those from the badge.
        let out = self.run_gh(
            repo_dir,
            &[
                "pr",
                "list",
                "--head",
                branch,
                "--author",
                "@me",
                "--state",
                "all",
                "--json",
                "number,state,isDraft,title,url",
            ],
        )?;

        if !out.success() {
            if is_no_pr_error(&out.stderr) {
                return Ok(None);
            }
            return Err(ForgeError::CommandFailed {
                message: format!(
                    "gh pr list exited with {:?}: {}",
                    out.status,
                    out.stderr.trim()
                ),
            });
        }

        let prs: Vec<GhPrView> =
            serde_json::from_str(&out.stdout).map_err(|e| ForgeError::Parse {
                message: format!("gh pr list output: {e}"),
            })?;

        // A branch can carry several PRs over its life (reopened, or closed then
        // superseded). Prefer a live one — that is what the badge is about —
        // and otherwise the most recent, which is the highest number.
        let Some(chosen) = prs
            .iter()
            .find(|pr| pr.state == "OPEN")
            .or_else(|| prs.iter().max_by_key(|pr| pr.number))
        else {
            return Ok(None);
        };

        Ok(Some(PrInfo {
            number: chosen.number,
            state: map_pr_state(&chosen.state, chosen.is_draft)?,
            title: chosen.title.clone(),
            url: chosen
                .url
                .clone()
                .filter(|url| url.starts_with("https://") || url.starts_with("http://")),
        }))
    }

    fn create_pr(&self, repo_dir: &Path, req: &CreatePrRequest) -> Result<PrInfo, ForgeError> {
        self.ensure_available()?;

        // Every user-supplied value goes through `--flag value` rather than
        // being interpolated, and there are no positionals at all, so a branch
        // or title beginning with `-` cannot be read as an option (the same
        // arg-injection discipline as the `--` terminators elsewhere).
        let mut args: Vec<&str> = vec![
            "pr",
            "create",
            "--head",
            &req.branch,
            "--title",
            &req.title,
            "--body",
            &req.body,
        ];
        if let Some(base) = &req.base {
            args.extend_from_slice(&["--base", base]);
        }
        if req.draft {
            args.push("--draft");
        }

        let out = self.run_gh(repo_dir, &args)?;
        if !out.success() {
            return Err(ForgeError::CommandFailed {
                message: format!(
                    "gh pr create exited with {:?}: {}",
                    out.status,
                    out.stderr.trim()
                ),
            });
        }

        // `gh pr create` prints the new PR's URL, not JSON. Rather than parse
        // the URL, re-read the branch so the returned PrInfo comes from the
        // same code path the badges use — one source of truth for PR state.
        self.pr_for_branch(repo_dir, &req.branch)?
            .ok_or_else(|| ForgeError::Parse {
                message: format!(
                    "gh pr create succeeded but no PR is visible for {:?}",
                    req.branch
                ),
            })
    }

    fn checks_for_branch(
        &self,
        repo_dir: &Path,
        branch: &str,
    ) -> Result<Vec<CheckRun>, ForgeError> {
        self.ensure_available()?;
        let out = self.run_gh(
            repo_dir,
            &["pr", "checks", "--json", "name,state,bucket", "--", branch],
        )?;

        // Parse stdout first whenever it is JSON. Verified against gh 2.95.0
        // (docs/m1-gh-verification.md §4): with `--json`, gh exits 0 regardless
        // of check outcome — its 1-means-failing / 8-means-pending convention
        // applies to the *human* output only. So the status tells us nothing
        // about the checks, and a non-zero status here means a real error.
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
