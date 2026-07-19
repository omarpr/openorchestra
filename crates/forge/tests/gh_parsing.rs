//! Parsing tests for the GhCli backend against recorded gh output fixtures.
//!
//! gh is NOT installed in CI for these tests — a `FakeRunner` plays back
//! canned stdout/stderr/exit codes whose shapes were taken from the gh manual
//! (`gh pr view --json`, `gh pr checks --json`; see docs/spike2-notes.md).

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use forge::{
    CheckStatus, CommandOutput, CommandRunner, Forge, ForgeError, GhCli, PrState,
};

const PR_OPEN: &str = include_str!("../fixtures/pr_open.json");
const PR_DRAFT: &str = include_str!("../fixtures/pr_draft.json");
const PR_MERGED: &str = include_str!("../fixtures/pr_merged.json");
const PR_CLOSED: &str = include_str!("../fixtures/pr_closed.json");
const CHECKS_MIXED: &str = include_str!("../fixtures/checks_mixed.json");
const CHECKS_PASS: &str = include_str!("../fixtures/checks_pass.json");
const CHECKS_UNKNOWN_BUCKET: &str = include_str!("../fixtures/checks_unknown_bucket.json");
const GARBAGE: &str = include_str!("../fixtures/garbage.txt");

/// Plays back recorded outputs keyed by the joined argument list.
struct FakeRunner {
    responses: Mutex<HashMap<String, io::Result<CommandOutput>>>,
    /// When true, `gh --version` succeeds (gh "installed").
    gh_present: bool,
}

impl FakeRunner {
    fn new(gh_present: bool) -> Self {
        FakeRunner {
            responses: Mutex::new(HashMap::new()),
            gh_present,
        }
    }

    fn record(self, args: &str, out: io::Result<CommandOutput>) -> Self {
        self.responses.lock().unwrap().insert(args.to_string(), out);
        self
    }

    fn ok(self, args: &str, code: i32, stdout: &str, stderr: &str) -> Self {
        self.record(
            args,
            Ok(CommandOutput {
                status: Some(code),
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            }),
        )
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, _cwd: Option<&Path>, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        assert_eq!(program, "gh", "only gh should ever be invoked");
        if args == ["--version"] {
            return if self.gh_present {
                Ok(CommandOutput {
                    status: Some(0),
                    stdout: "gh version 2.62.0 (2026-01-01)\n".into(),
                    stderr: String::new(),
                })
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "gh: command not found"))
            };
        }
        let key = args.join(" ");
        self.responses
            .lock()
            .unwrap()
            .remove(&key)
            .unwrap_or_else(|| panic!("unexpected command: gh {key}"))
    }
}

fn repo() -> &'static Path {
    Path::new("/repos/omar-harness")
}

const VIEW_ARGS: &str = "pr view feat/nav --json number,state,isDraft,title";
const CHECKS_ARGS: &str = "pr checks feat/nav --json name,state,bucket";

// ---- pr_for_branch ---------------------------------------------------------

#[test]
fn open_pr_parses() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(VIEW_ARGS, 0, PR_OPEN, ""));
    let pr = gh.pr_for_branch(repo(), "feat/nav").unwrap().unwrap();
    assert_eq!(pr.number, 128);
    assert_eq!(pr.state, PrState::Open);
    assert_eq!(pr.title, "Add worktree navigation sidebar");
}

#[test]
fn draft_pr_parses() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(VIEW_ARGS, 0, PR_DRAFT, ""));
    let pr = gh.pr_for_branch(repo(), "feat/nav").unwrap().unwrap();
    assert_eq!(pr.number, 131);
    assert_eq!(pr.state, PrState::Draft);
}

#[test]
fn merged_pr_parses() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(VIEW_ARGS, 0, PR_MERGED, ""));
    let pr = gh.pr_for_branch(repo(), "feat/nav").unwrap().unwrap();
    assert_eq!(pr.state, PrState::Merged);
}

#[test]
fn closed_pr_parses() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(VIEW_ARGS, 0, PR_CLOSED, ""));
    let pr = gh.pr_for_branch(repo(), "feat/nav").unwrap().unwrap();
    assert_eq!(pr.state, PrState::Closed);
}

#[test]
fn no_pr_found_is_none() {
    // Real gh stderr for a branch without a PR, exit code 1.
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(
        VIEW_ARGS,
        1,
        "",
        "no pull requests found for branch \"feat/nav\"\n",
    ));
    assert_eq!(gh.pr_for_branch(repo(), "feat/nav").unwrap(), None);
}

