//! `harvest` — turn the worktree registry's session exhaust into training data.
//!
//! Walks `~/.openorchestra/registry.json`, reads each worktree's session JSONL,
//! joins a merged/abandoned outcome label from `gh`, and writes schema-versioned
//! records as JSONL (architecture §6.4, roadmap M7).
//!
//! ```text
//! harvest [--state-dir DIR] [--out FILE] [--all] [--include-unlabeled] [--no-forge]
//! ```
//!
//! Defaults harvest only **archived** worktrees with a **decided** outcome —
//! archiving is the "task is over" signal (§6.1 D6), and an undecided outcome is
//! not supervision. `--all` and `--include-unlabeled` relax each of those.

use std::path::PathBuf;
use std::process::ExitCode;

use forge::{Forge, GhCli};
use training::{harvest, HarvestRequest};
use worktrees::Registry;

struct Args {
    state_dir: PathBuf,
    out: PathBuf,
    all: bool,
    include_unlabeled: bool,
    use_forge: bool,
}

fn default_state_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".openorchestra")
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        state_dir: default_state_dir(),
        out: PathBuf::from("training-data.jsonl"),
        all: false,
        include_unlabeled: false,
        use_forge: true,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--state-dir" => args.state_dir = argv.next().ok_or("--state-dir needs a path")?.into(),
            "--out" => args.out = argv.next().ok_or("--out needs a path")?.into(),
            "--all" => args.all = true,
            "--include-unlabeled" => args.include_unlabeled = true,
            "--no-forge" => args.use_forge = false,
            "-h" | "--help" => return Err("help".into()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(args)
}

const USAGE: &str = "\
harvest — session exhaust -> schema-versioned training records (architecture §6.4)

USAGE:
    harvest [OPTIONS]

OPTIONS:
    --state-dir DIR        harness state root (default: ~/.openorchestra)
    --out FILE             output JSONL (default: ./training-data.jsonl)
    --all                  include active worktrees, not just archived ones
    --include-unlabeled    keep Open/Unknown outcomes (provenance, not supervision)
    --no-forge             skip gh; every outcome falls back to the archive flag
    -h, --help             show this help
";

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            if msg == "help" {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            eprintln!("error: {msg}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let registry_path = args.state_dir.join("registry.json");
    let registry = match Registry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: could not read {}: {e}", registry_path.display());
            return ExitCode::FAILURE;
        }
    };

    let gh = args.use_forge.then(GhCli::new);
    if args.use_forge && !gh.as_ref().is_some_and(|g| g.available()) {
        // Not fatal: outcomes degrade to the archive flag rather than being
        // guessed, so the run is still useful — just less well labeled.
        eprintln!("warning: gh unavailable — outcomes fall back to the archived flag");
    }

    let candidates: Vec<_> = registry
        .entries()
        .iter()
        .filter(|e| args.all || e.archived)
        .collect();

    let mut records = Vec::new();
    let (mut skipped_no_session, mut failed) = (0usize, 0usize);

    for entry in &candidates {
        if entry.session_file.is_none() {
            skipped_no_session += 1;
            continue;
        }
        // A gh failure must not be read as "no PR" — that would relabel a
        // merged task as abandoned. Treat it as no information.
        let pr_state = gh
            .as_ref()
            .filter(|g| g.available())
            .and_then(|g| g.pr_for_branch(&entry.repo_root, &entry.branch).ok())
            .flatten()
            .map(|pr| pr.state);

        match harvest(&HarvestRequest {
            entry,
            pr_state,
            // Archived worktrees have no directory left to diff (§6.1 D6).
            diff: None,
        }) {
            Ok(record) => records.push(record),
            Err(e) => {
                eprintln!("warning: {} — {e}", entry.id);
                failed += 1;
            }
        }
    }

    let harvested = records.len();
    if !args.include_unlabeled {
        records = training::labeled(records);
    }

    if let Err(e) = training::write_jsonl(&records, &args.out) {
        eprintln!("error: writing {}: {e}", args.out.display());
        return ExitCode::FAILURE;
    }

    let count = |o: training::Outcome| records.iter().filter(|r| r.outcome == o).count();
    let mut breakdown = vec![
        format!("{} merged", count(training::Outcome::Merged)),
        format!("{} abandoned", count(training::Outcome::Abandoned)),
    ];
    // Only meaningful under --include-unlabeled; omitted otherwise so the
    // common case reads cleanly.
    for (label, outcome) in [
        ("open", training::Outcome::Open),
        ("unknown", training::Outcome::Unknown),
    ] {
        let n = count(outcome);
        if n > 0 {
            breakdown.push(format!("{n} {label}"));
        }
    }
    println!(
        "{} candidate worktrees -> {harvested} sessions read -> {} records written ({})",
        candidates.len(),
        records.len(),
        breakdown.join(", "),
    );
    if skipped_no_session > 0 {
        println!("  {skipped_no_session} skipped: no recorded session file");
    }
    if failed > 0 {
        println!("  {failed} failed to parse (see warnings above)");
    }
    if !args.include_unlabeled && harvested > records.len() {
        println!(
            "  {} dropped as unlabeled (still open, or no outcome evidence) — --include-unlabeled keeps them",
            harvested - records.len()
        );
    }
    println!("wrote {}", args.out.display());
    ExitCode::SUCCESS
}
