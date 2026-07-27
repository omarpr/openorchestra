//! What the app needs on the machine, and what it costs when a piece is absent.
//!
//! `script/doctor.sh` answered this for a terminal, but the app never answered
//! it for itself: a missing tool showed up as an empty screen, a terminal that
//! was "just a shell", or badges that never appeared — each of which reads as a
//! broken app rather than a missing dependency. Notably `doctor.sh` never
//! checked **tmux** at all, while the app calls `tmux -V` at startup and hangs
//! every persistent agent session off it.
//!
//! Two rules decide what a gap does, and they are deliberately different:
//!
//! - [`Severity::Required`] — git and tmux. Nothing meaningful works without
//!   git, and without tmux an agent dies with the window, which quietly breaks
//!   the one promise the app makes about running agents across worktrees. These
//!   block.
//! - Everything else degrades a feature, and says which one. A warning that
//!   does not name what you lose is a warning people learn to dismiss.
//!
//! Probing is injected rather than called directly so the classification is
//! testable without a machine that happens to have the right tools installed.

/// How much a missing tool costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The app refuses to run without it.
    Required,
    /// A headline feature silently degrades.
    Recommended,
    /// A convenience is unavailable.
    Optional,
}

/// One probed tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Binary name, as the user would type it.
    pub name: &'static str,
    pub severity: Severity,
    /// Version string when found; `None` means absent.
    pub found: Option<String>,
    /// What stops working without it. Present tense, specific.
    pub cost: &'static str,
    /// The command that fixes it.
    pub fix: &'static str,
    /// The flag this tool answers its version on.
    ///
    /// Data, not an assumption: `tmux` has no `--version` and exits non-zero on
    /// it, so probing every tool the same way reported tmux missing on every
    /// machine — and tmux blocks, so the app refused to start for everyone.
    pub version_flag: &'static str,
}

impl Check {
    pub fn missing(&self) -> bool {
        self.found.is_none()
    }
}

/// The agent CLIs this app can drive, in the order the row reports them.
///
/// `pi` is still one of them — it is what the native chat pane speaks — but it
/// no longer leads, because nothing here needs *pi* specifically and a screen
/// that names one vendor reads as a requirement rather than an example.
const AGENTS: [&str; 5] = ["claude", "codex", "opencode", "kimi", "pi"];

/// Probe every tool. `lookup` returns the version string for a binary on PATH,
/// or `None` when it is absent.
///
/// The real one is [`version_on_path`]; tests pass a fake so the classification
/// can be asserted on a machine that has any mix of these installed.
pub fn probe(lookup: impl Fn(&str, &str) -> Option<String>) -> Vec<Check> {
    // Any one agent is enough — they are peers, and the harness is useful with
    // whichever one you actually use. Reporting five separate "missing" rows
    // would imply you need all of them.
    let agent_found = AGENTS
        .iter()
        .find_map(|a| lookup(a, "--version").map(|v| format!("{a} {v}")));

    vec![
        Check {
            name: "git",
            severity: Severity::Required,
            found: lookup("git", "--version"),
            cost: "worktrees, diffs and branch status — the app has nothing to show without it",
            fix: "xcode-select --install",
            version_flag: "--version",
        },
        Check {
            name: "tmux",
            severity: Severity::Required,
            found: lookup("tmux", "-V"),
            cost: "agents run inside tmux sessions; without it an agent dies when you close a tab, \
                   and nothing reattaches after a restart",
            fix: "brew install tmux",
            version_flag: "-V",
        },
        Check {
            name: "an agent CLI",
            severity: Severity::Recommended,
            found: agent_found,
            cost: "agent tabs fall back to a plain shell — Claude Code, Codex, OpenCode or Kimi, \
                   any one of them is enough",
            fix: "npm install -g @anthropic-ai/claude-code",
            version_flag: "--version",
        },
        Check {
            name: "gh",
            severity: Severity::Optional,
            found: lookup("gh", "--version"),
            cost: "pull-request and CI badges stay hidden on worktree cards",
            fix: "brew install gh   (then: gh auth login)",
            version_flag: "--version",
        },
        Check {
            name: "rust-analyzer",
            severity: Severity::Optional,
            found: lookup("rust-analyzer", "--version"),
            cost: "the editor's LSP indicator reports no language server",
            fix: "rustup component add rust-analyzer",
            version_flag: "--version",
        },
    ]
}

