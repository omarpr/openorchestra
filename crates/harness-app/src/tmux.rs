//! tmux integration: pure builders for the client invocation and session
//! classification (this file), plus thin process wrappers (see `runtime`
//! submodule / Task 2). Kept pure so the argv, quoting, and reattach logic are
//! unit-testable without a tmux server or a GUI.

use std::path::Path;

use agent_runtime::AgentCommand;

/// The `harness_<slug>_` stem every session for a given key shares. The
/// resource monitor keys off this to attribute a live session back to its
/// worktree, so the slugging lives here and `session_name` builds on it — the
/// two can never drift.
pub fn session_prefix(worktree_key: &str) -> String {
    let mut slug = String::with_capacity(worktree_key.len());
    for ch in worktree_key.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            slug.push(ch);
        } else {
            slug.push('-');
        }
    }
    format!("harness_{slug}_")
}

/// Deterministic tmux session name for a tab. Prefixed so it never collides
/// with the user's own sessions; sanitized to tmux-legal characters.
pub fn session_name(worktree_key: &str, tab_id: u64) -> String {
    format!("{}{tab_id}", session_prefix(worktree_key))
}

/// Single-quote a string for POSIX `sh`, escaping embedded single quotes.
pub fn shell_quote(s: &str) -> String {
    // Wrap in single quotes; a literal ' becomes '\'' (close, escaped ', reopen).
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The script passed to `sh -lc`: inline env, the quoted agent argv, then drop
/// to a login shell so quitting the agent leaves a usable shell in the same
/// (still-persistent) session.
pub fn agent_script(cmd: &AgentCommand) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in &cmd.env {
        parts.push(format!("{k}={}", shell_quote(v)));
    }
    parts.push(shell_quote(&cmd.program.to_string_lossy()));
    for arg in &cmd.args {
        parts.push(shell_quote(arg));
    }
    // Run the agent, then drop to a login shell so the session survives the
    // agent exiting and stays reattachable.
    format!("{}; exec \"$SHELL\" -l", parts.join(" "))
}

/// Args after `tmux` for the client the pane runs. tmux splits its argv on a
/// bare `;` argument, so each invocation is a small command sequence.
///
/// Every path disables the status bar and enables mouse reporting **for that
/// session only** (`set-option -t NAME status off` / `mouse on`) so
/// openOrchestra's embedded terminals stay clean and scrollable without ever
/// touching the user's global tmux config or their other sessions — both
/// options are set before the client attaches. Mouse mode is what makes the
/// scroll wheel work: tmux runs on the alternate screen, so without it the
/// wheel is inert (there is no host scrollback to fall back on).
///
/// - `attach_only` (restore of a live session): `set-option … ; attach-session`.
///   No `new-session`: on a session that already exists `-A` attaches
///   immediately and would re-attach again after the user detaches, trapping the
///   pane. The set-option here also migrates sessions made before the bar was
///   hidden.
/// - otherwise (fresh tab, newly-minted unique name): `new-session -A -d …`
///   creates it *detached* (the name can't already exist, so `-A` never attaches
///   early), then `set-option`, then `attach-session` as the pane's foreground
///   client. `-c CWD`, one `-e K=V` per `env` entry, and the optional
///   `sh -lc SCRIPT` agent command ride on the new-session.
///
/// `env` becomes the session's inherited environment, so the pane's shell has
/// `HARNESS_*` from its first prompt without anything being typed into it. It
/// is ignored on the `attach_only` path: the session already exists with the
/// environment it was created with, and `set-environment` would only reach
/// panes created later, not the shell already running.
pub fn client_args(
    session: &str,
    cwd: &Path,
    command: Option<&AgentCommand>,
    attach_only: bool,
    env: &[(String, String)],
) -> Vec<String> {
    if attach_only {
        return vec![
            "set-option".into(), "-t".into(), session.into(), "status".into(), "off".into(),
            ";".into(),
            "set-option".into(), "-t".into(), session.into(), "mouse".into(), "on".into(),
            ";".into(),
            "attach-session".into(), "-t".into(), session.into(),
        ];
    }
    let mut args = vec![
        "new-session".into(),
        "-A".into(),
        "-d".into(),
        "-s".into(),
        session.into(),
        "-c".into(),
        cwd.to_string_lossy().into_owned(),
    ];
    // Sorted so the argv is stable across runs (and testable); tmux takes one
    // -e per variable. No shell quoting here — tmux receives argv directly.
    let mut env: Vec<_> = env.iter().collect();
    env.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    if let Some(cmd) = command {
        args.push("sh".into());
        args.push("-lc".into());
        args.push(agent_script(cmd));
    }
    args.extend([
        ";".into(),
        "set-option".into(), "-t".into(), session.into(), "status".into(), "off".into(),
        ";".into(),
        "set-option".into(), "-t".into(), session.into(), "mouse".into(), "on".into(),
        ";".into(),
        "attach-session".into(), "-t".into(), session.into(),
    ]);
    args
}

