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

/// tmux has no native Windows build; there the app runs its bundled stand-in,
/// `oo-session`, and without it degrades (agents die with their tab) instead
/// of blocking startup (`platform::Sessions::required`).
pub use crate::platform::Fix;

const TMUX_SEVERITY: Severity = if crate::platform::CURRENT.sessions.required {
    Severity::Required
} else {
    Severity::Recommended
};

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
    /// How to get it: commands the screen can copy or run, or advice.
    pub fix: Fix,
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
pub(crate) const AGENTS: [&str; 6] = ["claude", "codex", "opencode", "kimi", "grok", "pi"];

/// Probe every tool. `lookup` returns the version string for a binary on PATH,
/// or `None` when it is absent.
///
/// The real one is [`version_on_path`]; tests pass a fake so the classification
/// can be asserted on a machine that has any mix of these installed.
#[cfg(test)]
pub fn probe(lookup: impl Fn(&str, &str) -> Option<String>) -> Vec<Check> {
    probe_all(lookup).checks
}

/// One agent CLI on the screen's Agents tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCheck {
    pub program: &'static str,
    pub label: &'static str,
    /// Version string when installed.
    pub found: Option<String>,
    /// How to install it here.
    pub fix: Fix,
    /// Installed, but its version check fails: why, and what fixes that
    /// instead of installing it again. Filled by [`diagnose`].
    pub broken: Option<Broken>,
}

/// An agent CLI that is on PATH but fails to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    /// The line of its output that says why.
    pub error: String,
    pub fix: Fix,
}

/// The oldest Node.js the npm-installed agents start on (pi needs 20).
pub const MIN_NODE: u32 = 20;

/// What the requirements screen shows: the requirement rows, and every agent
/// CLI for the Agents tab.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub checks: Vec<Check>,
    pub agents: Vec<AgentCheck>,
}

/// [`probe`], plus each agent CLI on its own.
pub fn probe_all(lookup: impl Fn(&str, &str) -> Option<String>) -> Probe {
    let agents: Vec<AgentCheck> = crate::platform::CURRENT
        .agents
        .iter()
        .map(|a| AgentCheck {
            program: a.program,
            label: a.label,
            found: lookup(a.program, "--version"),
            fix: a.fix,
            broken: None,
        })
        .collect();
    // Any one agent is enough — they are peers, and the harness is useful with
    // whichever one you actually use. A missing row per CLI would imply you
    // need all of them; the Agents tab lists them all instead.
    let agent_found = agents
        .iter()
        .find_map(|a| a.found.as_ref().map(|v| format!("{} {v}", a.program)));

    // `rustup component add` is no help without rustup: then the fix installs
    // rustup first. Asked only when rust-analyzer is missing.
    let analyzer = lookup("rust-analyzer", "--version");
    let analyzer_fix = if analyzer.is_none() && lookup("rustup", "--version").is_none() {
        crate::platform::CURRENT.install.rustup_then_analyzer
    } else {
        Fix::Run(&[crate::platform::ADD_ANALYZER])
    };

    let checks = vec![
        Check {
            name: "git",
            severity: Severity::Required,
            found: lookup("git", "--version"),
            cost: "worktrees, diffs and branch status — the app has nothing to show without it",
            fix: crate::platform::CURRENT.install.git,
            version_flag: "--version",
        },
        Check {
            // What keeps sessions alive here: "tmux", or "oo-session" on Windows.
            name: crate::platform::CURRENT.sessions.host,
            severity: TMUX_SEVERITY,
            found: lookup("tmux", "-V"),
            cost: "agents run in sessions that outlive their tab; without it an agent dies when you close a tab, \
                   and nothing reattaches after a restart",
            fix: crate::platform::CURRENT.install.sessions,
            version_flag: "-V",
        },
        Check {
            name: "an agent CLI",
            severity: Severity::Recommended,
            found: agent_found,
            cost: "agent tabs fall back to a plain shell — Claude Code, Codex, OpenCode, Kimi \
                   or Grok, any one of them is enough",
            fix: Fix::Advice("install any one of them from the Agents tab"),
            version_flag: "--version",
        },
        Check {
            name: "gh",
            severity: Severity::Optional,
            found: lookup("gh", "--version"),
            cost: "pull-request and CI badges stay hidden on worktree cards",
            fix: crate::platform::CURRENT.install.gh,
            version_flag: "--version",
        },
        Check {
            name: "rust-analyzer",
            severity: Severity::Optional,
            found: analyzer,
            cost: "the editor's LSP indicator reports no language server",
            fix: analyzer_fix,
            version_flag: "--version",
        },
    ];
    Probe { checks, agents }
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
    // "tmux" is whatever keeps sessions alive here: tmux itself, or on
    // Windows the bundled oo-session beside the app rather than on PATH.
    let resolved = if bin == "tmux" {
        crate::tmux::program()
    } else {
        bin.to_string()
    };
    let out = run_version(resolved.as_str(), flag)?;
    if !out.status.success() {
        return None;
    }
    Some(version_from(&out.stdout, &out.stderr))
}