#[test]
fn other_gh_failure_is_command_failed() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(
        VIEW_ARGS,
        1,
        "",
        "HTTP 401: Bad credentials (https://api.github.com/graphql)\n",
    ));
    assert!(matches!(
        gh.pr_for_branch(repo(), "feat/nav"),
        Err(ForgeError::CommandFailed { .. })
    ));
}

#[test]
fn garbage_pr_output_is_parse_error_not_panic() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(VIEW_ARGS, 0, GARBAGE, ""));
    assert!(matches!(
        gh.pr_for_branch(repo(), "feat/nav"),
        Err(ForgeError::Parse { .. })
    ));
}

#[test]
fn unknown_pr_state_is_parse_error() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(
        VIEW_ARGS,
        0,
        r#"{"number":1,"state":"LOCKED","isDraft":false,"title":"x"}"#,
        "",
    ));
    assert!(matches!(
        gh.pr_for_branch(repo(), "feat/nav"),
        Err(ForgeError::Parse { .. })
    ));
}

// ---- checks_for_branch -----------------------------------------------------

#[test]
fn mixed_checks_parse_with_bucket_mapping() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(CHECKS_ARGS, 1, CHECKS_MIXED, ""));
    let checks = gh.checks_for_branch(repo(), "feat/nav").unwrap();
    let by_name: HashMap<&str, CheckStatus> =
        checks.iter().map(|c| (c.name.as_str(), c.status)).collect();
    assert_eq!(by_name["build (ubuntu-latest)"], CheckStatus::Pass);
    assert_eq!(by_name["test (macos-latest)"], CheckStatus::Fail);
    assert_eq!(by_name["clippy"], CheckStatus::Running);
    assert_eq!(by_name["deploy-preview"], CheckStatus::Running);
    assert_eq!(by_name["docs"], CheckStatus::Skipped);
    assert_eq!(by_name["nightly"], CheckStatus::Fail); // bucket "cancel"
    assert_eq!(checks.len(), 6);
}

#[test]
fn passing_checks_parse_even_with_pending_exit_code_8() {
    // gh pr checks exits 8 while checks are pending; stdout still parses.
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(CHECKS_ARGS, 8, CHECKS_PASS, ""));
    let checks = gh.checks_for_branch(repo(), "feat/nav").unwrap();
    assert_eq!(checks.len(), 2);
    assert!(checks.iter().all(|c| c.status == CheckStatus::Pass));
}

#[test]
fn unknown_bucket_falls_back_to_state() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(CHECKS_ARGS, 0, CHECKS_UNKNOWN_BUCKET, ""));
    let checks = gh.checks_for_branch(repo(), "feat/nav").unwrap();
    assert_eq!(checks[0].status, CheckStatus::Pass); // state SUCCESS
    assert_eq!(checks[1].status, CheckStatus::Running); // unknown everything
}

#[test]
fn no_checks_reported_is_empty() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(
        CHECKS_ARGS,
        1,
        "",
        "no checks reported on the 'feat/nav' branch\n",
    ));
    assert_eq!(gh.checks_for_branch(repo(), "feat/nav").unwrap(), vec![]);
}

#[test]
fn no_pr_for_checks_is_empty() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(
        CHECKS_ARGS,
        1,
        "",
        "no pull requests found for branch \"feat/nav\"\n",
    ));
    assert_eq!(gh.checks_for_branch(repo(), "feat/nav").unwrap(), vec![]);
}

#[test]
fn garbage_checks_output_is_error_not_panic() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(CHECKS_ARGS, 0, GARBAGE, ""));
    assert!(gh.checks_for_branch(repo(), "feat/nav").is_err());
}

// ---- availability (R7) -----------------------------------------------------

#[test]
fn missing_gh_binary_degrades_gracefully() {
    let gh = GhCli::with_runner(FakeRunner::new(false));
    assert!(!gh.available());
    assert_eq!(
        gh.pr_for_branch(repo(), "feat/nav"),
        Err(ForgeError::Unavailable)
    );
    assert_eq!(
        gh.checks_for_branch(repo(), "feat/nav"),
        Err(ForgeError::Unavailable)
    );
}

#[test]
fn present_gh_binary_reports_available() {
    let gh = GhCli::with_runner(FakeRunner::new(true));
    assert!(gh.available());
}