/// Whether a required tool is missing, which is what makes the screen refuse to
/// be dismissed.
pub fn blocked(checks: &[Check]) -> bool {
    checks
        .iter()
        .any(|c| c.severity == Severity::Required && c.missing())
}

/// Whether anything at all is missing.
pub fn any_missing(checks: &[Check]) -> bool {
    checks.iter().any(Check::missing)
}

/// Real probe: is `bin` on PATH, and what does it say its version is?
///
/// Runs `<bin> --version` rather than only testing for the file, because a
/// rustup shim sits on PATH and exits non-zero when its component is absent —
/// "present on PATH" is the wrong question for at least one of these tools, and
/// asking the binary is the only answer that survives that case.
pub fn version_on_path(bin: &str, flag: &str) -> Option<String> {
    let out = std::process::Command::new(bin).arg(flag).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    // Keep the version, drop the program's own name: "git version 2.43" and
    // "tmux 3.4" become "2.43" and "3.4", so the column stays scannable.
    let trimmed = line
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or(line);
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(present: &[&str]) -> Vec<Check> {
        let present: Vec<String> = present.iter().map(|s| s.to_string()).collect();
        probe(move |bin, _flag| present.iter().any(|p| p == bin).then(|| "1.0".to_string()))
    }

    #[test]
    fn tmux_is_probed_with_dash_v_because_it_has_no_long_flag() {
        // Regression: probing every tool with `--version` made tmux — which
        // exits non-zero on it — look absent on every machine. tmux blocks, so
        // the app refused to start for everyone who had it installed.
        let checks = with(&[]);
        let tmux = checks.iter().find(|c| c.name == "tmux").unwrap();
        assert_eq!(tmux.version_flag, "-V");
        // Everything else does answer the long flag.
        for c in checks.iter().filter(|c| c.name != "tmux") {
            assert_eq!(c.version_flag, "--version", "{} should use --version", c.name);
        }
    }

    #[test]
    fn git_or_tmux_missing_blocks_and_nothing_else_does() {
        assert!(blocked(&with(&["tmux", "pi", "gh", "rust-analyzer"])), "no git must block");
        assert!(blocked(&with(&["git", "pi", "gh", "rust-analyzer"])), "no tmux must block");
        // Everything required present: the rest only degrades features.
        assert!(!blocked(&with(&["git", "tmux"])));
        assert!(any_missing(&with(&["git", "tmux"])), "but they are still reported");
    }

    #[test]
    fn the_suggested_fix_installs_an_agent_the_probe_accepts() {
        // Copy and probe drift apart silently: a row can end up recommending a
        // CLI that the check itself would not count as an agent.
        let row = with(&[]);
        let agent = row.iter().find(|c| c.name == "an agent CLI").unwrap();
        assert!(
            AGENTS.iter().any(|a| agent.fix.contains(a)),
            "fix {:?} names no probed agent",
            agent.fix
        );
        // pi is supported, not required — the screen must not read as if it were.
        assert!(!agent.cost.contains("pi"), "cost still advertises pi: {:?}", agent.cost);
        assert!(!agent.fix.contains("pi-coding-agent"), "fix still installs pi: {:?}", agent.fix);
    }

    #[test]
    fn any_single_agent_satisfies_the_agent_check() {
        for agent in AGENTS {
            let checks = with(&["git", "tmux", agent]);
            let agent_check = checks.iter().find(|c| c.name == "an agent CLI").unwrap();
            assert!(
                !agent_check.missing(),
                "{agent} alone should satisfy the agent check"
            );
            assert!(
                agent_check.found.as_ref().unwrap().starts_with(agent),
                "the row should name the agent that was found"
            );
        }
        let none = with(&["git", "tmux"]);
        assert!(none.iter().find(|c| c.name == "an agent CLI").unwrap().missing());
    }

    #[test]
    fn a_fully_equipped_machine_reports_nothing_missing() {
        let checks = with(&["git", "tmux", "pi", "gh", "rust-analyzer"]);
        assert!(!blocked(&checks));
        assert!(!any_missing(&checks));
    }

    #[test]
    fn every_check_carries_a_cost_and_a_fix() {
        // A row that cannot say what breaks or how to fix it is a row that
        // teaches the user to dismiss the screen.
        for c in with(&[]) {
            assert!(!c.cost.is_empty(), "{} has no stated cost", c.name);
            assert!(!c.fix.is_empty(), "{} has no fix command", c.name);
        }
    }
}