/// Parse `tmux ls -F '#{session_name}'` output into session names.
pub fn parse_sessions(ls_output: &str) -> Vec<String> {
    ls_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `name` is among the live sessions.
pub fn is_live(name: &str, live: &[String]) -> bool {
    live.iter().any(|s| s == name)
}

/// Parse `tmux list-panes -a -F '#{session_name} #{pane_pid}'` into
/// `(session_name, pane_pid)` pairs. Session names never contain a space (they
/// are our slugged keys), so the pid is whatever follows the last space; a
/// malformed or empty line is skipped rather than guessed at.
pub fn parse_session_panes(output: &str) -> Vec<(String, u32)> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, pid) = line.rsplit_once(' ')?;
            let pid: u32 = pid.trim().parse().ok()?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), pid))
        })
        .collect()
}

use std::process::Command;

/// Whether a `tmux` binary is on PATH. Cached by the caller (checked once at
/// startup); cheap enough to compute directly here.
pub fn has_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Names of currently-live tmux sessions. Empty on any error (no server yet,
/// tmux missing) — callers treat "unknown" as "not live", which restores fresh.
pub fn live_sessions() -> Vec<String> {
    let out = Command::new("tmux")
        .args(["ls", "-F", "#{session_name}"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_sessions(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// `(session_name, pane_pid)` for every pane the tmux server holds. Empty on
/// any error (no server, tmux missing) — the resource band then attributes
/// nothing to worktrees and shows the base-app total only.
pub fn list_session_panes() -> Vec<(String, u32)> {
    let out = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{session_name} #{pane_pid}"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            parse_session_panes(&String::from_utf8_lossy(&o.stdout))
        }
        _ => Vec::new(),
    }
}

/// Tear down a session (used on explicit tab close). Best-effort.
pub fn kill_session(name: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", name]).output();
}

/// Send a command line to a live session and press Enter. Best-effort — used
/// to surface setup hooks in the visible shell tab.
pub fn send_keys(session: &str, command: &str) {
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", command])
        .output();
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", session, "Enter"])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cmd() -> AgentCommand {
        AgentCommand {
            program: PathBuf::from("/usr/bin/pi"),
            args: vec!["--mode".into(), "tui".into()],
            env: vec![("HARNESS_AGENT_SCHEMA".into(), "/tmp/a b/schema".into())],
            label: "pi".into(),
        }
    }

    #[test]
    fn session_name_is_prefixed_and_sanitized() {
        let n = session_name("acme/fix.bug:v2", 7);
        assert!(n.starts_with("harness_"), "got {n}");
        assert!(
            n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "illegal char in {n}"
        );
        assert!(n.ends_with("_7"), "tab id should be in the name: {n}");
    }

    #[test]
    fn session_name_is_stable_and_unique() {
        assert_eq!(session_name("k", 1), session_name("k", 1));
        assert_ne!(session_name("k", 1), session_name("k", 2));
        assert_ne!(session_name("a", 1), session_name("b", 1));
    }

    #[test]
    fn parse_session_panes_pairs_names_with_pids_and_skips_junk() {
        let out = "harness_wtA_1 4321\nharness_wtA_2 4400\nmy-session 12\n\n  \nbroken-line\nname 99x\n";
        let panes = parse_session_panes(out);
        assert_eq!(
            panes,
            vec![
                ("harness_wtA_1".to_string(), 4321),
                ("harness_wtA_2".to_string(), 4400),
                ("my-session".to_string(), 12),
            ],
            "well-formed rows parse; blank/no-pid/non-numeric rows drop"
        );
    }

    #[test]
    fn every_session_name_starts_with_its_key_prefix() {
        // The resource monitor attributes a live session back to a worktree by
        // this prefix, so it must be exactly what `session_name` is built from.
        let key = "acme/fix.bug:v2";
        let prefix = session_prefix(key);
        assert_eq!(prefix, "harness_acme-fix-bug-v2_");
        for tab in [1u64, 7, 999] {
            assert!(
                session_name(key, tab).starts_with(&prefix),
                "session name for tab {tab} must carry the key prefix"
            );
        }
        // A different key must not share the prefix, or two worktrees' usage
        // would merge.
        assert!(!session_name("other", 1).starts_with(&prefix));
    }

    #[test]
    fn shell_quote_escapes_single_quotes_and_spaces() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn agent_script_inlines_env_quotes_argv_and_execs_shell() {
        let s = agent_script(&cmd());
        assert!(s.contains("HARNESS_AGENT_SCHEMA='/tmp/a b/schema'"), "{s}");
        assert!(s.contains("'/usr/bin/pi' '--mode' 'tui'"), "{s}");
        assert!(s.trim_end().ends_with(r#"exec "$SHELL" -l"#), "{s}");
    }

    // The status bar is hidden per-session before the client attaches. Helper:
    // the sub-slice `set-option -t NAME status off` must appear, and the session
    // must never be told `status on` (a change that would outlive the pane).
    fn hides_status_bar(a: &[String], session: &str) -> bool {
        a.windows(5).any(|w| w == ["set-option", "-t", session, "status", "off"])
            && !a.windows(5).any(|w| w == ["set-option", "-t", session, "status", "on"])
    }

    // Scroll works in a tmux pane only when the session requests mouse
    // reporting; the client sets it per-session, scoped like `status off`.
    fn enables_mouse(a: &[String], session: &str) -> bool {
        a.windows(5).any(|w| w == ["set-option", "-t", session, "mouse", "on"])
    }

    #[test]
    fn client_args_fresh_agent() {
        let c = cmd();
        let a = client_args("harness_k_7", Path::new("/repo"), Some(&c), false, &[]);
        // Fresh tab creates the session detached so `set-option` can run before
        // the attach — `-A -d` on a brand-new name won't attach early.
        assert_eq!(a[0], "new-session");
        assert_eq!(a[1], "-A");
        assert_eq!(a[2], "-d");
        assert!(a.iter().any(|x| x == "-c"), "cwd flag missing: {a:?}");
        assert!(a.iter().any(|x| x == "/repo"));
        assert!(a.iter().any(|x| x == "harness_k_7"));
        // The agent script rides on the new-session command, before the first `;`.
        let first_sep = a.iter().position(|x| x == ";").expect("command separator");
        assert_eq!(a[first_sep - 3], "sh");
        assert_eq!(a[first_sep - 2], "-lc");
        assert!(a[first_sep - 1].contains("/usr/bin/pi"));
        // Ends by attaching as the pane's foreground client.
        assert_eq!(a[a.len() - 3], "attach-session");
        assert_eq!(a[a.len() - 1], "harness_k_7");
        assert!(hides_status_bar(&a, "harness_k_7"), "status bar not hidden: {a:?}");
    }

    #[test]
    fn client_args_fresh_shell_has_no_command() {
        let a = client_args("harness_k_9", Path::new("/repo"), None, false, &[]);
        assert!(a.iter().any(|x| x == "-c"));
        assert!(!a.iter().any(|x| x == "sh"), "shell tab must have no sh -lc: {a:?}");
        assert!(hides_status_bar(&a, "harness_k_9"), "status bar not hidden: {a:?}");
    }

    #[test]
    fn client_args_passes_env_to_new_session_so_the_shell_inherits_it() {
        let env = vec![
            ("HARNESS_WORKTREE".to_string(), "/wt/a b".to_string()),
            ("HARNESS_BRANCH".to_string(), "feat/x".to_string()),
        ];
        let a = client_args("harness_k_4", Path::new("/repo"), None, false, &env);
        // One -e per var, sorted, unquoted — tmux gets argv, not a shell line.
        let es: Vec<&String> = a
            .iter()
            .enumerate()
            .filter(|(i, _)| i > &0 && a[i - 1] == "-e")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(es, vec!["HARNESS_BRANCH=feat/x", "HARNESS_WORKTREE=/wt/a b"]);
        // On new-session, before the command sequence separator.
        let first_sep = a.iter().position(|x| x == ";").expect("separator");
        assert!(a[..first_sep].iter().any(|x| x == "-e"), "{a:?}");
    }

    #[test]
    fn client_args_attach_only_ignores_env() {
        // A live session already has the environment it was created with, and
        // set-environment would not reach the shell already running in it.
        let env = vec![("HARNESS_BRANCH".to_string(), "feat/x".to_string())];
        let a = client_args("harness_k_5", Path::new("/repo"), None, true, &env);
        assert!(!a.iter().any(|x| x == "-e"), "{a:?}");
    }

    #[test]
    fn client_args_attach_only_sets_status_then_attaches() {
        let a = client_args("harness_k_7", Path::new("/repo"), None, true, &[]);
        assert_eq!(
            a,
            vec![
                "set-option", "-t", "harness_k_7", "status", "off",
                ";",
                "set-option", "-t", "harness_k_7", "mouse", "on",
                ";",
                "attach-session", "-t", "harness_k_7",
            ]
        );
        // No new-session on restore: it would attach early and trap the pane.
        assert!(!a.iter().any(|x| x == "new-session"), "restore must not re-create: {a:?}");
    }

    #[test]
    fn client_args_enable_mouse_so_the_wheel_scrolls() {
        // Every path must turn on mouse reporting for its session — without it
        // tmux (always on the alternate screen) leaves the scroll wheel inert.
        let c = cmd();
        let fresh_agent = client_args("harness_k_1", Path::new("/repo"), Some(&c), false, &[]);
        let fresh_shell = client_args("harness_k_2", Path::new("/repo"), None, false, &[]);
        let restored = client_args("harness_k_3", Path::new("/repo"), None, true, &[]);
        assert!(enables_mouse(&fresh_agent, "harness_k_1"), "{fresh_agent:?}");
        assert!(enables_mouse(&fresh_shell, "harness_k_2"), "{fresh_shell:?}");
        assert!(enables_mouse(&restored, "harness_k_3"), "{restored:?}");
        // Scoped to our session only — never a global `set -g`.
        assert!(!fresh_agent.iter().any(|x| x == "-g"), "must not touch global config");
    }

    #[test]
    fn parse_sessions_and_is_live() {
        let out = "harness_k_1\nharness_k_2\nother\n";
        let live = parse_sessions(out);
        assert_eq!(live, vec!["harness_k_1", "harness_k_2", "other"]);
        assert!(is_live("harness_k_2", &live));
        assert!(!is_live("harness_k_9", &live));
        assert!(parse_sessions("").is_empty());
    }

    #[test]
    fn has_tmux_matches_which() {
        let via_which = std::process::Command::new("which")
            .arg("tmux")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert_eq!(has_tmux(), via_which);
    }
}