/// The version a `--version` run printed. Most tools print it on stdout; pi
/// prints it on stderr, which left its column blank.
fn version_from(stdout: &[u8], stderr: &[u8]) -> String {
    let first_line = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    };
    let line = first_line(stdout).or_else(|| first_line(stderr)).unwrap_or_default();
    // Keep the version, drop the program's own name: "git version 2.43" and
    // "tmux 3.4" become "2.43" and "3.4", so the column stays scannable.
    line.split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or(&line)
        .to_string()
}

/// `<bin> <flag>`'s output, or `None` when `bin` is not on PATH at all.
fn run_version(bin: &str, flag: &str) -> Option<std::process::Output> {
    spawn_version(bin, flag).ok()
}

/// `bin flag`'s output, or why it could not be started.
fn spawn_version(bin: &str, flag: &str) -> std::io::Result<std::process::Output> {
    use crate::platform::{OS, Os};
    os_compat::process::command(bin)
        .arg(flag)
        .output()
        .or_else(|e| match OS {
            // std finds `bin.exe` on PATH but not the `bin.cmd` launcher npm
            // installs agent CLIs as (`claude.cmd`), so try that too.
            Os::Windows if e.kind() == std::io::ErrorKind::NotFound => {
                os_compat::process::command(format!("{bin}.cmd")).arg(flag).output()
            }
            Os::Windows | Os::Mac | Os::Linux => Err(e),
        })
}

/// Tell "not installed" from "installed but failing" for each agent the
/// probe did not find. A CLI whose `--version` fails is on PATH all the same,
/// and installing it again changes nothing: pi on Node 16 failed this way.
pub fn diagnose(agents: &mut [AgentCheck]) {
    if agents.iter().all(|a| a.found.is_some()) {
        return;
    }
    diagnose_with(agents, failure_on_path, node_major());
}

fn diagnose_with(
    agents: &mut [AgentCheck],
    failure: impl Fn(&str) -> Option<String>,
    node: Option<u32>,
) {
    for a in agents.iter_mut().filter(|a| a.found.is_none()) {
        let Some(error) = failure(a.program) else {
            continue;
        };
        let old_node = node.filter(|&major| major < MIN_NODE);
        a.broken = Some(match old_node {
            Some(major) if installs_with_npm(a.fix) => Broken {
                error: format!("needs Node.js {MIN_NODE} or newer, this is Node {major}: {error}"),
                fix: crate::platform::CURRENT.install.node,
            },
            _ => Broken { error, fix: a.fix },
        });
    }
}

fn installs_with_npm(fix: Fix) -> bool {
    matches!(fix, Fix::Run(commands) if commands.iter().any(|c| c.starts_with("npm ")))
}

/// Why `bin --version` failed, when `bin` is on PATH and did fail.
fn failure_on_path(bin: &str) -> Option<String> {
    failure_of(bin, spawn_version(bin, "--version"))
}

/// What a `bin --version` run says about a `bin` the probe didn't find:
/// nothing when it isn't there at all (not found) or ran fine, else why it
/// won't start. Only "not found" means not installed: a CLI the system
/// refuses to start (no permission, a bad binary) is installed, and
/// reinstalling it would not help.
fn failure_of(bin: &str, run: std::io::Result<std::process::Output>) -> Option<String> {
    let out = match run {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("can't start `{bin}`: {e}")),
    };
    if out.status.success() {
        return None;
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    Some(error_line(&text).unwrap_or_else(|| format!("`{bin} --version` failed ({})", out.status)))
}

/// The line that names the error ("SyntaxError: …"), else the first one.
fn error_line(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let named = lines.iter().find(|l| {
        l.split_once(':')
            .is_some_and(|(kind, _)| kind.ends_with("Error") && !kind.contains(' '))
    });
    named.or(lines.first()).map(|l| l.to_string())
}

