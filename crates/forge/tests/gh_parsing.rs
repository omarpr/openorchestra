//! Parsing tests for the GhCli backend against recorded gh output fixtures.
//!
//! gh is NOT installed in CI for these tests — a `FakeRunner` plays back
//! canned stdout/stderr/exit codes.
//!
//! `fixtures/` holds hand-authored output covering every branch of the parser;
//! `fixtures/live/` holds *verbatim* output captured from gh 2.95.0 against
//! public repos, which is what retires the "fixture-verified only" half of R7
//! (see docs/m1-gh-verification.md). The hand-authored shapes were confirmed
//! byte-for-byte compatible with the live capture during that pass.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use forge::{
    CheckStatus, CommandOutput, CommandRunner, CreatePrRequest, Forge, ForgeError, GhCli,
    PrState,
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

const VIEW_ARGS: &str =
    "pr list --head feat/nav --state all --json number,state,isDraft,title";
const CHECKS_ARGS: &str = "pr checks --json name,state,bucket -- feat/nav";

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
    // `gh pr list --head` reports "no PR" as an empty array on exit 0 — no
    // stderr-string matching required, unlike `gh pr view`.
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(VIEW_ARGS, 0, "[]", ""));
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
    // gh's human output exits 8 while checks are pending (and 1 when any check
    // fails). With `--json` it exits 0 either way — verified against gh 2.95.0,
    // docs/m1-gh-verification.md §4 — but the parser must not depend on either
    // convention, so feed it the pessimistic case.
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

// ---- live capture from gh 2.95.0 (M1 / R7) ---------------------------------
//
// Verbatim stdout from a real, authenticated gh against public repos. These
// exist so a future gh output change breaks a test instead of a badge.

const LIVE_PR_OPEN: &str = include_str!("../fixtures/live/pr_list_head_open.json");
const LIVE_PR_DRAFT: &str = include_str!("../fixtures/live/pr_list_head_draft.json");
const LIVE_PR_MERGED: &str = include_str!("../fixtures/live/pr_list_head_merged.json");
const LIVE_PR_CLOSED: &str = include_str!("../fixtures/live/pr_list_head_closed.json");
/// A PR opened from a **fork** — the case `gh pr view <branch>` cannot see at
/// all (M1 §5). This fixture is why `pr_for_branch` uses `gh pr list --head`.
const LIVE_PR_FORK: &str = include_str!("../fixtures/live/pr_list_head_fork.json");
/// `gh pr list --head` on a branch with no PR: an empty array, exit 0.
const LIVE_PR_NONE: &str = include_str!("../fixtures/live/pr_list_head_none.json");
const LIVE_CHECKS_MIXED: &str =
    include_str!("../fixtures/live/pr_checks_pass_fail_skipping.json");
const LIVE_CHECKS_PENDING: &str = include_str!("../fixtures/live/pr_checks_pending.json");

fn live_pr(stdout: &str) -> forge::PrInfo {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(VIEW_ARGS, 0, stdout, ""));
    gh.pr_for_branch(repo(), "feat/nav").unwrap().unwrap()
}

fn live_checks(stdout: &str) -> Vec<CheckStatus> {
    // gh exits 0 for --json regardless of check outcome (verification §4).
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(CHECKS_ARGS, 0, stdout, ""));
    gh.checks_for_branch(repo(), "feat/nav")
        .unwrap()
        .into_iter()
        .map(|c| c.status)
        .collect()
}

#[test]
fn live_pr_list_output_resolves_a_fork_head_pr() {
    // The regression this locks in: `gh pr view <branch>` returns "no pull
    // requests found" for a PR opened from a fork, so a fork-pushed worktree
    // branch silently showed no badge. `gh pr list --head` finds it.
    let pr = live_pr(LIVE_PR_FORK);
    assert_eq!(pr.number, 13927);
    assert_eq!(pr.state, PrState::Open);
}

#[test]
fn live_empty_pr_list_is_no_pr_not_an_error() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(VIEW_ARGS, 0, LIVE_PR_NONE, ""));
    assert_eq!(gh.pr_for_branch(repo(), "feat/nav").unwrap(), None);
}

#[test]
fn an_open_pr_wins_over_an_older_closed_one_on_the_same_branch() {
    // A branch can carry several PRs over its life; the badge should show the
    // live one, not whichever happens to come first.
    let both = r#"[{"number":10,"state":"CLOSED","isDraft":false,"title":"old"},
                   {"number":4,"state":"OPEN","isDraft":false,"title":"live"}]"#;
    let pr = live_pr(both);
    assert_eq!(pr.state, PrState::Open);
    assert_eq!(pr.number, 4);
}

