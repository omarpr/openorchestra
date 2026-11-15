//! M7 tests — session parsing and harvesting (architecture §6.4).
//!
//! `fixtures/pi-session-v3.jsonl` is a sanitized capture from
//! `@mariozechner/pi-coding-agent@0.73.1`, not a hand-written approximation.
//! Local paths and model labels are replaced with generic fixture values.

use std::path::{Path, PathBuf};

use forge::PrState;
use training::{
    build_record, harvest, labeled, parse_session, read_session, HarvestRequest, Outcome,
    SessionError, Turn, SUPPORTED_FORMAT_VERSION,
};
use worktrees::{AgentKind, Avatar, Diff, DiffFile, Change, WorktreeEntry};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name)
}

fn entry() -> WorktreeEntry {
    WorktreeEntry {
        id: "app/nav".into(),
        repo: "app".into(),
        repo_root: "/repos/app".into(),
        branch: "feat/nav".into(),
        task_slug: "nav".into(),
        name: "nav".into(),
        path: "/wt/app/nav".into(),
        session_file: Some(fixture("pi-session-v3.jsonl")),
        avatar: Avatar::from_slug("nav"),
        pr_number: Some(128),
        created_at: "2026-07-19T00:00:00Z".into(),
        archived: true,
        agent_kind: AgentKind::Pi,
        task_id: None,
    }
}

// ---- session parsing against a real capture --------------------------------

#[test]
fn parses_a_real_pi_session() {
    let s = read_session(&fixture("pi-session-v3.jsonl")).unwrap();
    assert_eq!(s.format_version, SUPPORTED_FORMAT_VERSION);
    assert!(!s.id.is_empty());
    assert_eq!(s.provider.as_deref(), Some("lmstudio"));
    assert_eq!(s.model.as_deref(), Some("test-model"));

    // user -> assistant(toolCall) -> toolResult -> assistant(text)
    assert_eq!(s.turns.len(), 4, "{:#?}", s.turns);
    assert!(matches!(&s.turns[0], Turn::User { text } if text == "run the probe"));

    match &s.turns[1] {
        Turn::Assistant { tool_calls, stop_reason, .. } => {
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].name, "bash");
            assert_eq!(tool_calls[0].arguments["command"], "echo harness-lifecycle-probe");
            assert_eq!(stop_reason.as_deref(), Some("toolUse"));
        }
        other => panic!("expected an assistant tool call, got {other:?}"),
    }

    match &s.turns[2] {
        Turn::ToolResult { tool_name, text, is_error, .. } => {
            assert_eq!(tool_name, "bash");
            assert!(text.contains("harness-lifecycle-probe"));
            assert!(!is_error);
        }
        other => panic!("expected a tool result, got {other:?}"),
    }

    assert!(matches!(&s.turns[3], Turn::Assistant { text: Some(t), .. } if t == "Probe complete."));
    assert_eq!(s.tool_call_count(), 1);
    assert!(!s.had_tool_errors());
}

#[test]
fn resolves_the_active_branch_of_a_forked_session() {
    // The format is a tree: /fork branches it. Reading records in file order
    // would splice an abandoned branch into the transcript and train the model
    // on a path the user explicitly walked away from.
    let forked = r#"
{"type":"session","version":3,"id":"s1","timestamp":"2026-07-21T00:00:00Z","cwd":"/w"}
{"type":"message","id":"m1","parentId":null,"message":{"role":"user","content":[{"type":"text","text":"first"}]}}
{"type":"message","id":"m2","parentId":"m1","message":{"role":"assistant","content":[{"type":"text","text":"ABANDONED BRANCH"}],"stopReason":"stop"}}
{"type":"message","id":"m3","parentId":"m1","message":{"role":"assistant","content":[{"type":"text","text":"kept branch"}],"stopReason":"stop"}}
{"type":"message","id":"m4","parentId":"m3","message":{"role":"user","content":[{"type":"text","text":"second"}]}}
"#;
    let s = parse_session(forked, Path::new("forked.jsonl")).unwrap();
    let texts: Vec<String> = s
        .turns
        .iter()
        .map(|t| match t {
            Turn::User { text } => text.clone(),
            Turn::Assistant { text, .. } => text.clone().unwrap_or_default(),
            Turn::ToolResult { text, .. } => text.clone(),
        })
        .collect();
    assert_eq!(texts, vec!["first", "kept branch", "second"]);
    assert!(
        !texts.iter().any(|t| t.contains("ABANDONED")),
        "the dead branch must not appear in the transcript"
    );
}