/// The major version of the `node` on PATH.
fn node_major() -> Option<u32> {
    let v = version_on_path("node", "--version")?;
    v.trim_start_matches('v').split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(present: &[&str]) -> Vec<Check> {
        let present: Vec<String> = present.iter().map(|s| s.to_string()).collect();
        probe(move |bin, _flag| present.iter().any(|p| p == bin).then(|| "1.0".to_string()))
    }

    #[test]
    fn only_a_missing_cli_counts_as_not_installed() {
        use std::io::{Error, ErrorKind};
        assert_eq!(failure_of("pi", Err(Error::from(ErrorKind::NotFound))), None);
        let refused = failure_of("pi", Err(Error::from(ErrorKind::PermissionDenied)));
        assert!(
            refused.as_deref().is_some_and(|e| e.starts_with("can't start `pi`")),
            "{refused:?}"
        );
    }

    #[test]
    fn tmux_is_probed_with_dash_v_because_it_has_no_long_flag() {
        // Regression: probing every tool with `--version` made tmux — which
        // exits non-zero on it — look absent on every machine. tmux blocks, so
        // the app refused to start for everyone who had it installed.
        let checks = with(&[]);
        let tmux = checks.iter().find(|c| c.name == crate::platform::CURRENT.sessions.host).unwrap();
        assert_eq!(tmux.version_flag, "-V");
        // Everything else does answer the long flag.
        for c in checks.iter().filter(|c| c.name != crate::platform::CURRENT.sessions.host) {
            assert_eq!(
                c.version_flag, "--version",
                "{} should use --version",
                c.name
            );
        }
    }

    #[test]
    fn git_or_tmux_missing_blocks_and_nothing_else_does() {
        assert!(
            blocked(&with(&["tmux", "pi", "gh", "rust-analyzer"])),
            "no git must block"
        );
        // On Windows a missing oo-session degrades instead
        // (`platform::Sessions::required`).
        assert_eq!(
            blocked(&with(&["git", "pi", "gh", "rust-analyzer"])),
            crate::platform::CURRENT.sessions.required,
            "no tmux must block where sessions are required"
        );
        // Everything required present: the rest only degrades features.
        assert!(!blocked(&with(&["git", "tmux"])));
        assert!(
            any_missing(&with(&["git", "tmux"])),
            "but they are still reported"
        );
    }

    #[test]
    fn every_agent_the_probe_accepts_is_installable_on_every_os() {
        // Copy and probe drift apart silently: the Agents tab could list a CLI
        // the check would not count, or miss one it would.
        for os in crate::platform::Os::ALL {
            let programs: Vec<&str> = crate::platform::Profile::of(os)
                .agents
                .iter()
                .map(|a| a.program)
                .collect();
            assert_eq!(programs, AGENTS, "{os:?}");
            for a in crate::platform::Profile::of(os).agents {
                assert!(
                    matches!(a.fix, Fix::Run(cmds) if !cmds.is_empty()),
                    "{os:?} {}: nothing to run",
                    a.program
                );
            }
        }
        let row = with(&[]);
        let agent = row.iter().find(|c| c.name == "an agent CLI").unwrap();
        assert!(fix_text(agent).contains("Agents tab"), "{:?}", agent.fix);
        // pi is supported, not required — the screen must not read as if it were.
        assert!(
            !agent.cost.contains("pi"),
            "cost still advertises pi: {:?}",
            agent.cost
        );
    }

    #[test]
    fn the_agents_tab_reports_each_agent_on_its_own() {
        let p = probe_all(|bin, _| (bin == "codex").then(|| "0.171.0".to_string()));
        let found: Vec<(&str, bool)> = p.agents.iter().map(|a| (a.program, a.found.is_some())).collect();
        assert_eq!(
            found,
            AGENTS.iter().map(|&a| (a, a == "codex")).collect::<Vec<_>>()
        );
        let row = p.checks.iter().find(|c| c.name == "an agent CLI").unwrap();
        assert_eq!(row.found.as_deref(), Some("codex 0.171.0"));
    }

    #[test]
    fn an_agent_that_fails_to_start_on_old_node_asks_for_newer_node() {
        let mut agents = probe_all(|bin, _| (bin == "codex").then(|| "0.171.0".into())).agents;
        // pi is on PATH but crashes; grok is simply absent.
        let failure = |bin: &str| (bin == "pi").then(|| "SyntaxError: Invalid regular expression flags".into());
        diagnose_with(&mut agents, failure, Some(16));
        let pi = agents.iter().find(|a| a.program == "pi").unwrap();
        let broken = pi.broken.as_ref().expect("pi is installed but broken");
        assert_eq!(broken.fix, crate::platform::CURRENT.install.node);
        assert!(broken.error.contains("Node.js 20") && broken.error.contains("Node 16"), "{}", broken.error);
        assert!(broken.error.ends_with("Invalid regular expression flags"), "{}", broken.error);
        for a in agents.iter().filter(|a| a.program != "pi") {
            assert_eq!(a.broken, None, "{} is found or absent, not broken", a.program);
        }
    }

    #[test]
    fn an_agent_that_fails_on_new_enough_node_keeps_its_own_fix() {
        let mut agents = probe_all(|_, _| None).agents;
        diagnose_with(&mut agents, |bin| (bin == "pi").then(|| "boom".into()), Some(22));
        let pi = agents.iter().find(|a| a.program == "pi").unwrap();
        assert_eq!(pi.broken, Some(Broken { error: "boom".into(), fix: pi.fix }));
    }

    #[test]
    fn a_version_printed_on_stderr_still_shows() {
        assert_eq!(version_from(b"git version 2.43.0\n", b""), "2.43.0");
        // pi writes its version to stderr only.
        assert_eq!(version_from(b"", b"0.73.1\n"), "0.73.1");
        assert_eq!(version_from(b"\n", b"tmux 3.4\n"), "3.4");
        assert_eq!(version_from(b"", b""), "");
    }

    #[test]
    fn the_error_line_is_the_one_that_names_the_error() {
        let node = "file:///C:/x/pi.js:3\nconst r = /a/v;\n          ^\n\nSyntaxError: Invalid regular expression flags\n    at foo (node:internal)\n";
        assert_eq!(
            error_line(node).as_deref(),
            Some("SyntaxError: Invalid regular expression flags")
        );
        assert_eq!(error_line("\n  usage: x [opts]\n").as_deref(), Some("usage: x [opts]"));
        assert_eq!(error_line("\n\n"), None);
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
        assert!(
            none.iter()
                .find(|c| c.name == "an agent CLI")
                .unwrap()
                .missing()
        );
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
            assert!(!fix_text(&c).is_empty(), "{} has no fix", c.name);
        }
    }

    #[test]
    fn rust_analyzer_installs_rustup_first_only_when_it_is_missing() {
        let fix_of = |present: &[&str]| {
            with(present)
                .into_iter()
                .find(|c| c.name == "rust-analyzer")
                .unwrap()
                .fix
        };
        assert_eq!(
            fix_of(&["rustup"]),
            Fix::Run(&[crate::platform::ADD_ANALYZER])
        );
        assert_eq!(
            fix_of(&[]),
            crate::platform::CURRENT.install.rustup_then_analyzer
        );
        for os in crate::platform::Os::ALL {
            let Fix::Run(cmds) = crate::platform::Profile::of(os).install.rustup_then_analyzer
            else {
                panic!("{os:?}: rustup should be installable from the screen");
            };
            assert_eq!(cmds.last(), Some(&crate::platform::ADD_ANALYZER), "{os:?}");
        }
    }

    fn fix_text(c: &Check) -> String {
        match c.fix {
            Fix::Run(cmds) => cmds.join("\n"),
            Fix::Advice(text) => text.to_string(),
        }
    }

    #[test]
    fn every_runnable_fix_is_one_command_as_typed() {
        // Run types each command into a shell and presses Enter, and Copy puts
        // it on the clipboard: a note riding along ("(then: …)") or a line
        // break would run as part of the command.
        let mut fixes: Vec<(String, Fix)> = with(&[])
            .into_iter()
            .map(|c| (c.name.to_string(), c.fix))
            .collect();
        for os in crate::platform::Os::ALL {
            let i = &crate::platform::Profile::of(os).install;
            for (name, fix) in [
                ("git", i.git),
                ("sessions", i.sessions),
                ("gh", i.gh),
                ("rustup", i.rustup_then_analyzer),
                ("node", i.node),
            ] {
                fixes.push((format!("{os:?} {name}"), fix));
            }
            for a in crate::platform::Profile::of(os).agents {
                fixes.push((format!("{os:?} {}", a.program), a.fix));
            }
        }
        for (name, fix) in fixes {
            if let Fix::Run(cmds) = fix {
                assert!(!cmds.is_empty(), "{name}: nothing to run");
                for cmd in cmds {
                    assert!(
                        !cmd.is_empty()
                            && cmd.trim() == *cmd
                            && !cmd.contains(['\n', '(', ')'])
                            && !cmd.contains("  "),
                        "{name}: {cmd:?} is not a single command"
                    );
                }
            }
        }
    }
}