#[test]
fn with_no_open_pr_the_most_recent_wins() {
    let both = r#"[{"number":4,"state":"CLOSED","isDraft":false,"title":"older"},
                   {"number":10,"state":"MERGED","isDraft":false,"title":"newer"}]"#;
    let pr = live_pr(both);
    assert_eq!(pr.number, 10);
    assert_eq!(pr.state, PrState::Merged);
}

#[test]
fn live_pr_view_output_maps_to_every_pr_state() {
    assert_eq!(live_pr(LIVE_PR_OPEN).state, PrState::Open);
    assert_eq!(live_pr(LIVE_PR_DRAFT).state, PrState::Draft);
    assert_eq!(live_pr(LIVE_PR_MERGED).state, PrState::Merged);
    assert_eq!(live_pr(LIVE_PR_CLOSED).state, PrState::Closed);
    // Numbers and titles survive too, not just the state discriminant.
    let open = live_pr(LIVE_PR_OPEN);
    assert_eq!(open.number, 13894);
    assert!(!open.title.is_empty());
}

#[test]
fn live_pr_checks_output_covers_pass_fail_skipping_and_pending() {
    let mixed = live_checks(LIVE_CHECKS_MIXED);
    assert!(mixed.contains(&CheckStatus::Pass));
    assert!(mixed.contains(&CheckStatus::Fail));
    assert!(mixed.contains(&CheckStatus::Skipped));

    let pending = live_checks(LIVE_CHECKS_PENDING);
    assert!(pending.contains(&CheckStatus::Running));
    assert!(pending.contains(&CheckStatus::Pass));
}

// ---- create_pr (cross-cutting, D3-adjacent) --------------------------------

fn create_request() -> CreatePrRequest {
    CreatePrRequest {
        branch: "feat/nav".into(),
        title: "Add worktree navigation sidebar".into(),
        body: "body".into(),
        base: Some("main".into()),
        draft: false,
    }
}

const CREATE_ARGS: &str = "pr create --head feat/nav --title Add worktree navigation sidebar \
                           --body body --base main";

#[test]
fn create_pr_reports_the_created_pr() {
    // gh prints a URL, not JSON, so the backend re-reads the branch — that way
    // the returned PrInfo comes from the same path the badges use.
    let gh = GhCli::with_runner(
        FakeRunner::new(true)
            .ok(CREATE_ARGS, 0, "https://github.com/o/r/pull/128\n", "")
            .ok(VIEW_ARGS, 0, PR_OPEN, ""),
    );
    let pr = gh.create_pr(repo(), &create_request()).unwrap();
    assert_eq!(pr.number, 128);
    assert_eq!(pr.state, PrState::Open);
}

#[test]
fn create_pr_surfaces_gh_failures() {
    let gh = GhCli::with_runner(FakeRunner::new(true).ok(
        CREATE_ARGS,
        1,
        "",
        "pull request create failed: GraphQL: No commits between main and feat/nav\n",
    ));
    let err = gh.create_pr(repo(), &create_request()).unwrap_err();
    assert!(
        matches!(&err, ForgeError::CommandFailed { message } if message.contains("No commits")),
        "{err:?}"
    );
}

#[test]
fn create_pr_passes_every_user_value_as_a_flag_argument() {
    // Arg-injection guard: a branch or title starting with `-` must never be
    // able to become an option. There are no positionals in this command.
    struct Capture(std::sync::Mutex<Vec<String>>);
    impl CommandRunner for Capture {
        fn run(&self, _: Option<&Path>, _: &str, args: &[&str]) -> io::Result<CommandOutput> {
            if args == ["--version"] {
                return Ok(CommandOutput { status: Some(0), stdout: "gh 2".into(), stderr: String::new() });
            }
            self.0.lock().unwrap().extend(args.iter().map(|a| a.to_string()));
            Ok(CommandOutput { status: Some(0), stdout: "[]".into(), stderr: String::new() })
        }
    }
    let gh = GhCli::with_runner(Capture(std::sync::Mutex::new(Vec::new())));
    let mut req = create_request();
    req.branch = "--not-a-flag".into();
    req.title = "-also-not".into();
    req.draft = true;
    // Errors because the follow-up read finds no PR; the arg shape is the point.
    let _ = gh.create_pr(repo(), &req);

    let gh_args = gh.runner().0.lock().unwrap().clone();
    let head = gh_args.iter().position(|a| a == "--head").expect("--head present");
    assert_eq!(gh_args[head + 1], "--not-a-flag", "branch is a flag *value*");
    let title = gh_args.iter().position(|a| a == "--title").expect("--title present");
    assert_eq!(gh_args[title + 1], "-also-not");
    assert!(gh_args.contains(&"--draft".to_string()));
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