#[test]
fn refuses_an_unsupported_session_format_version() {
    // Guessing at an unknown format would poison the training set silently;
    // failing loudly is the only safe option.
    let future = r#"{"type":"session","version":99,"id":"s1","timestamp":"t","cwd":"/w"}"#;
    let err = parse_session(future, Path::new("future.jsonl")).unwrap_err();
    assert!(matches!(err, SessionError::UnsupportedVersion { version: 99, .. }));
    assert!(err.to_string().contains("v99"));
}

#[test]
fn rejects_a_file_that_is_not_a_session() {
    let err = parse_session("{\"hello\":\"world\"}", Path::new("nope.jsonl")).unwrap_err();
    assert!(matches!(err, SessionError::NotASession(_)));
}

#[test]
fn tolerates_a_truncated_trailing_line() {
    // A session cut short by a crash is still worth harvesting up to the break.
    let truncated = r#"
{"type":"session","version":3,"id":"s1","timestamp":"t","cwd":"/w"}
{"type":"message","id":"m1","parentId":null,"message":{"role":"user","content":[{"type":"text","text":"hi"}]}}
{"type":"message","id":"m2","parentId":"m1","message":{"role":"assist
"#;
    let s = parse_session(truncated, Path::new("t.jsonl")).unwrap();
    assert_eq!(s.turns.len(), 1);
}

#[test]
fn a_parent_cycle_terminates_instead_of_hanging() {
    // Defensive: a corrupt file must not wedge a batch harvest.
    let cyclic = r#"
{"type":"session","version":3,"id":"s1","timestamp":"t","cwd":"/w"}
{"type":"message","id":"a","parentId":"b","message":{"role":"user","content":[{"type":"text","text":"a"}]}}
{"type":"message","id":"b","parentId":"a","message":{"role":"user","content":[{"type":"text","text":"b"}]}}
"#;
    let s = parse_session(cyclic, Path::new("cyclic.jsonl")).unwrap();
    // Each record appears at most once — a plain iteration cap would still
    // splice the same turns in twice before giving up.
    assert_eq!(s.turns.len(), 2);
}

// ---- outcome labelling -----------------------------------------------------

#[test]
fn outcome_labels_follow_the_forge_and_the_archive_flag() {
    assert_eq!(Outcome::derive(Some(PrState::Merged), true), Outcome::Merged);
    assert_eq!(Outcome::derive(Some(PrState::Closed), true), Outcome::Abandoned);
    assert_eq!(Outcome::derive(Some(PrState::Open), false), Outcome::Open);
    assert_eq!(Outcome::derive(Some(PrState::Draft), false), Outcome::Open);
    // Archived with no PR: the user threw it away.
    assert_eq!(Outcome::derive(None, true), Outcome::Abandoned);
}

#[test]
fn an_unknown_outcome_is_never_guessed_into_a_label() {
    // Live worktree, no PR yet — genuinely undecided. Mislabeling it as
    // abandoned would teach the model that unfinished work is failed work.
    let o = Outcome::derive(None, false);
    assert_eq!(o, Outcome::Unknown);
    assert!(!o.is_labeled());
    assert!(!Outcome::Open.is_labeled());
    assert!(Outcome::Merged.is_labeled());
    assert!(Outcome::Abandoned.is_labeled());
}

// ---- harvesting ------------------------------------------------------------

fn diff() -> Diff {
    Diff {
        base: "abc123".into(),
        files: vec![
            DiffFile {
                path: "src/nav.rs".into(),
                change: Change::Modified,
                adds: 30,
                dels: 8,
                untracked: false,
                renamed_from: None,
            },
            DiffFile {
                path: "src/new.rs".into(),
                change: Change::Added,
                adds: 8,
                dels: 0,
                untracked: false,
                renamed_from: None,
            },
        ],
        untracked_elided: 0,
    }
}

/// The harvested summary: the diff plus the full unified text, which the
/// caller now supplies (see `DiffSummary::new`).
fn diff_summary() -> training::DiffSummary {
    training::DiffSummary::new(&diff(), "--- a/src/nav.rs\n+++ b/src/nav.rs\n".into())
}

#[test]
fn harvests_a_full_record_from_registry_forge_and_git() {
    let e = entry();
    let record = harvest(&HarvestRequest {
        entry: &e,
        pr_state: Some(PrState::Merged),
        diff: Some(diff_summary()),
    })
    .unwrap();

    assert_eq!(record.outcome, Outcome::Merged);
    assert_eq!(record.schema_version, "1.0");
    assert_eq!(record.session_format_version, 3);

    assert_eq!(record.provenance.worktree_id, "app/nav");
    assert_eq!(record.provenance.branch, "feat/nav");
    assert_eq!(record.provenance.pr_number, Some(128));
    assert_eq!(record.provenance.agent_kind, "pi");
    assert_eq!(record.provenance.model.as_deref(), Some("test-model"));

    let diff_summary = record.diff.unwrap();
    assert_eq!(diff_summary.files_changed, 2);
    assert_eq!((diff_summary.adds, diff_summary.dels), (38, 8));
    assert_eq!(diff_summary.paths, vec!["src/nav.rs", "src/new.rs"]);

    assert_eq!(record.stats.turn_count, 4);
    assert_eq!(record.stats.tool_call_count, 1);
    assert!(!record.stats.had_tool_errors);
}

#[test]
fn harvest_works_without_a_diff() {
    // Archiving removes the worktree directory, so an archived task usually has
    // no diff left to compute — which is exactly why archive keeps the branch.
    let e = entry();
    let record = harvest(&HarvestRequest {
        entry: &e,
        pr_state: Some(PrState::Merged),
        diff: None,
    })
    .unwrap();
    assert!(record.diff.is_none());
    assert_eq!(record.outcome, Outcome::Merged);
}

#[test]
fn a_worktree_with_no_session_file_is_reported_not_skipped() {
    let mut e = entry();
    e.session_file = None;
    let err = harvest(&HarvestRequest { entry: &e, pr_state: None, diff: None }).unwrap_err();
    assert!(err.to_string().contains("no recorded session file"));
}

#[test]
fn only_decided_outcomes_survive_the_training_filter() {
    let e = entry();
    let session = read_session(&fixture("pi-session-v3.jsonl")).unwrap();
    let mk = |pr: Option<PrState>, archived: bool| {
        let mut e2 = e.clone();
        e2.archived = archived;
        build_record(
            &HarvestRequest { entry: &e2, pr_state: pr, diff: None },
            session.clone(),
        )
    };
    let all = vec![
        mk(Some(PrState::Merged), true),
        mk(Some(PrState::Closed), true),
        mk(Some(PrState::Open), false),
        mk(None, false),
    ];
    let kept = labeled(all);
    assert_eq!(kept.len(), 2);
    assert!(kept.iter().all(|r| r.outcome.is_labeled()));
}

#[test]
fn records_round_trip_through_jsonl() {
    let e = entry();
    let record = harvest(&HarvestRequest {
        entry: &e,
        pr_state: Some(PrState::Merged),
        diff: Some(diff_summary()),
    })
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("nested/train.jsonl");
    training::write_jsonl(std::slice::from_ref(&record), &out).unwrap();

    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 1, "one compact object per line");
    let back: training::TrainingRecord = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(back, record);

    // Atomic write leaves no residue.
    let strays: Vec<_> = std::fs::read_dir(out.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "train.jsonl")
        .collect();
    assert!(strays.is_empty(), "left {strays:?} behind");
}
